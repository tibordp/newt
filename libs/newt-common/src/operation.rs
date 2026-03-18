use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::{debug, info, warn};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::rpc::Communicator;
use crate::vfs::{Vfs, VfsDescriptor, VfsPath, VfsRegistry};

pub type OperationId = u64;
pub type IssueId = u64;

// --- Issue Resolution Types ---

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub enum IssueKind {
    AlreadyExists,
    PermissionDenied,
    IoError,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum IssueAction {
    Skip,
    Overwrite,
    Retry,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OperationIssue {
    pub issue_id: IssueId,
    pub kind: IssueKind,
    pub message: String,
    pub detail: Option<String>,
    pub actions: Vec<IssueAction>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueResponse {
    pub action: IssueAction,
    pub apply_to_all: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResolveIssueRequest {
    pub operation_id: OperationId,
    pub issue_id: IssueId,
    pub response: IssueResponse,
}

// --- Copy Options ---

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct CopyOptions {
    pub preserve_timestamps: bool,
    pub preserve_owner: bool,
    pub preserve_group: bool,
    pub create_symlink: bool,
}

// --- Operation Request ---

#[derive(Debug, Serialize, Deserialize)]
pub enum OperationRequest {
    Copy {
        sources: Vec<VfsPath>,
        destination: VfsPath,
        #[serde(default)]
        options: CopyOptions,
    },
    Move {
        sources: Vec<VfsPath>,
        destination: VfsPath,
        #[serde(default)]
        options: CopyOptions,
    },
    Delete {
        paths: Vec<VfsPath>,
    },
    SetMetadata {
        paths: Vec<VfsPath>,
        /// Bits to force ON (applied as `old_mode | mode_set`)
        mode_set: u32,
        /// Bits to force OFF (applied as `old_mode & !mode_clear`)
        mode_clear: u32,
        uid: Option<u32>,
        gid: Option<u32>,
        recursive: bool,
    },
    RunCommand {
        command: String,
        working_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StartOperationRequest {
    pub id: OperationId,
    pub request: OperationRequest,
}

// --- Progress ---

#[derive(Debug, Serialize, Deserialize)]
pub enum OperationProgress {
    /// Sent during the scanning/planning phase with running totals.
    Scanning {
        id: OperationId,
        items_found: u64,
        bytes_found: u64,
    },
    Prepared {
        id: OperationId,
        total_bytes: u64,
        total_items: u64,
    },
    Progress {
        id: OperationId,
        bytes_done: u64,
        items_done: u64,
        current_item: String,
    },
    Completed {
        id: OperationId,
    },
    Failed {
        id: OperationId,
        error: String,
    },
    Cancelled {
        id: OperationId,
    },
    Issue {
        id: OperationId,
        issue: OperationIssue,
    },
}

// --- Copy Entry ---

#[derive(Debug)]
enum CopyEntryKind {
    File,
    Directory,
    Symlink { target: PathBuf },
}

struct CopyEntry {
    source: PathBuf,
    dest: PathBuf,
    kind: CopyEntryKind,
    #[allow(dead_code)]
    size_bytes: u64,
}

struct CopyPlan {
    entries: Vec<CopyEntry>,
    total_bytes: u64,
}

// --- Per-operation issue resolver map ---

pub type IssueResolvers = Arc<Mutex<HashMap<IssueId, oneshot::Sender<IssueResponse>>>>;

// --- OperationHandle: per-operation state ---

pub struct OperationHandle {
    pub cancel: CancellationToken,
    pub issue_resolvers: IssueResolvers,
}

// --- OperationContext ---

pub struct OperationContext {
    pub registry: Arc<VfsRegistry>,
}

// --- OperationsClient trait ---

#[async_trait::async_trait]
pub trait OperationsClient: Send + Sync {
    async fn start_operation(&self, req: StartOperationRequest) -> Result<(), crate::Error>;
    async fn cancel_operation(&self, id: OperationId) -> Result<(), crate::Error>;
    async fn resolve_issue(&self, req: ResolveIssueRequest) -> Result<(), crate::Error>;
}

// --- Local implementation ---

pub struct Local {
    operations: Arc<Mutex<HashMap<OperationId, OperationHandle>>>,
    next_issue_id: Arc<AtomicU64>,
    progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>,
    context: Arc<OperationContext>,
}

impl Local {
    pub fn new(
        progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>,
        context: Arc<OperationContext>,
    ) -> Self {
        Self {
            operations: Arc::new(Mutex::new(HashMap::new())),
            next_issue_id: Arc::new(AtomicU64::new(1)),
            progress_tx,
            context,
        }
    }
}

#[async_trait::async_trait]
impl OperationsClient for Local {
    async fn start_operation(&self, req: StartOperationRequest) -> Result<(), crate::Error> {
        let handle = OperationHandle {
            cancel: CancellationToken::new(),
            issue_resolvers: Arc::new(Mutex::new(HashMap::new())),
        };
        let cancel = handle.cancel.clone();
        let issue_resolvers = handle.issue_resolvers.clone();
        self.operations.lock().insert(req.id, handle);

        let operations = self.operations.clone();
        let next_issue_id = self.next_issue_id.clone();
        let progress_tx = self.progress_tx.clone();
        let context = self.context.clone();
        let id = req.id;

        tokio::spawn(async move {
            execute_operation(
                id,
                req.request,
                progress_tx,
                cancel,
                issue_resolvers,
                next_issue_id,
                context,
            )
            .await;
            operations.lock().remove(&id);
        });

        Ok(())
    }

    async fn cancel_operation(&self, id: OperationId) -> Result<(), crate::Error> {
        if let Some(handle) = self.operations.lock().get(&id) {
            handle.cancel.cancel();
        }
        Ok(())
    }

    async fn resolve_issue(&self, req: ResolveIssueRequest) -> Result<(), crate::Error> {
        if let Some(handle) = self.operations.lock().get(&req.operation_id)
            && let Some(sender) = handle.issue_resolvers.lock().remove(&req.issue_id)
        {
            let _ = sender.send(req.response);
        }
        Ok(())
    }
}

// --- Remote implementation ---

pub struct Remote {
    communicator: Communicator,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl OperationsClient for Remote {
    async fn start_operation(&self, req: StartOperationRequest) -> Result<(), crate::Error> {
        let ret: Result<(), crate::Error> = self
            .communicator
            .invoke(crate::api::API_START_OPERATION, &req)
            .await?;
        ret
    }

    async fn cancel_operation(&self, id: OperationId) -> Result<(), crate::Error> {
        let ret: Result<(), crate::Error> = self
            .communicator
            .invoke(crate::api::API_CANCEL_OPERATION, &id)
            .await?;
        ret
    }

    async fn resolve_issue(&self, req: ResolveIssueRequest) -> Result<(), crate::Error> {
        let ret: Result<(), crate::Error> = self
            .communicator
            .invoke(crate::api::API_RESOLVE_ISSUE, &req)
            .await?;
        ret
    }
}

// --- SyncProgressSender: cloneable, movable into spawn_blocking ---

#[derive(Clone)]
struct SyncProgressSender {
    id: OperationId,
    progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>,
    last_report: Arc<Mutex<std::time::Instant>>,
}

impl SyncProgressSender {
    fn send(&self, progress: OperationProgress) {
        let _ = self.progress_tx.send(progress);
    }

    fn maybe_send_progress(&self, bytes_done: u64, items_done: u64, current_item: &str) {
        let now = std::time::Instant::now();
        let mut last = self.last_report.lock();
        if now.duration_since(*last).as_millis() >= 100 {
            *last = now;
            drop(last);
            self.send(OperationProgress::Progress {
                id: self.id,
                bytes_done,
                items_done,
                current_item: current_item.to_string(),
            });
        }
    }

    fn maybe_send_scanning(&self, items_found: u64, bytes_found: u64) {
        let now = std::time::Instant::now();
        let mut last = self.last_report.lock();
        if now.duration_since(*last).as_millis() >= 100 {
            *last = now;
            drop(last);
            self.send(OperationProgress::Scanning {
                id: self.id,
                items_found,
                bytes_found,
            });
        }
    }
}

// --- IssueOutcome: result of handle_io_error ---

enum IssueOutcome {
    Skip,
    Retry,
}

// --- ProgressReporter: async issue resolution + progress ---

struct ProgressReporter {
    sync_sender: SyncProgressSender,
    issue_resolvers: IssueResolvers,
    next_issue_id: Arc<AtomicU64>,
    sticky_resolutions: HashMap<IssueKind, IssueAction>,
    cancel: CancellationToken,
}

impl ProgressReporter {
    fn new(
        id: OperationId,
        progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>,
        issue_resolvers: IssueResolvers,
        next_issue_id: Arc<AtomicU64>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            sync_sender: SyncProgressSender {
                id,
                progress_tx,
                last_report: Arc::new(Mutex::new(std::time::Instant::now())),
            },
            issue_resolvers,
            next_issue_id,
            sticky_resolutions: HashMap::new(),
            cancel,
        }
    }

    fn id(&self) -> OperationId {
        self.sync_sender.id
    }

    fn send(&self, progress: OperationProgress) {
        self.sync_sender.send(progress);
    }

    fn send_prepared(&self, total_bytes: u64, total_items: u64) {
        self.send(OperationProgress::Prepared {
            id: self.id(),
            total_bytes,
            total_items,
        });
    }

    fn maybe_send_progress(&self, bytes_done: u64, items_done: u64, current_item: &str) {
        self.sync_sender
            .maybe_send_progress(bytes_done, items_done, current_item);
    }

    fn maybe_send_scanning(&self, items_found: u64, bytes_found: u64) {
        self.sync_sender
            .maybe_send_scanning(items_found, bytes_found);
    }

    fn send_completed(&self) {
        self.send(OperationProgress::Completed { id: self.id() });
    }

    fn send_failed(&self, error: String) {
        self.send(OperationProgress::Failed {
            id: self.id(),
            error,
        });
    }

    fn send_cancelled(&self) {
        self.send(OperationProgress::Cancelled { id: self.id() });
    }

    fn sync_sender(&self) -> SyncProgressSender {
        self.sync_sender.clone()
    }

    async fn raise_issue(
        &mut self,
        kind: IssueKind,
        message: String,
        detail: Option<String>,
        actions: Vec<IssueAction>,
    ) -> Result<IssueAction, crate::Error> {
        // Check sticky resolutions first
        if let Some(action) = self.sticky_resolutions.get(&kind) {
            return Ok(action.clone());
        }

        let issue_id = self.next_issue_id.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = oneshot::channel();
        self.issue_resolvers.lock().insert(issue_id, tx);

        self.send(OperationProgress::Issue {
            id: self.id(),
            issue: OperationIssue {
                issue_id,
                kind: kind.clone(),
                message,
                detail,
                actions,
            },
        });

        tokio::select! {
            result = rx => {
                match result {
                    Ok(response) => {
                        if response.apply_to_all {
                            self.sticky_resolutions
                                .insert(kind, response.action.clone());
                        }
                        Ok(response.action)
                    }
                    Err(_) => Err(crate::Error::cancelled()),
                }
            }
            _ = self.cancel.cancelled() => {
                Err(crate::Error::cancelled())
            }
        }
    }

    async fn handle_io_error(
        &mut self,
        error: crate::Error,
        context: &str,
        detail: Option<String>,
        cancel: &CancellationToken,
        allow_retry: bool,
    ) -> Result<IssueOutcome, crate::Error> {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        warn!("operation {}: {} — {}", self.id(), context, error);
        let kind = match error.kind {
            crate::ErrorKind::PermissionDenied => IssueKind::PermissionDenied,
            crate::ErrorKind::AlreadyExists => IssueKind::AlreadyExists,
            _ => IssueKind::IoError,
        };
        let mut actions = vec![IssueAction::Skip];
        if allow_retry {
            actions.push(IssueAction::Retry);
        }

        match self
            .raise_issue(kind, format!("{}: {}", context, error), detail, actions)
            .await?
        {
            IssueAction::Skip => Ok(IssueOutcome::Skip),
            IssueAction::Retry => Ok(IssueOutcome::Retry),
            _ => unreachable!("not offered"),
        }
    }
}

// --- Run command ---

async fn execute_run_command(
    reporter: &mut ProgressReporter,
    command: &str,
    working_dir: Option<&Path>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    reporter.send_prepared(0, 0);
    reporter.maybe_send_progress(0, 0, command);

    let mut child = {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", command]);
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.spawn()
            .map_err(|e| crate::Error::custom(format!("failed to spawn command: {}", e)))?
    };

    let status = tokio::select! {
        status = child.wait() => {
            status.map_err(|e| crate::Error::custom(format!("failed to wait for command: {}", e)))?
        }
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(crate::Error::custom("cancelled".to_string()));
        }
    };

    if status.success() {
        Ok(())
    } else {
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        Err(crate::Error::custom(format!(
            "command exited with code {}",
            code
        )))
    }
}

// --- Entry point ---

pub async fn execute_operation(
    id: OperationId,
    request: OperationRequest,
    progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>,
    cancel: CancellationToken,
    issue_resolvers: IssueResolvers,
    next_issue_id: Arc<AtomicU64>,
    context: Arc<OperationContext>,
) {
    info!("operation {}: starting [{:?}]", id, request);

    let mut reporter = ProgressReporter::new(
        id,
        progress_tx,
        issue_resolvers,
        next_issue_id,
        cancel.clone(),
    );

    let result = match request {
        OperationRequest::Delete { paths } => {
            execute_delete(&mut reporter, &context, paths, cancel.clone()).await
        }
        OperationRequest::Copy {
            sources,
            destination,
            options,
        } => {
            execute_copy(
                &mut reporter,
                &context,
                sources,
                destination,
                options,
                cancel.clone(),
                false,
                0,
            )
            .await
        }
        OperationRequest::Move {
            sources,
            destination,
            options,
        } => {
            execute_move(
                &mut reporter,
                &context,
                sources,
                destination,
                options,
                cancel.clone(),
            )
            .await
        }
        OperationRequest::SetMetadata {
            paths,
            mode_set,
            mode_clear,
            uid,
            gid,
            recursive,
        } => {
            execute_set_metadata(
                &mut reporter,
                &context,
                paths,
                mode_set,
                mode_clear,
                uid,
                gid,
                recursive,
                cancel.clone(),
            )
            .await
        }
        OperationRequest::RunCommand {
            command,
            working_dir,
        } => {
            execute_run_command(
                &mut reporter,
                &command,
                working_dir.as_deref(),
                cancel.clone(),
            )
            .await
        }
    };

    match &result {
        Ok(()) => info!("operation {}: completed", id),
        Err(_) if cancel.is_cancelled() => info!("operation {}: cancelled", id),
        Err(e) => info!("operation {}: failed: {}", id, e),
    }

    match result {
        Ok(()) => reporter.send_completed(),
        Err(_) if cancel.is_cancelled() => reporter.send_cancelled(),
        Err(e) => reporter.send_failed(e.to_string()),
    }
}

/// Wrap an async VFS call so it respects cancellation.
async fn cancellable<T>(
    cancel: &CancellationToken,
    fut: impl std::future::Future<Output = Result<T, crate::Error>>,
) -> Result<T, crate::Error> {
    tokio::select! {
        result = fut => result,
        _ = cancel.cancelled() => Err(crate::Error::cancelled()),
    }
}

// --- Plan copy (async, uses Vfs) ---

async fn plan_copy(
    src_vfs: &dyn Vfs,
    src_descriptor: &dyn VfsDescriptor,
    sources: &[PathBuf],
    destination: &Path,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<CopyPlan, crate::Error> {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;
    let has_symlinks = src_descriptor.has_symlinks();

    for source in sources {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let file_name = source
            .file_name()
            .ok_or_else(|| crate::Error::custom("source has no file name".to_string()))?;
        let dest_base = destination.join(file_name);

        // Classify the top-level source. Use file_info (stat) when available,
        // fall back to listing the parent directory for VFSes like S3 where
        // directories aren't real filesystem entries.
        let file_entry = if src_descriptor.can_stat_directories() {
            src_vfs.file_info(source).await?
        } else {
            let parent = source
                .parent()
                .ok_or_else(|| crate::Error::custom("source has no parent".to_string()))?;
            let file_list = cancellable(cancel, src_vfs.list_files(parent, None)).await?;
            file_list
                .into_iter()
                .find(|f| f.name == file_name.to_string_lossy().as_ref())
                .ok_or_else(|| {
                    crate::Error::custom(format!("source not found: {}", source.display()))
                })?
        };

        if has_symlinks && file_entry.is_symlink {
            entries.push(CopyEntry {
                source: source.clone(),
                dest: dest_base,
                kind: CopyEntryKind::Symlink {
                    target: file_entry.symlink_target.clone().unwrap(),
                },
                size_bytes: 0,
            });
        } else if file_entry.is_dir {
            let mut stack = vec![(source.clone(), dest_base.clone())];
            entries.push(CopyEntry {
                source: source.clone(),
                dest: dest_base,
                kind: CopyEntryKind::Directory,
                size_bytes: 0,
            });

            while let Some((src_dir, dst_dir)) = stack.pop() {
                if cancel.is_cancelled() {
                    return Err(crate::Error::cancelled());
                }

                let file_list = loop {
                    match cancellable(cancel, src_vfs.list_files(&src_dir, None)).await {
                        Ok(list) => break list,
                        Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                        Err(e) => {
                            match reporter
                                .handle_io_error(
                                    e,
                                    &format!("Error scanning directory {}", src_dir.display()),
                                    None,
                                    cancel,
                                    true,
                                )
                                .await?
                            {
                                IssueOutcome::Skip => break vec![],
                                IssueOutcome::Retry => continue,
                            }
                        }
                    }
                };

                for file in &file_list {
                    if file.name == ".." {
                        continue;
                    }
                    let src_path = src_dir.join(&file.name);
                    let dst_path = dst_dir.join(&file.name);

                    if has_symlinks && file.is_symlink {
                        entries.push(CopyEntry {
                            source: src_path,
                            dest: dst_path,
                            kind: CopyEntryKind::Symlink {
                                target: file.symlink_target.clone().unwrap(),
                            },
                            size_bytes: 0,
                        });
                    } else if file.is_dir {
                        entries.push(CopyEntry {
                            source: src_path.clone(),
                            dest: dst_path.clone(),
                            kind: CopyEntryKind::Directory,
                            size_bytes: 0,
                        });
                        stack.push((src_path, dst_path));
                    } else {
                        let size = file.size.unwrap_or(0);
                        total_bytes += size;
                        entries.push(CopyEntry {
                            source: src_path,
                            dest: dst_path,
                            kind: CopyEntryKind::File,
                            size_bytes: size,
                        });
                    }
                }
                reporter.maybe_send_scanning(entries.len() as u64, total_bytes);
            }
        } else {
            let size = file_entry.size.unwrap_or(0);
            total_bytes += size;
            entries.push(CopyEntry {
                source: source.clone(),
                dest: dest_base,
                kind: CopyEntryKind::File,
                size_bytes: size,
            });
        }
    }

    debug!(
        "plan_copy: {} entries, {} total bytes",
        entries.len(),
        total_bytes
    );

    Ok(CopyPlan {
        entries,
        total_bytes,
    })
}

// --- Chunked byte copy (runs in spawn_blocking with trait objects) ---

fn copy_bytes_sync(
    reader: &mut dyn std::io::Read,
    writer: &mut dyn std::io::Write,
    cancel: &CancellationToken,
    sender: &SyncProgressSender,
    bytes_done: &mut u64,
    items_done: u64,
    display: &str,
) -> Result<(), crate::Error> {
    let mut buf = [0u8; 64 * 1024];

    loop {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        *bytes_done += n as u64;
        sender.maybe_send_progress(*bytes_done, items_done, display);
    }

    Ok(())
}

// --- Async chunked byte copy ---

async fn copy_bytes_async(
    reader: &mut (dyn tokio::io::AsyncRead + Send + Unpin),
    writer: &mut dyn crate::vfs::VfsAsyncWriter,
    cancel: &CancellationToken,
    reporter: &mut ProgressReporter,
    bytes_done: &mut u64,
    items_done: u64,
    display: &str,
) -> Result<(), crate::Error> {
    use tokio::io::AsyncReadExt;

    let mut buf = [0u8; 64 * 1024];

    loop {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write(&buf[..n]).await?;
        *bytes_done += n as u64;
        reporter.maybe_send_progress(*bytes_done, items_done, display);
    }

    Ok(())
}

// --- Copy a single file through VFS, with strategy cascade ---

#[allow(clippy::too_many_arguments)]
async fn copy_single_file(
    src_vfs: &dyn Vfs,
    dst_vfs: &dyn Vfs,
    entry: &CopyEntry,
    same_vfs: bool,
    cancel: &CancellationToken,
    reporter: &mut ProgressReporter,
    bytes_done: &mut u64,
    items_done: u64,
    options: &CopyOptions,
    display: &str,
) -> Result<(), crate::Error> {
    let src_descriptor = src_vfs.descriptor();
    let dst_descriptor = dst_vfs.descriptor();

    // 1. Same-VFS copy_within fast path
    if same_vfs && dst_descriptor.can_copy_within() {
        debug!(
            "copy_single_file: trying copy_within for {}",
            entry.source.display()
        );
        if src_vfs
            .copy_within(&entry.source, &entry.dest)
            .await
            .is_ok()
        {
            *bytes_done += entry.size_bytes;
            return preserve_metadata(src_vfs, &entry.source, dst_vfs, &entry.dest, options).await;
        }
    }

    // 2. Sync read + sync write
    if src_descriptor.can_read_sync() && dst_descriptor.can_overwrite_sync() {
        debug!(
            "copy_single_file: sync-read + sync-write for {}",
            entry.source.display()
        );
        let mut reader = src_vfs.open_read_sync(&entry.source).await?;
        let mut writer = dst_vfs.overwrite_sync(&entry.dest).await?;

        let cancel2 = cancel.clone();
        let sender2 = reporter.sync_sender();
        let bd = *bytes_done;
        let id = items_done;
        let display_owned = display.to_string();

        let bd_back = tokio::task::spawn_blocking(move || {
            let mut bd_local = bd;
            let result = copy_bytes_sync(
                &mut *reader,
                &mut *writer,
                &cancel2,
                &sender2,
                &mut bd_local,
                id,
                &display_owned,
            );
            (bd_local, result)
        })
        .await?;

        *bytes_done = bd_back.0;
        bd_back.1?;

        return preserve_metadata(src_vfs, &entry.source, dst_vfs, &entry.dest, options).await;
    }

    // 3. Async read + async write
    if src_descriptor.can_read_async() && dst_descriptor.can_overwrite_async() {
        debug!(
            "copy_single_file: async-read + async-write for {}",
            entry.source.display()
        );
        let mut reader = src_vfs.open_read_async(&entry.source).await?;
        let mut writer = dst_vfs.overwrite_async(&entry.dest).await?;

        copy_bytes_async(
            &mut *reader,
            &mut *writer,
            cancel,
            reporter,
            bytes_done,
            items_done,
            display,
        )
        .await?;
        writer.finish().await?;

        return preserve_metadata(src_vfs, &entry.source, dst_vfs, &entry.dest, options).await;
    }

    // 4. Sync read + async write
    if src_descriptor.can_read_sync() && dst_descriptor.can_overwrite_async() {
        debug!(
            "copy_single_file: sync-read + async-write for {}",
            entry.source.display()
        );
        let sync_reader = src_vfs.open_read_sync(&entry.source).await?;
        let mut writer = dst_vfs.overwrite_async(&entry.dest).await?;

        // Bridge sync reader to async via spawn_blocking + channel
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, crate::Error>>(4);
        let cancel2 = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let mut reader = sync_reader;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                if cancel2.is_cancelled() {
                    let _ = tx.blocking_send(Err(crate::Error::cancelled()));
                    return;
                }
                match std::io::Read::read(&mut *reader, &mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        if tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Err(e.into()));
                        return;
                    }
                }
            }
        });

        while let Some(chunk) = rx.recv().await {
            let data = chunk?;
            writer.write(&data).await?;
            *bytes_done += data.len() as u64;
            reporter.maybe_send_progress(*bytes_done, items_done, display);
        }
        writer.finish().await?;

        return preserve_metadata(src_vfs, &entry.source, dst_vfs, &entry.dest, options).await;
    }

    // 5. Async read + sync write
    if src_descriptor.can_read_async() && dst_descriptor.can_overwrite_sync() {
        debug!(
            "copy_single_file: async-read + sync-write for {}",
            entry.source.display()
        );
        let mut reader = src_vfs.open_read_async(&entry.source).await?;
        let sync_writer = dst_vfs.overwrite_sync(&entry.dest).await?;

        // Bridge async reader to sync writer via channel + spawn_blocking
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Vec<u8>, crate::Error>>(4);
        let cancel2 = cancel.clone();
        let sender2 = reporter.sync_sender();
        let bd = *bytes_done;
        let id = items_done;
        let display_owned = display.to_string();

        let writer_handle = tokio::task::spawn_blocking(move || {
            let mut writer = sync_writer;
            let mut bd_local = bd;
            for chunk in rx {
                match chunk {
                    Ok(data) => {
                        if let Err(e) = std::io::Write::write_all(&mut *writer, &data) {
                            return (bd_local, Err(e.into()));
                        }
                        bd_local += data.len() as u64;
                        sender2.maybe_send_progress(bd_local, id, &display_owned);
                    }
                    Err(e) => return (bd_local, Err(e)),
                }
            }
            (bd_local, Ok(()))
        });

        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if cancel2.is_cancelled() {
                drop(tx);
                let _ = writer_handle.await;
                return Err(crate::Error::cancelled());
            }
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            if tx.send(Ok(buf[..n].to_vec())).is_err() {
                break;
            }
        }
        drop(tx);

        let (bd_back, result) = writer_handle.await?;
        *bytes_done = bd_back;
        result?;

        return preserve_metadata(src_vfs, &entry.source, dst_vfs, &entry.dest, options).await;
    }

    Err(crate::Error::not_supported())
}

