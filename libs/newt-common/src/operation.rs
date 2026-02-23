use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::rpc::Communicator;

pub type OperationId = u64;
pub type IssueId = u64;

// --- Issue Resolution Types ---

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub enum IssueKind {
    AlreadyExists,
    PermissionDenied,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum IssueAction {
    Skip,
    Overwrite,
    Retry,
    Abort,
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
        sources: Vec<PathBuf>,
        destination: PathBuf,
        #[serde(default)]
        options: CopyOptions,
    },
    Move {
        sources: Vec<PathBuf>,
        destination: PathBuf,
        #[serde(default)]
        options: CopyOptions,
    },
    Delete {
        paths: Vec<PathBuf>,
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
    size: u64,
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
}

impl Local {
    pub fn new(progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>) -> Self {
        Self {
            operations: Arc::new(Mutex::new(HashMap::new())),
            next_issue_id: Arc::new(AtomicU64::new(1)),
            progress_tx,
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
        let id = req.id;

        tokio::spawn(async move {
            execute_operation(id, req.request, progress_tx, cancel, issue_resolvers, next_issue_id)
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
        if let Some(handle) = self.operations.lock().get(&req.operation_id) {
            if let Some(sender) = handle.issue_resolvers.lock().remove(&req.issue_id) {
                let _ = sender.send(req.response);
            }
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

    fn maybe_send_progress(&mut self, bytes_done: u64, items_done: u64, current_item: &str) {
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

    fn maybe_send_progress(&mut self, bytes_done: u64, items_done: u64, current_item: &str) {
        self.sync_sender
            .maybe_send_progress(bytes_done, items_done, current_item);
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
                    Err(_) => Err(crate::Error::Cancelled),
                }
            }
            _ = self.cancel.cancelled() => {
                Err(crate::Error::Cancelled)
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
            return Err(crate::Error::Cancelled);
        }
        let kind = match &error {
            crate::Error::Io(io_err) => issue_kind_from_io_error(io_err),
            _ => IssueKind::Other(format!("{}", error)),
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

fn issue_kind_from_io_error(e: &std::io::Error) -> IssueKind {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => IssueKind::PermissionDenied,
        std::io::ErrorKind::AlreadyExists => IssueKind::AlreadyExists,
        other => IssueKind::Other(format!("{:?}", other)),
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
) {
    let mut reporter = ProgressReporter::new(id, progress_tx, issue_resolvers, next_issue_id, cancel.clone());

    let result = match request {
        OperationRequest::Delete { paths } => {
            execute_delete(&mut reporter, paths, cancel.clone()).await
        }
        OperationRequest::Copy {
            sources,
            destination,
            options,
        } => execute_copy(&mut reporter, sources, destination, options, cancel.clone()).await,
        OperationRequest::Move {
            sources,
            destination,
            options,
        } => execute_move(&mut reporter, sources, destination, options, cancel.clone()).await,
    };

    match result {
        Ok(()) => reporter.send_completed(),
        Err(_) if cancel.is_cancelled() => reporter.send_cancelled(),
        Err(e) => reporter.send_failed(e.to_string()),
    }
}

// --- Plan copy ---

fn plan_copy(sources: &[PathBuf], destination: &PathBuf) -> Result<CopyPlan, crate::Error> {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;

    for source in sources {
        let file_name = source
            .file_name()
            .ok_or_else(|| crate::Error::Custom("source has no file name".to_string()))?;
        let dest_base = destination.join(file_name);

        let meta = std::fs::symlink_metadata(source)?;

        if meta.is_symlink() {
            let target = std::fs::read_link(source)?;
            entries.push(CopyEntry {
                source: source.clone(),
                dest: dest_base,
                kind: CopyEntryKind::Symlink { target },
                size: 0,
            });
        } else if meta.is_dir() {
            let mut stack = vec![(source.clone(), dest_base.clone())];
            entries.push(CopyEntry {
                source: source.clone(),
                dest: dest_base,
                kind: CopyEntryKind::Directory,
                size: 0,
            });

            while let Some((src_dir, dst_dir)) = stack.pop() {
                for entry in std::fs::read_dir(&src_dir)? {
                    let entry = entry?;
                    let src_path = entry.path();
                    let dst_path = dst_dir.join(entry.file_name());
                    let metadata = std::fs::symlink_metadata(&src_path)?;

                    if metadata.is_symlink() {
                        let target = std::fs::read_link(&src_path)?;
                        entries.push(CopyEntry {
                            source: src_path,
                            dest: dst_path,
                            kind: CopyEntryKind::Symlink { target },
                            size: 0,
                        });
                    } else if metadata.is_dir() {
                        entries.push(CopyEntry {
                            source: src_path.clone(),
                            dest: dst_path.clone(),
                            kind: CopyEntryKind::Directory,
                            size: 0,
                        });
                        stack.push((src_path, dst_path));
                    } else {
                        let size = metadata.len();
                        total_bytes += size;
                        entries.push(CopyEntry {
                            source: src_path,
                            dest: dst_path,
                            kind: CopyEntryKind::File,
                            size,
                        });
                    }
                }
            }
        } else {
            let size = meta.len();
            total_bytes += size;
            entries.push(CopyEntry {
                source: source.clone(),
                dest: dest_base,
                kind: CopyEntryKind::File,
                size,
            });
        }
    }

    Ok(CopyPlan {
        entries,
        total_bytes,
    })
}

// --- Chunked file copy (runs in spawn_blocking) ---

fn copy_file_chunked(
    source: &PathBuf,
    dest: &PathBuf,
    cancel: &CancellationToken,
    sender: &mut SyncProgressSender,
    bytes_done: &mut u64,
    items_done: u64,
    options: &CopyOptions,
) -> Result<(), crate::Error> {
    use std::io::{Read, Write};

    let src_metadata = std::fs::metadata(source)?;
    let mut src = std::fs::File::open(source)?;
    let mut dst = std::fs::File::create(dest)?;

    let mut buf = [0u8; 64 * 1024];
    let display = source.display().to_string();

    loop {
        if cancel.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }

        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])?;
        *bytes_done += n as u64;
        sender.maybe_send_progress(*bytes_done, items_done, &display);
    }

    // Preserve permissions
    #[cfg(unix)]
    {
        std::fs::set_permissions(dest, src_metadata.permissions())?;
    }

    // Preserve timestamps
    if options.preserve_timestamps {
        let atime = filetime::FileTime::from_last_access_time(&src_metadata);
        let mtime = filetime::FileTime::from_last_modification_time(&src_metadata);
        filetime::set_file_times(dest, atime, mtime)?;
    }

    // Preserve owner/group
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let uid = if options.preserve_owner {
            Some(nix::unistd::Uid::from_raw(src_metadata.uid()))
        } else {
            None
        };
        let gid = if options.preserve_group {
            Some(nix::unistd::Gid::from_raw(src_metadata.gid()))
        } else {
            None
        };
        if uid.is_some() || gid.is_some() {
            nix::unistd::chown(dest.as_path(), uid, gid)?;
        }
    }

    Ok(())
}

// --- Execute Copy (async outer loop) ---

async fn execute_copy(
    reporter: &mut ProgressReporter,
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: CopyOptions,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    // Handle create_symlink for single-file copy
    if options.create_symlink {
        if sources.len() != 1 {
            return Err(crate::Error::Custom(
                "Symlink creation only supported for single file".to_string(),
            ));
        }
        let source = sources[0].clone();
        let file_name = match source.file_name() {
            Some(f) => f.to_owned(),
            None => {
                return Err(crate::Error::Custom(
                    "source has no file name".to_string(),
                ))
            }
        };
        let dest = destination.join(file_name);
        reporter.send_prepared(0, 1);

        match tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&source, &dest)?;
            #[cfg(not(unix))]
            return Err(crate::Error::Custom(
                "Symlink creation not supported on this platform".to_string(),
            ));
            Ok::<(), crate::Error>(())
        })
        .await
        {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(e.into()),
        }
    }

    let plan_task = tokio::task::spawn_blocking({
        let sources = sources.clone();
        let destination = destination.clone();
        move || plan_copy(&sources, &destination)
    });

    let plan = tokio::select! {
        result = plan_task => match result {
            Ok(Ok(plan)) => plan,
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(e.into()),
        },
        _ = cancel.cancelled() => return Err(crate::Error::Cancelled),
    };

    let total_items = plan.entries.len() as u64;
    reporter.send_prepared(plan.total_bytes, total_items);

    let mut bytes_done = 0u64;
    let mut items_done = 0u64;

    for entry in &plan.entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }

        let display = entry.source.display().to_string();
        reporter.maybe_send_progress(bytes_done, items_done, &display);

        // Check for destination conflicts
        let dest_meta = tokio::fs::symlink_metadata(&entry.dest).await;
        if let Ok(dest_meta) = dest_meta {
            match &entry.kind {
                CopyEntryKind::Directory => {
                    if dest_meta.is_dir() {
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
                    if dest_meta.is_dir() {
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
                                vec![
                                    IssueAction::Skip,
                                    IssueAction::Overwrite,
                                ],
                            )
                            .await
                        {
                            Ok(IssueAction::Skip) => {
                                items_done += 1;
                                continue;
                            }
                            Ok(IssueAction::Overwrite) => {
                                let dest = entry.dest.clone();
                                let remove_result =
                                    tokio::task::spawn_blocking(move || std::fs::remove_file(&dest))
                                        .await;
                                match remove_result {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => return Err(e.into()),
                                    Err(e) => return Err(e.into()),
                                }
                            }
                            Err(e) => return Err(e),
                            _ => unreachable!("not offered"),
                        }
                    }
                }
            }
        }

        // Perform the operation
        let mut retry = true;
        while retry {
            retry = false;

            let result = match &entry.kind {
                CopyEntryKind::Directory => {
                    let dest = entry.dest.clone();
                    tokio::task::spawn_blocking(move || std::fs::create_dir(&dest))
                        .await
                        .map_err(crate::Error::from)
                        .and_then(|r| r.map_err(crate::Error::from))
                }
                CopyEntryKind::Symlink { target } => {
                    let target = target.clone();
                    let dest = entry.dest.clone();
                    #[cfg(unix)]
                    {
                        tokio::task::spawn_blocking(move || {
                            std::os::unix::fs::symlink(&target, &dest)
                        })
                        .await
                        .map_err(crate::Error::from)
                        .and_then(|r| r.map_err(crate::Error::from))
                    }
                    #[cfg(not(unix))]
                    {
                        Err(crate::Error::Custom(
                            "Symlink not supported on this platform".to_string(),
                        ))
                    }
                }
                CopyEntryKind::File => {
                    let source = entry.source.clone();
                    let dest = entry.dest.clone();
                    let cancel2 = cancel.clone();
                    let opts = options.clone();
                    let mut sender = reporter.sync_sender();
                    let bd = bytes_done;
                    let id = items_done;

                    match tokio::task::spawn_blocking(move || {
                        let mut bd_local = bd;
                        let result = copy_file_chunked(
                            &source, &dest, &cancel2, &mut sender, &mut bd_local, id, &opts,
                        );
                        (bd_local, result)
                    })
                    .await
                    {
                        Ok((bd_back, result)) => {
                            bytes_done = bd_back;
                            result.map_err(crate::Error::from)
                        }
                        Err(e) => Err(e.into()),
                    }
                }
            };

            if let Err(e) = result {
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
                    IssueOutcome::Skip => {}
                    IssueOutcome::Retry => {
                        retry = true;
                    }
                }
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(bytes_done, items_done, "");
    Ok(())
}

// --- Execute Delete (async outer loop) ---

async fn execute_delete(
    reporter: &mut ProgressReporter,
    paths: Vec<PathBuf>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    let total_items = paths.len() as u64;
    reporter.send_prepared(0, total_items);

    let mut items_done = 0u64;

    for path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }

        let display = path.display().to_string();
        reporter.maybe_send_progress(0, items_done, &display);

        let mut retry = true;
        while retry {
            retry = false;

            let p = path.clone();
            let result = tokio::task::spawn_blocking(move || {
                if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                }
            })
            .await;

            let result = match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(crate::Error::from(e)),
                Err(e) => Err(crate::Error::from(e)),
            };

            if let Err(e) = result {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error deleting {}", path.display()),
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

// --- Execute Move (async) ---

async fn execute_move(
    reporter: &mut ProgressReporter,
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: CopyOptions,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    let mut needs_copy = Vec::new();

    for source in &sources {
        if cancel.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }

        let file_name = match source.file_name() {
            Some(f) => f,
            None => {
                return Err(crate::Error::Custom(
                    "source has no file name".to_string(),
                ))
            }
        };
        let dest_path = destination.join(file_name);

        let src = source.clone();
        let dst = dest_path.clone();
        let result = tokio::task::spawn_blocking(move || std::fs::rename(&src, &dst)).await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.raw_os_error() == Some(libc::EXDEV) => {
                needs_copy.push(source.clone());
            }
            Ok(Err(e)) => {
                let io_kind = issue_kind_from_io_error(&e);
                match reporter
                    .raise_issue(
                        io_kind,
                        format!("Error moving {}: {}", source.display(), e),
                        Some(format!("{} -> {}", source.display(), dest_path.display())),
                        vec![IssueAction::Skip, IssueAction::Retry],
                    )
                    .await
                {
                    Ok(IssueAction::Skip) => {}
                    Err(e) => return Err(e),
                    Ok(IssueAction::Retry) => {
                        // TODO: implement retry for rename
                    }
                    _ => unreachable!("not offered"),
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    if needs_copy.is_empty() {
        reporter.send_prepared(0, sources.len() as u64);
        return Ok(());
    }

    // Fall back to copy+delete for cross-device moves
    execute_copy(reporter, needs_copy.clone(), destination, options, cancel).await?;

    // Delete originals after successful copy
    match tokio::task::spawn_blocking(move || -> Result<(), crate::Error> {
        for path in &needs_copy {
            if path.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    })
    .await
    {
        Ok(r) => r,
        Err(e) => Err(e.into()),
    }
}