// --- Preserve metadata after copy ---

async fn preserve_metadata(
    src_vfs: &dyn Vfs,
    src_path: &Path,
    dst_vfs: &dyn Vfs,
    dst_path: &Path,
    options: &CopyOptions,
) -> Result<(), crate::Error> {
    // Skip entirely if destination doesn't support metadata
    if !dst_vfs.descriptor().can_set_metadata() {
        return Ok(());
    }

    // Always try to preserve permissions; additionally preserve timestamps/owner/group if requested
    let meta = match src_vfs.get_metadata(src_path).await {
        Ok(m) => m,
        Err(_) => return Ok(()), // source doesn't support metadata, nothing to preserve
    };

    let mut to_set = crate::vfs::VfsMetadata {
        permissions: meta.permissions,
        ..Default::default()
    };

    if options.preserve_timestamps {
        to_set.atime = meta.atime;
        to_set.mtime = meta.mtime;
    }
    if options.preserve_owner {
        to_set.uid = meta.uid;
    }
    if options.preserve_group {
        to_set.gid = meta.gid;
    }

    // Ignore errors on destination that doesn't support metadata
    let _ = dst_vfs.set_metadata(dst_path, &to_set).await;

    Ok(())
}

// --- Execute Copy (async outer loop, uses Vfs) ---

#[allow(clippy::too_many_arguments)]
async fn execute_copy(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    options: CopyOptions,
    cancel: CancellationToken,
    is_move: bool,
    items_done_offset: u64,
) -> Result<(), crate::Error> {
    let first_source = sources
        .first()
        .ok_or_else(|| crate::Error::custom("no sources provided"))?;
    let (src_vfs, _) = context.registry.resolve(first_source)?;
    let (dst_vfs, dst_path) = context.registry.resolve(&destination)?;

    let src_vfs_id = first_source.vfs_id;
    let dst_vfs_id = destination.vfs_id;
    let same_vfs = src_vfs_id == dst_vfs_id;

    // Validate all sources share the same VFS
    if let Some(mismatched) = sources.iter().find(|s| s.vfs_id != src_vfs_id) {
        return Err(crate::Error::custom(format!(
            "all sources must be on the same VFS (expected {}, got {})",
            src_vfs_id, mismatched.vfs_id
        )));
    }

    let src_descriptor = src_vfs.descriptor();
    let dst_descriptor = dst_vfs.descriptor();

    debug!(
        "execute_copy: {} sources, src_vfs={} ({}), dst_vfs={} ({}), same_vfs={}",
        sources.len(),
        src_vfs_id,
        src_descriptor.type_name(),
        dst_vfs_id,
        dst_descriptor.type_name(),
        same_vfs
    );

    let source_paths: Vec<PathBuf> = sources.iter().map(|s| s.path.clone()).collect();

    // Handle create_symlink for single-file copy
    if options.create_symlink {
        if !dst_descriptor.can_create_symlink() {
            return Err(crate::Error::custom(
                "Destination does not support symlink creation".to_string(),
            ));
        }
        if source_paths.len() != 1 {
            return Err(crate::Error::custom(
                "Symlink creation only supported for single file".to_string(),
            ));
        }
        let source = &source_paths[0];
        let file_name = match source.file_name() {
            Some(f) => f.to_owned(),
            None => return Err(crate::Error::custom("source has no file name".to_string())),
        };
        let dest = dst_path.join(file_name);
        reporter.send_prepared(0, 1);
        dst_vfs.create_symlink(&dest, source).await?;
        return Ok(());
    }

    let plan = plan_copy(
        &*src_vfs,
        src_descriptor,
        &source_paths,
        &dst_path,
        reporter,
        &cancel,
    )
    .await?;

    let total_items = plan.entries.len() as u64 + items_done_offset;
    reporter.send_prepared(plan.total_bytes, total_items);

    let mut bytes_done = 0u64;
    let mut items_done = items_done_offset;

    for entry in &plan.entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let display = entry
            .dest
            .strip_prefix(&dst_path)
            .unwrap_or(&entry.dest)
            .display()
            .to_string();
        reporter.maybe_send_progress(bytes_done, items_done, &display);

        // Check for destination conflicts
        let dest_file = dst_vfs.file_info(&entry.dest).await;
        if let Ok(dest_file) = dest_file {
            match &entry.kind {
                CopyEntryKind::Directory => {
                    if dest_file.is_dir {
                        // Merge: directory already exists, skip mkdir
                        items_done += 1;
                        continue;
                    } else {
                        // Type mismatch
                        match reporter
                            .raise_issue(
                                IssueKind::AlreadyExists,
                                format!(
                                    "Cannot replace file with directory: {}",
                                    entry.dest.display()
                                ),
                                None,
                                vec![IssueAction::Skip],
                            )
                            .await
                        {
                            Ok(IssueAction::Skip) => {
                                items_done += 1;
                                continue;
                            }
                            Err(e) => return Err(e),
                            _ => unreachable!("not offered"),
                        }
                    }
                }
                CopyEntryKind::File | CopyEntryKind::Symlink { .. } => {
                    if dest_file.is_dir {
                        // Type mismatch
                        match reporter
                            .raise_issue(
                                IssueKind::AlreadyExists,
                                format!(
                                    "Cannot replace directory with file: {}",
                                    entry.dest.display()
                                ),
                                None,
                                vec![IssueAction::Skip],
                            )
                            .await
                        {
                            Ok(IssueAction::Skip) => {
                                bytes_done += entry.size_bytes;
                                items_done += 1;
                                continue;
                            }
                            Err(e) => return Err(e),
                            _ => unreachable!("not offered"),
                        }
                    } else {
                        // Both are files (or symlinks)
                        match reporter
                            .raise_issue(
                                IssueKind::AlreadyExists,
                                format!("File already exists: {}", entry.dest.display()),
                                None,
                                vec![IssueAction::Skip, IssueAction::Overwrite],
                            )
                            .await
                        {
                            Ok(IssueAction::Skip) => {
                                bytes_done += entry.size_bytes;
                                items_done += 1;
                                continue;
                            }
                            Ok(IssueAction::Overwrite) => {
                                let source_is_symlink =
                                    matches!(&entry.kind, CopyEntryKind::Symlink { .. });
                                if dest_file.is_symlink || source_is_symlink {
                                    // Remove when either side is a symlink:
                                    // - dest is symlink: writing would go through to the
                                    //   target rather than replacing the symlink itself
                                    // - source is symlink: create_symlink can't overwrite
                                    //   an existing file
                                    dst_vfs.remove_file(&entry.dest).await?;
                                }
                                // For regular file → regular file: overwrite in place.
                                // VFS write methods truncate and replace contents without
                                // a delete+create gap, so partial failure doesn't lose
                                // the destination.
                            }
                            Err(e) => return Err(e),
                            _ => unreachable!("not offered"),
                        }
                    }
                }
            }
        }

        // Perform the operation
        let bytes_before = bytes_done;
        let mut retry = true;
        let mut succeeded = false;
        while retry {
            retry = false;
            bytes_done = bytes_before; // Reset progress on retry to avoid double-counting

            let result = match &entry.kind {
                CopyEntryKind::Directory => dst_vfs.create_directory(&entry.dest).await,
                CopyEntryKind::Symlink { target } => {
                    if dst_descriptor.can_create_symlink() {
                        dst_vfs.create_symlink(&entry.dest, target).await
                    } else {
                        Err(crate::Error::custom(format!(
                            "Cannot create symlink on {}: not supported",
                            dst_descriptor.type_name()
                        )))
                    }
                }
                CopyEntryKind::File => {
                    copy_single_file(
                        &*src_vfs,
                        &*dst_vfs,
                        entry,
                        same_vfs,
                        &cancel,
                        reporter,
                        &mut bytes_done,
                        items_done,
                        &options,
                        &display,
                    )
                    .await
                }
            };

            match result {
                Ok(()) => {
                    succeeded = true;
                }
                Err(e) => {
                    match reporter
                        .handle_io_error(
                            e,
                            "Error",
                            Some(format!(
                                "{} -> {}",
                                entry.source.display(),
                                entry.dest.display()
                            )),
                            &cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => {
                            // Advance bytes so progress reaches 100% even with skips
                            bytes_done = bytes_before + entry.size_bytes;
                        }
                        IssueOutcome::Retry => {
                            retry = true;
                        }
                    }
                }
            }
        }

        // For move: delete source file/symlink immediately after successful copy.
        // Directories are cleaned up in a separate reverse pass below.
        if is_move
            && succeeded
            && matches!(
                &entry.kind,
                CopyEntryKind::File | CopyEntryKind::Symlink { .. }
            )
        {
            let mut src_retry = true;
            while src_retry {
                src_retry = false;
                if let Err(e) = src_vfs.remove_file(&entry.source).await {
                    match reporter
                        .handle_io_error(
                            e,
                            &format!("Error removing source {}", entry.source.display()),
                            None,
                            &cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => {}
                        IssueOutcome::Retry => {
                            src_retry = true;
                        }
                    }
                }
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(bytes_done, items_done, "");

    // For move: reverse pass to clean up empty source directories (deepest first).
    // DirectoryNotEmpty is expected (items may have been skipped) and silently ignored.
    // Other errors (e.g. permission denied) are reported through issue resolution.
    if is_move {
        for entry in plan.entries.iter().rev() {
            if cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }
            if let CopyEntryKind::Directory = &entry.kind {
                let mut dir_retry = true;
                while dir_retry {
                    dir_retry = false;
                    if let Err(e) = src_vfs.remove_dir(&entry.source).await {
                        if e.kind == crate::ErrorKind::DirectoryNotEmpty {
                            // Expected when child items were skipped — leave intact
                        } else {
                            match reporter
                                .handle_io_error(
                                    e,
                                    &format!(
                                        "Error removing source directory {}",
                                        entry.source.display()
                                    ),
                                    None,
                                    &cancel,
                                    true,
                                )
                                .await?
                            {
                                IssueOutcome::Skip => {}
                                IssueOutcome::Retry => {
                                    dir_retry = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// --- Execute Delete (async outer loop, uses Vfs) ---

/// Determine whether a path is a directory.
///
/// When `can_stat_directories` is true (most VFSes), uses `file_info` directly.
/// When false (e.g. S3), falls back to listing the parent directory.
async fn probe_is_dir(
    vfs: &dyn Vfs,
    descriptor: &dyn VfsDescriptor,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<bool, crate::Error> {
    if descriptor.can_stat_directories() {
        return Ok(vfs.file_info(path).await?.is_dir);
    }

    let parent = path.parent().unwrap_or(Path::new("/"));
    let file_name = path.file_name().map(|n| n.to_string_lossy().to_string());
    match file_name {
        Some(name) => {
            let listing = cancellable(cancel, vfs.list_files(parent, None)).await?;
            Ok(listing
                .iter()
                .find(|f| f.name == name)
                .is_some_and(|f| f.is_dir && !f.is_symlink))
        }
        None => Ok(true), // root-level path, treat as directory
    }
}

/// Walk a directory tree depth-first and collect all entries for deletion.
/// Returns entries in deletion order: files first, then directories (deepest first).
struct DeleteEntry {
    path: PathBuf,
    is_dir: bool,
}

async fn collect_delete_entries(
    vfs: &dyn Vfs,
    path: &Path,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<DeleteEntry>, crate::Error> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut stack = vec![path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let file_list = loop {
            match cancellable(cancel, vfs.list_files(&dir, None)).await {
                Ok(list) => break list,
                Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                Err(e) => {
                    match reporter
                        .handle_io_error(
                            e,
                            &format!("Error scanning directory {}", dir.display()),
                            None,
                            cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => break vec![],
                        IssueOutcome::Retry => continue,
                    }
                }
            }
        };

        for file in &file_list {
            if file.name == ".." {
                continue;
            }
            let entry_path = dir.join(&file.name);
            if file.is_dir && !file.is_symlink {
                stack.push(entry_path.clone());
                dirs.push(DeleteEntry {
                    path: entry_path,
                    is_dir: true,
                });
            } else {
                files.push(DeleteEntry {
                    path: entry_path,
                    is_dir: false,
                });
            }
        }
    }

    // Files first, then directories in reverse order (deepest first)
    dirs.reverse();
    files.extend(dirs);
    Ok(files)
}

async fn collect_chmod_entries(
    vfs: &dyn Vfs,
    path: &Path,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<PathBuf>, crate::Error> {
    let mut entries = vec![path.to_path_buf()];
    let mut stack = vec![path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let file_list = loop {
            match cancellable(cancel, vfs.list_files(&dir, None)).await {
                Ok(list) => break list,
                Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                Err(e) => {
                    match reporter
                        .handle_io_error(
                            e,
                            &format!("Error scanning directory {}", dir.display()),
                            None,
                            cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => break vec![],
                        IssueOutcome::Retry => continue,
                    }
                }
            }
        };

        for file in &file_list {
            if file.name == ".." {
                continue;
            }
            let entry_path = dir.join(&file.name);
            if file.is_dir && !file.is_symlink {
                stack.push(entry_path.clone());
            }
            entries.push(entry_path);
        }
    }

    Ok(entries)
}

#[allow(clippy::too_many_arguments)]
async fn execute_set_metadata(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    mode_set: u32,
    mode_clear: u32,
    uid: Option<u32>,
    gid: Option<u32>,
    recursive: bool,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!(
        "execute_set_metadata: {} paths, mode_set={:o}, mode_clear={:o}, uid={:?}, gid={:?}, recursive={}",
        paths.len(),
        mode_set,
        mode_clear,
        uid,
        gid,
        recursive
    );

    // Collect all entries to process
    let mut all_entries: Vec<(Arc<dyn Vfs>, PathBuf, String)> = Vec::new();

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;
        let descriptor = vfs.descriptor();

        if recursive {
            let is_dir = probe_is_dir(&*vfs, descriptor, &local_path, &cancel).await?;
            if is_dir {
                let entries = collect_chmod_entries(&*vfs, &local_path, reporter, &cancel).await?;
                for entry in entries {
                    let display = format!("{}:{}", vfs_path.vfs_id, entry.display());
                    all_entries.push((vfs.clone(), entry, display));
                }
                continue;
            }
        }

        let display = vfs_path.to_string();
        all_entries.push((vfs, local_path, display));
    }

    let total_items = all_entries.len() as u64;
    reporter.send_prepared(0, total_items);

    let has_mode_changes = mode_set != 0 || mode_clear != 0;

    let mut items_done = 0u64;

    for (vfs, local_path, display) in &all_entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        reporter.maybe_send_progress(0, items_done, display);

        let mut retry = true;
        while retry {
            retry = false;

            // Compute the new permissions if mode bits are being changed
            let new_permissions = if has_mode_changes {
                match vfs.file_info(local_path).await {
                    Ok(file_info) => {
                        let old_mode = file_info.mode.map(|m| m.0).unwrap_or(0);
                        Some((old_mode | mode_set) & !mode_clear)
                    }
                    Err(e) => {
                        match reporter
                            .handle_io_error(
                                e,
                                &format!("Error setting metadata on {}", display),
                                None,
                                &cancel,
                                true,
                            )
                            .await?
                        {
                            IssueOutcome::Skip => {
                                break;
                            }
                            IssueOutcome::Retry => {
                                retry = true;
                                continue;
                            }
                        }
                    }
                }
            } else {
                None
            };

            let meta = crate::vfs::VfsMetadata {
                permissions: new_permissions,
                uid,
                gid,
                ..Default::default()
            };

            if let Err(e) = vfs.set_metadata(local_path, &meta).await {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error setting metadata on {}", display),
                        None,
                        &cancel,
                        true,
                    )
                    .await?
                {
                    IssueOutcome::Skip => {}
                    IssueOutcome::Retry => {
                        retry = true;
                    }
                }
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(0, items_done, "");
    Ok(())
}

/// Flattened delete entry with the VFS it belongs to.
struct ResolvedDeleteEntry {
    vfs: Arc<dyn Vfs>,
    path: PathBuf,
    is_dir: bool,
    /// Whether to use atomic remove_tree (skips per-item walk).
    use_remove_tree: bool,
}

async fn execute_delete(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!("execute_delete: {} paths", paths.len());

    // Phase 1: Scan — collect all entries into a flat list so we know the
    // real total before we start deleting.
    let mut all_entries: Vec<ResolvedDeleteEntry> = Vec::new();

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;
        let descriptor = vfs.descriptor();

        if descriptor.can_remove_tree() {
            // Fast path: single atomic removal, count as 1 item
            all_entries.push(ResolvedDeleteEntry {
                vfs,
                path: local_path,
                is_dir: true,
                use_remove_tree: true,
            });
        } else {
            let is_dir = probe_is_dir(&*vfs, descriptor, &local_path, &cancel).await?;
            if is_dir {
                let children =
                    collect_delete_entries(&*vfs, &local_path, reporter, &cancel).await?;
                for entry in children {
                    all_entries.push(ResolvedDeleteEntry {
                        vfs: vfs.clone(),
                        path: entry.path,
                        is_dir: entry.is_dir,
                        use_remove_tree: false,
                    });
                }
                // The top-level directory itself (removed last)
                all_entries.push(ResolvedDeleteEntry {
                    vfs,
                    path: local_path,
                    is_dir: true,
                    use_remove_tree: false,
                });
            } else {
                all_entries.push(ResolvedDeleteEntry {
                    vfs,
                    path: local_path,
                    is_dir: false,
                    use_remove_tree: false,
                });
            }
        }

        reporter.maybe_send_scanning(all_entries.len() as u64, 0);
    }

    // Phase 2: Execute
    let total_items = all_entries.len() as u64;
    reporter.send_prepared(0, total_items);

    let mut items_done = 0u64;

    for entry in &all_entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let display = entry.path.display().to_string();
        reporter.maybe_send_progress(0, items_done, &display);

        let mut retry = true;
        while retry {
            retry = false;

            let result = if entry.use_remove_tree {
                entry.vfs.remove_tree(&entry.path).await
            } else if entry.is_dir {
                entry.vfs.remove_dir(&entry.path).await
            } else {
                entry.vfs.remove_file(&entry.path).await
            };

            if let Err(e) = result {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error deleting {}", entry.path.display()),
                        None,
                        &cancel,
                        true,
                    )
                    .await?
                {
                    IssueOutcome::Skip => {}
                    IssueOutcome::Retry => {
                        retry = true;
                    }
                }
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(0, items_done, "");
    Ok(())
}

// --- Execute Move (async, uses Vfs) ---

async fn execute_move(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    options: CopyOptions,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    let src_vfs_id = sources
        .first()
        .ok_or_else(|| crate::Error::custom("no sources provided"))?
        .vfs_id;
    let dst_vfs_id = destination.vfs_id;
    let same_vfs = src_vfs_id == dst_vfs_id;

    let (src_vfs, _) = context.registry.resolve(&sources[0])?;
    let (_, dst_path) = context.registry.resolve(&destination)?;
    let src_descriptor = src_vfs.descriptor();

    let mut needs_copy = Vec::new();
    let mut renamed_count = 0u64;

    if same_vfs && src_descriptor.can_rename() {
        debug!(
            "execute_move: trying rename for {} sources (same VFS)",
            sources.len()
        );
        // Try rename first for each source (instant for same-VFS, same-device)
        for source in &sources {
            if cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }

            let file_name = match source.path.file_name() {
                Some(f) => f,
                None => return Err(crate::Error::custom("source has no file name".to_string())),
            };
            let dest_local = dst_path.join(file_name);

            // Check for destination conflicts before renaming (rename silently overwrites)
            if let Ok(dest_file) = src_vfs.file_info(&dest_local).await {
                let source_file = src_vfs.file_info(&source.path).await?;
                if dest_file.is_dir != source_file.is_dir {
                    // Type mismatch (file vs directory) — can only skip
                    let msg = if dest_file.is_dir {
                        format!(
                            "Cannot replace directory with file: {}",
                            dest_local.display()
                        )
                    } else {
                        format!(
                            "Cannot replace file with directory: {}",
                            dest_local.display()
                        )
                    };
                    match reporter
                        .raise_issue(IssueKind::AlreadyExists, msg, None, vec![IssueAction::Skip])
                        .await
                    {
                        Ok(IssueAction::Skip) => continue,
                        Err(e) => return Err(e),
                        _ => unreachable!("not offered"),
                    }
                } else if !dest_file.is_dir {
                    // Both are files — offer skip/overwrite
                    match reporter
                        .raise_issue(
                            IssueKind::AlreadyExists,
                            format!("File already exists: {}", dest_local.display()),
                            None,
                            vec![IssueAction::Skip, IssueAction::Overwrite],
                        )
                        .await
                    {
                        Ok(IssueAction::Skip) => continue,
                        Ok(IssueAction::Overwrite) => {
                            // Proceed with rename (will overwrite)
                        }
                        Err(e) => return Err(e),
                        _ => unreachable!("not offered"),
                    }
                }
                // Both are directories: merge semantics — fall through to rename
                // (which will fail for non-empty dirs, then fall back to copy+delete)
            }

            match src_vfs.rename(&source.path, &dest_local).await {
                Ok(()) => {
                    debug!(
                        "execute_move: renamed {} -> {}",
                        source.path.display(),
                        dest_local.display()
                    );
                    renamed_count += 1;
                }
                Err(_) => {
                    debug!(
                        "execute_move: rename failed for {}, falling back to copy+delete",
                        source.path.display()
                    );
                    // Any rename failure (cross-device, permission, etc.)
                    // falls through to copy+delete
                    needs_copy.push(source.clone());
                }
            }
        }
    } else {
        // Cross-VFS or VFS doesn't support rename: all sources need copy+delete
        needs_copy = sources.clone();
    }

    if needs_copy.is_empty() {
        reporter.send_prepared(0, renamed_count);
        reporter.maybe_send_progress(0, renamed_count, "");
        return Ok(());
    }

    // Fall back to copy-then-delete-per-file for cross-device/cross-VFS moves.
    // execute_copy with is_move=true deletes each source file immediately after
    // a successful copy, then cleans up empty source directories in reverse order.
    execute_copy(
        reporter,
        context,
        needs_copy,
        destination,
        options,
        cancel,
        true,
        renamed_count,
    )
    .await
}

#[cfg(test)]
#[path = "operation_tests.rs"]
mod tests;
