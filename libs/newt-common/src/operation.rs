use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::{debug, info, warn};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::filesystem::File;
use crate::proc::NoConsoleWindow;
use crate::rpc::Communicator;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{VFS_READ_CHUNK_SIZE, Vfs, VfsDescriptor, VfsPath, VfsRegistry};

pub type OperationId = u64;
pub type IssueId = u64;

// --- Issue Resolution Types ---

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash, specta::Type)]
pub enum IssueKind {
    AlreadyExists,
    PermissionDenied,
    IoError,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum IssueAction {
    Skip,
    Overwrite,
    Retry,
}

#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct OperationIssue {
    pub issue_id: IssueId,
    pub kind: IssueKind,
    pub message: String,
    pub detail: Option<String>,
    pub actions: Vec<IssueAction>,
}

#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct IssueResponse {
    pub action: IssueAction,
    pub apply_to_all: bool,
}

#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct ResolveIssueRequest {
    pub operation_id: OperationId,
    pub issue_id: IssueId,
    pub response: IssueResponse,
}

// --- Copy Options ---

#[derive(Debug, Serialize, Deserialize, Default, Clone, specta::Type)]
pub struct CopyOptions {
    pub preserve_timestamps: bool,
    pub preserve_owner: bool,
    pub preserve_group: bool,
    pub create_symlink: bool,
}

// --- Archive Options ---

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarXz,
    TarZst,
}

impl ArchiveFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::TarXz => "tar.xz",
            ArchiveFormat::TarZst => "tar.zst",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, specta::Type)]
pub struct ArchiveOptions {
    pub format: ArchiveFormat,
    /// `None` = per-format default (gzip/xz/deflate 6, zstd 3); zip 0 = store.
    pub level: Option<i32>,
    /// Store symlinks as symlink entries; off = follow them into the archive.
    pub preserve_symlinks: bool,
    /// Zip only — WinZip AES-256 encryption.
    pub password: Option<String>,
}

// `execute_operation` logs the whole request with `{:?}` — keep the password
// out of the logs.
impl std::fmt::Debug for ArchiveOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveOptions")
            .field("format", &self.format)
            .field("level", &self.level)
            .field("preserve_symlinks", &self.preserve_symlinks)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

// --- Operation Request ---

#[derive(Debug, Serialize, Deserialize, specta::Type)]
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
    /// Give `source` a new leaf name in its parent. Uses native
    /// `Vfs::rename` when available, else copy+delete (so S3 objects and
    /// prefixes can be "renamed" via server-side CopyObject).
    Rename { source: VfsPath, new_name: String },
    Delete {
        paths: Vec<VfsPath>,
        /// Move to the OS trash (`Vfs::trash_item`) instead of deleting.
        #[serde(default)]
        to_trash: bool,
    },
    CreateArchive {
        sources: Vec<VfsPath>,
        /// Full path of the archive file itself, not its directory.
        destination: VfsPath,
        options: ArchiveOptions,
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
    /// Apply a property-sheet patch (`Vfs::apply_properties`) to each
    /// path; `recursive` walks directories/prefixes like `SetMetadata`.
    ApplyProperties {
        paths: Vec<VfsPath>,
        patch: crate::vfs::PropertyPatch,
        recursive: bool,
    },
    RunCommand {
        command: String,
        /// VFS path, not `std::path` — crosses RPC; the executor (the
        /// agent in a remote session) converts to native in its own OS.
        working_dir: Option<crate::vfs::path::PathBuf>,
    },
    /// Synthetic long-running operation for manual testing of the progress
    /// UI — scan phase, prepared totals, ticking progress, and completion.
    /// Exposed only from the Debug modal in debug builds; kept here
    /// unconditionally so the wire format stays identical across debug
    /// and release builds.
    DebugSleep { duration_seconds: u64 },
}

#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct StartOperationRequest {
    pub id: OperationId,
    pub request: OperationRequest,
}

// --- Progress ---

#[derive(Debug, Serialize, Deserialize, specta::Type)]
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
    Symlink { target: String },
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

/// Minimum interval between progress/scanning notifications. The host
/// throttles UI updates anyway; sending more is wasted work.
const PROGRESS_THROTTLE: std::time::Duration = std::time::Duration::from_millis(100);

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
        if now.duration_since(*last) >= PROGRESS_THROTTLE {
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
        if now.duration_since(*last) >= PROGRESS_THROTTLE {
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
        if let Some(&action) = self.sticky_resolutions.get(&kind) {
            return Ok(action);
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
                            self.sticky_resolutions.insert(kind, response.action);
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
    working_dir: Option<&crate::vfs::path::Path>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    reporter.send_prepared(0, 0);
    reporter.maybe_send_progress(0, 0, command);

    let mut child = {
        let (shell, shell_args) = crate::shell::run_via_shell(command);
        let mut cmd = tokio::process::Command::new(shell);
        cmd.no_console_window();
        cmd.args(shell_args);
        if let Some(dir) = working_dir {
            // Native conversion happens here — the executor runs where
            // the FS is (the agent in a remote session). `launch_cwd`
            // (not `to_native`) so cmd.exe accepts a local directory.
            cmd.current_dir(crate::vfs::local::launch_cwd(dir));
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

// --- Debug sleep (manual-testing fixture) ---

async fn execute_debug_sleep(
    reporter: &mut ProgressReporter,
    duration_seconds: u64,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    // Synthetic numbers chosen so the progress bar visibly moves and bytes-
    // per-second readouts land in a familiar range.
    const TOTAL_ITEMS: u64 = 1_000;
    const BYTES_PER_ITEM: u64 = 1024 * 1024;
    let total_bytes = TOTAL_ITEMS * BYTES_PER_ITEM;

    // Split the budget: ~15% scanning, the rest doing "work".
    let scan_ms = (duration_seconds * 1000 * 15) / 100;
    let work_ms = duration_seconds * 1000 - scan_ms;

    // Scan phase — ramp items_found / bytes_found up to the totals.
    let scan_ticks: u64 = 50;
    let scan_tick_ms = scan_ms.max(1) / scan_ticks.max(1);
    for i in 1..=scan_ticks {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        let items_found = TOTAL_ITEMS * i / scan_ticks;
        let bytes_found = total_bytes * i / scan_ticks;
        reporter.maybe_send_scanning(items_found, bytes_found);
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(scan_tick_ms)) => {}
            _ = cancel.cancelled() => return Err(crate::Error::cancelled()),
        }
    }

    reporter.send_prepared(total_bytes, TOTAL_ITEMS);

    // Work phase — tick once per simulated item. Raise a synthetic
    // AlreadyExists conflict at four points so the issue-resolution UI
    // and the apply-to-all/sticky-resolution path can be exercised.
    let conflict_at: [u64; 4] = [
        TOTAL_ITEMS / 5,
        2 * TOTAL_ITEMS / 5,
        3 * TOTAL_ITEMS / 5,
        4 * TOTAL_ITEMS / 5,
    ];
    let work_tick_ms = work_ms.max(1) / TOTAL_ITEMS.max(1);
    let mut bytes_done = 0u64;
    for i in 1..=TOTAL_ITEMS {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        if conflict_at.contains(&i) {
            match reporter
                .raise_issue(
                    IssueKind::AlreadyExists,
                    format!("synthetic item {} already exists at destination", i),
                    Some(format!(
                        "(debug fixture) tick #{} of {} — pick Skip/Overwrite/Retry; tick \"apply to all\" to make the remaining synthetic conflicts resolve automatically",
                        conflict_at.iter().position(|&n| n == i).unwrap() + 1,
                        conflict_at.len(),
                    )),
                    vec![
                        IssueAction::Skip,
                        IssueAction::Overwrite,
                        IssueAction::Retry,
                    ],
                )
                .await
            {
                Ok(_) => {}
                Err(e) => return Err(e),
            }
        }

        bytes_done += BYTES_PER_ITEM;
        let display = format!("synthetic item {} of {}", i, TOTAL_ITEMS);
        reporter.maybe_send_progress(bytes_done, i, &display);
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(work_tick_ms)) => {}
            _ = cancel.cancelled() => return Err(crate::Error::cancelled()),
        }
    }

    Ok(())
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
        OperationRequest::Delete { paths, to_trash } => {
            if to_trash {
                execute_trash(&mut reporter, &context, paths, cancel.clone()).await
            } else {
                execute_delete(&mut reporter, &context, paths, cancel.clone()).await
            }
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
                None,
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
        OperationRequest::Rename { source, new_name } => {
            execute_rename(&mut reporter, &context, source, new_name, cancel.clone()).await
        }
        OperationRequest::CreateArchive {
            sources,
            destination,
            options,
        } => {
            execute_create_archive(
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
        OperationRequest::ApplyProperties {
            paths,
            patch,
            recursive,
        } => {
            execute_apply_properties(
                &mut reporter,
                &context,
                paths,
                patch,
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
        OperationRequest::DebugSleep { duration_seconds } => {
            execute_debug_sleep(&mut reporter, duration_seconds, cancel.clone()).await
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

// --- Shared source-tree walk (copy and archive planning) ---

enum WalkedKind {
    File,
    Directory,
    Symlink { target: String },
}

struct WalkedEntry {
    pub source: PathBuf,
    /// Path relative to the selection: the top-level source's file name, then
    /// dirent names down the tree, `/`-joined.
    pub rel: String,
    pub kind: WalkedKind,
    /// The dirent as seen during the walk — carries the metadata (mode,
    /// owner, mtime, size) so consumers don't need a second stat pass.
    pub file: File,
}

#[derive(Default)]
struct WalkOptions {
    /// Classify through symlinks (archive "follow" mode): symlinks to
    /// directories are recursed into, symlinks to files become plain files.
    /// Cycles among followed targets are detected and skipped.
    pub follow_symlinks: bool,
    /// Path on the source VFS to silently omit — the archive being written,
    /// so it doesn't pack itself.
    pub exclude: Option<PathBuf>,
}

/// Longest chain of dir-symlinks the walk will follow before assuming a
/// cycle it failed to detect structurally (mirrors the archive-read side's
/// `MAX_SYMLINK_HOPS`).
const MAX_FOLLOWED_LINKS: usize = 40;

/// Resolve a raw symlink target against the directory containing the link.
/// Best-effort textual normalization — the VFS surface has no realpath.
fn resolve_symlink_target(parent: &Path, target: &str) -> PathBuf {
    let mut path = if target.starts_with('/') {
        PathBuf::root()
    } else {
        parent.to_owned()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                path = path
                    .parent()
                    .map(|p| p.to_owned())
                    .unwrap_or_else(PathBuf::root);
            }
            seg => path.push(seg),
        }
    }
    path
}

async fn walk_sources(
    src_vfs: &dyn Vfs,
    src_descriptor: &dyn VfsDescriptor,
    sources: &[PathBuf],
    options: &WalkOptions,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<(Vec<WalkedEntry>, u64), crate::Error> {
    struct DirFrame {
        src: PathBuf,
        rel: String,
        /// Normalized targets of the dir-symlinks followed to reach here.
        link_ancestry: Arc<Vec<PathBuf>>,
    }

    let mut entries: Vec<WalkedEntry> = Vec::new();
    let mut total_bytes = 0u64;
    let has_symlinks = src_descriptor.has_symlinks();
    let follow = options.follow_symlinks;

    for source in sources {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let file_name = source
            .file_name()
            .ok_or_else(|| crate::Error::custom("source has no file name".to_string()))?
            .to_string();

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
                .files
                .into_iter()
                .find(|f| f.name == file_name)
                .ok_or_else(|| crate::Error::custom(format!("source not found: {}", source)))?
        };

        let mut stack: Vec<DirFrame> = Vec::new();
        // The top-level source enters classification as a pseudo-child;
        // directory listings feed the same queue below.
        let mut pending: Vec<(PathBuf, String, File, Arc<Vec<PathBuf>>)> =
            vec![(source.clone(), file_name, file_entry, Arc::new(Vec::new()))];

        loop {
            for (src_path, rel, file, link_ancestry) in pending.drain(..) {
                if options.exclude.as_ref() == Some(&src_path) {
                    continue;
                }

                if has_symlinks && file.is_symlink && !follow {
                    entries.push(WalkedEntry {
                        source: src_path,
                        rel,
                        kind: WalkedKind::Symlink {
                            target: file.symlink_target.clone().unwrap_or_default(),
                        },
                        file,
                    });
                } else if has_symlinks && file.is_symlink && follow && file.is_dir {
                    let parent = src_path
                        .parent()
                        .map(|p| p.to_owned())
                        .unwrap_or_else(PathBuf::root);
                    let target = resolve_symlink_target(
                        &parent,
                        file.symlink_target.as_deref().unwrap_or_default(),
                    );
                    // A target that is itself on the followed chain, or an
                    // ancestor of the link, recurses forever.
                    let cycle = link_ancestry.contains(&target) || src_path.starts_with(&target);
                    if cycle || link_ancestry.len() >= MAX_FOLLOWED_LINKS {
                        reporter
                            .raise_issue(
                                IssueKind::Other("SymlinkCycle".to_string()),
                                format!("Symlink cycle at {}", src_path),
                                Some(format!("target: {}", target)),
                                vec![IssueAction::Skip],
                            )
                            .await?;
                        continue;
                    }
                    let mut ancestry = (*link_ancestry).clone();
                    ancestry.push(target.clone());
                    entries.push(WalkedEntry {
                        source: src_path,
                        rel: rel.clone(),
                        kind: WalkedKind::Directory,
                        file,
                    });
                    // Recurse into the resolved target: identical on a real
                    // FS, and keeps the frame path physical for VFSes that
                    // don't resolve links on access.
                    stack.push(DirFrame {
                        src: target,
                        rel,
                        link_ancestry: Arc::new(ancestry),
                    });
                } else if has_symlinks && file.is_symlink && follow {
                    // Followed file symlink: the dirent's size is the link's
                    // own length — stat the target for the real size (drives
                    // progress totals and the zip writer's zip64 decision).
                    let parent = src_path
                        .parent()
                        .map(|p| p.to_owned())
                        .unwrap_or_else(PathBuf::root);
                    let target = resolve_symlink_target(
                        &parent,
                        file.symlink_target.as_deref().unwrap_or_default(),
                    );
                    let resolved = src_vfs.file_info(&target).await.unwrap_or(file);
                    total_bytes += resolved.size.unwrap_or(0);
                    entries.push(WalkedEntry {
                        source: target,
                        rel,
                        kind: WalkedKind::File,
                        file: resolved,
                    });
                } else if file.is_dir {
                    entries.push(WalkedEntry {
                        source: src_path.clone(),
                        rel: rel.clone(),
                        kind: WalkedKind::Directory,
                        file,
                    });
                    stack.push(DirFrame {
                        src: src_path,
                        rel,
                        link_ancestry,
                    });
                } else {
                    total_bytes += file.size.unwrap_or(0);
                    entries.push(WalkedEntry {
                        source: src_path,
                        rel,
                        kind: WalkedKind::File,
                        file,
                    });
                }
            }

            let Some(frame) = stack.pop() else { break };
            if cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }

            let file_list = loop {
                match cancellable(cancel, src_vfs.list_files(&frame.src, None)).await {
                    Ok(list) => break list,
                    Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                    Err(e) => {
                        match reporter
                            .handle_io_error(
                                e,
                                &format!("Error scanning directory {}", frame.src),
                                None,
                                cancel,
                                true,
                            )
                            .await?
                        {
                            IssueOutcome::Skip => break crate::vfs::VfsFileList::default(),
                            IssueOutcome::Retry => continue,
                        }
                    }
                }
            };

            for file in file_list.files {
                if file.name == ".." {
                    continue;
                }
                let src_path = frame.src.join(&file.name);
                let rel = format!("{}/{}", frame.rel, file.name);
                pending.push((src_path, rel, file, frame.link_ancestry.clone()));
            }
            reporter.maybe_send_scanning(entries.len() as u64, total_bytes);
        }
    }

    Ok((entries, total_bytes))
}

// --- Plan copy (async, uses Vfs) ---

async fn plan_copy(
    src_vfs: &dyn Vfs,
    src_descriptor: &dyn VfsDescriptor,
    sources: &[PathBuf],
    destination: &Path,
    rename_to: Option<&str>,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<CopyPlan, crate::Error> {
    let (walked, total_bytes) = walk_sources(
        src_vfs,
        src_descriptor,
        sources,
        &WalkOptions::default(),
        reporter,
        cancel,
    )
    .await?;

    let entries = walked
        .into_iter()
        .map(|w| {
            // `rel` leads with the top-level source's file name; a rename
            // lands under a different leaf name in the destination.
            let rel = match rename_to {
                Some(new_name) => match w.rel.split_once('/') {
                    Some((_, rest)) => format!("{}/{}", new_name, rest),
                    None => new_name.to_string(),
                },
                None => w.rel,
            };
            CopyEntry {
                dest: destination.join(&rel),
                size_bytes: match w.kind {
                    WalkedKind::File => w.file.size.unwrap_or(0),
                    _ => 0,
                },
                kind: match w.kind {
                    WalkedKind::File => CopyEntryKind::File,
                    WalkedKind::Directory => CopyEntryKind::Directory,
                    WalkedKind::Symlink { target } => CopyEntryKind::Symlink { target },
                },
                source: w.source,
            }
        })
        .collect::<Vec<_>>();

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

// --- Sync-reader bridge (spawn_blocking + bounded channel) ---

/// Bridge a sync reader into an async chunk stream. The bounded channel
/// provides backpressure; the blocking task ends on EOF, error, or cancel.
pub(crate) fn bridge_sync_reader(
    mut reader: Box<dyn std::io::Read + Send>,
    cancel: CancellationToken,
) -> tokio::sync::mpsc::Receiver<Result<Vec<u8>, crate::Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, crate::Error>>(4);
    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
        loop {
            if cancel.is_cancelled() {
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
    rx
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
    let mut buf = [0u8; VFS_READ_CHUNK_SIZE];

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

    let mut buf = [0u8; VFS_READ_CHUNK_SIZE];

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
        debug!("copy_single_file: trying copy_within for {}", entry.source);
        match src_vfs.copy_within(&entry.source, &entry.dest).await {
            Ok(()) => {
                *bytes_done += entry.size_bytes;
                return preserve_metadata(src_vfs, &entry.source, dst_vfs, &entry.dest, options)
                    .await;
            }
            // The descriptor can't see per-call quirks (a RootVfs spans
            // many real filesystems; server-side copies have size caps),
            // so "unsupported" is only known at call time — fall through
            // to the streaming strategies. Real failures surface as
            // issues instead of silently downgrading to a full re-stream.
            Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                debug!(
                    "copy_single_file: copy_within unsupported for {}: {}",
                    entry.source, e
                );
            }
            Err(e) => return Err(e),
        }
    }

    // 2. Sync read + sync write
    if src_descriptor.can_read_sync() && dst_descriptor.can_overwrite_sync() {
        debug!(
            "copy_single_file: sync-read + sync-write for {}",
            entry.source
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
            entry.source
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
            entry.source
        );
        let sync_reader = src_vfs.open_read_sync(&entry.source).await?;
        let mut writer = dst_vfs.overwrite_async(&entry.dest).await?;

        let mut rx = bridge_sync_reader(sync_reader, cancel.clone());
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
            entry.source
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
        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
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
    if !dst_vfs.descriptor().can_set_metadata() {
        return Ok(());
    }

    // Permissions are always preserved; timestamps/owner/group only if requested.
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
    rename_to: Option<&str>,
) -> Result<(), crate::Error> {
    debug_assert!(
        rename_to.is_none() || sources.len() == 1,
        "rename_to requires exactly one source"
    );
    // Follow any redirect_target hooks (e.g. flat search results) so the
    // copy operates on the underlying real files, not on the synthetic
    // SearchVfs paths the user clicked.
    let mut sources = sources;
    for s in sources.iter_mut() {
        *s = context.registry.dereference(s).await;
    }
    let first_source = sources
        .first()
        .ok_or_else(|| crate::Error::custom("no sources provided"))?;
    let (src_vfs, _) = context.registry.resolve(first_source)?;
    let (dst_vfs, dst_path) = context.registry.resolve(&destination)?;

    let src_vfs_id = first_source.vfs_id;
    let dst_vfs_id = destination.vfs_id;
    let same_vfs = src_vfs_id == dst_vfs_id;

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
        let file_name = match sources[0].file_name() {
            Some(f) => f,
            None => return Err(crate::Error::custom("source has no file name".to_string())),
        };
        let dest = dst_path.join(file_name);
        reporter.send_prepared(0, 1);
        dst_vfs.create_symlink(&dest, source.as_wire_str()).await?;
        return Ok(());
    }

    let plan = plan_copy(
        &*src_vfs,
        src_descriptor,
        &source_paths,
        &dst_path,
        rename_to,
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
            .map(str::to_string)
            .unwrap_or_else(|| entry.dest.as_wire_str().to_string());
        reporter.maybe_send_progress(bytes_done, items_done, &display);

        let dest_file = dst_vfs.file_info(&entry.dest).await;
        if let Ok(dest_file) = dest_file {
            match &entry.kind {
                CopyEntryKind::Directory => {
                    if dest_file.is_dir {
                        // Directory already exists — merge, skip mkdir.
                        items_done += 1;
                        continue;
                    } else {
                        // Type mismatch
                        match reporter
                            .raise_issue(
                                IssueKind::AlreadyExists,
                                format!("Cannot replace file with directory: {}", entry.dest),
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
                                format!("Cannot replace directory with file: {}", entry.dest),
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
                                format!("File already exists: {}", entry.dest),
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
                        dst_vfs.create_symlink(&entry.dest, target.as_str()).await
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
                            Some(format!("{} -> {}", entry.source, entry.dest)),
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
                            &format!("Error removing source {}", entry.source),
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
                                    &format!("Error removing source directory {}", entry.source),
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

// --- Execute CreateArchive (async pack loop, streams via archive_pack) ---

async fn execute_create_archive(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    options: ArchiveOptions,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    use crate::archive_pack::{ArchiveSink, ArchiveWriter};

    // Follow any redirect_target hooks (e.g. flat search results), same as copy.
    let mut sources = sources;
    for s in sources.iter_mut() {
        *s = context.registry.dereference(s).await;
    }
    let first_source = sources
        .first()
        .ok_or_else(|| crate::Error::custom("no sources provided"))?;
    let (src_vfs, _) = context.registry.resolve(first_source)?;
    let (dst_vfs, dst_path) = context.registry.resolve(&destination)?;

    let src_vfs_id = first_source.vfs_id;
    if let Some(mismatched) = sources.iter().find(|s| s.vfs_id != src_vfs_id) {
        return Err(crate::Error::custom(format!(
            "all sources must be on the same VFS (expected {}, got {})",
            src_vfs_id, mismatched.vfs_id
        )));
    }
    if options.password.is_some() && options.format != ArchiveFormat::Zip {
        return Err(crate::Error::custom(
            "password protection is only supported for zip archives",
        ));
    }

    let src_descriptor = src_vfs.descriptor();
    debug!(
        "execute_create_archive: {} sources, src_vfs={} ({}), dst={} on vfs {} ({})",
        sources.len(),
        src_vfs_id,
        src_descriptor.type_name(),
        dst_path,
        destination.vfs_id,
        dst_vfs.descriptor().type_name(),
    );

    // Destination conflict before any work. The archive is a single artifact,
    // so declining to overwrite simply cancels the operation.
    if let Ok(existing) = dst_vfs.file_info(&dst_path).await {
        if existing.is_dir {
            return Err(crate::Error::custom(format!(
                "destination is a directory: {}",
                dst_path
            )));
        }
        match reporter
            .raise_issue(
                IssueKind::AlreadyExists,
                format!("File already exists: {}", dst_path),
                None,
                vec![IssueAction::Skip, IssueAction::Overwrite],
            )
            .await?
        {
            IssueAction::Overwrite => {}
            _ => {
                // Skipping the only artifact means nothing to do — surface
                // as a cancellation, not a failure.
                cancel.cancel();
                return Err(crate::Error::cancelled());
            }
        }
    }

    let source_paths: Vec<PathBuf> = sources.iter().map(|s| s.path.clone()).collect();
    let walk_options = WalkOptions {
        follow_symlinks: !options.preserve_symlinks,
        // Keep a same-VFS destination out of the walk, or the archive
        // would pack its growing self.
        exclude: (src_vfs_id == destination.vfs_id).then(|| dst_path.to_owned()),
    };
    let (walked, total_bytes) = walk_sources(
        &*src_vfs,
        src_descriptor,
        &source_paths,
        &walk_options,
        reporter,
        &cancel,
    )
    .await?;

    // Duplicate top-level names would silently collide inside the archive.
    let mut top_level = std::collections::HashSet::new();
    for entry in &walked {
        if !entry.rel.contains('/') && !top_level.insert(entry.rel.as_str()) {
            return Err(crate::Error::custom(format!(
                "duplicate top-level name in selection: {}",
                entry.rel
            )));
        }
    }

    reporter.send_prepared(total_bytes, walked.len() as u64);

    let writer = ArchiveWriter::new(&options)?;
    let mut sink = ArchiveSink::open(&*dst_vfs, &dst_path).await?;

    let result = match pack_entries(reporter, &*src_vfs, writer, &mut sink, &walked, &cancel).await
    {
        Ok(()) => sink.finish().await,
        Err(e) => {
            drop(sink);
            Err(e)
        }
    };
    if let Err(e) = result {
        // Append-only stream — a failed or cancelled archive can't be
        // salvaged; best-effort cleanup of the partial artifact.
        let _ = dst_vfs.remove_file(&dst_path).await;
        return Err(e);
    }
    Ok(())
}

async fn pack_entries(
    reporter: &mut ProgressReporter,
    src_vfs: &dyn Vfs,
    mut writer: crate::archive_pack::ArchiveWriter,
    sink: &mut crate::archive_pack::ArchiveSink,
    entries: &[WalkedEntry],
    cancel: &CancellationToken,
) -> Result<(), crate::Error> {
    use crate::archive_pack::SourceReader;

    let mut buf = Vec::new();
    let mut bytes_done = 0u64;
    let mut items_done = 0u64;

    for entry in entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        reporter.maybe_send_progress(bytes_done, items_done, &entry.rel);

        match &entry.kind {
            WalkedKind::Directory => {
                writer.add_directory(&entry.rel, &entry.file, &mut buf)?;
                sink.write_all(std::mem::take(&mut buf)).await?;
            }
            WalkedKind::Symlink { target } => {
                writer.add_symlink(&entry.rel, target, &entry.file, &mut buf)?;
                sink.write_all(std::mem::take(&mut buf)).await?;
            }
            WalkedKind::File => {
                let scanned_size = entry.file.size;
                let entry_start = bytes_done;

                // Open the source before the entry header is committed to the
                // stream — an open failure can still Skip/Retry cleanly.
                let reader = loop {
                    match SourceReader::open(src_vfs, &entry.source, cancel).await {
                        Ok(reader) => break Some(reader),
                        Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                        Err(e) => {
                            match reporter
                                .handle_io_error(
                                    e,
                                    "Error",
                                    Some(entry.source.as_wire_str().to_string()),
                                    cancel,
                                    true,
                                )
                                .await?
                            {
                                IssueOutcome::Skip => break None,
                                IssueOutcome::Retry => continue,
                            }
                        }
                    }
                };
                let Some(mut reader) = reader else {
                    bytes_done = entry_start + scanned_size.unwrap_or(0);
                    items_done += 1;
                    continue;
                };

                writer.begin_file(&entry.rel, scanned_size, &entry.file, &mut buf)?;
                sink.write_all(std::mem::take(&mut buf)).await?;

                let mut read_error = None;
                loop {
                    if cancel.is_cancelled() {
                        return Err(crate::Error::cancelled());
                    }
                    match reader.next().await {
                        Ok(Some(chunk)) => {
                            let accepted = writer.write_data(&chunk, &mut buf)?;
                            sink.write_all(std::mem::take(&mut buf)).await?;
                            bytes_done += chunk.len() as u64;
                            reporter.maybe_send_progress(bytes_done, items_done, &entry.rel);
                            if accepted < chunk.len() {
                                warn!(
                                    "archive entry {} truncated: source grew past its scanned size",
                                    entry.rel
                                );
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                        Err(e) => {
                            read_error = Some(e);
                            break;
                        }
                    }
                }

                let padded = writer.end_file(&mut buf)?;
                sink.write_all(std::mem::take(&mut buf)).await?;
                if padded > 0 && read_error.is_none() {
                    warn!(
                        "archive entry {} zero-padded: source shrank below its scanned size",
                        entry.rel
                    );
                }
                if let Some(e) = read_error {
                    // The entry header is already on the append-only stream;
                    // the entry was finalized as truncated/padded, so a retry
                    // is structurally impossible — only Skip is offered.
                    reporter
                        .handle_io_error(
                            e,
                            &format!(
                                "Error reading {} (stored truncated in the archive)",
                                entry.rel
                            ),
                            Some(entry.source.as_wire_str().to_string()),
                            cancel,
                            false,
                        )
                        .await?;
                }
                // Snap to the scanned contribution so skips/shrinks still
                // drive the bar to 100% (mirrors execute_copy's accounting).
                bytes_done = bytes_done.max(entry_start + scanned_size.unwrap_or(0));
            }
        }
        items_done += 1;
    }

    writer.finish(&mut buf)?;
    sink.write_all(std::mem::take(&mut buf)).await?;
    reporter.maybe_send_progress(bytes_done, items_done, "");
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

    let root = PathBuf::root();
    let parent = path.parent().unwrap_or(&root);
    let file_name = path.file_name();
    match file_name {
        Some(name) => {
            let listing = cancellable(cancel, vfs.list_files(parent, None)).await?;
            Ok(listing
                .files
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
    let mut stack = vec![path.to_owned()];

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
                            &format!("Error scanning directory {}", dir),
                            None,
                            cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => break crate::vfs::VfsFileList::default(),
                        IssueOutcome::Retry => continue,
                    }
                }
            }
        };

        for file in &file_list.files {
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

/// Walk a directory tree and collect every entry (root included) as
/// `(path, is_dir)`, for per-item recursive apply (chmod, properties).
async fn collect_chmod_entries(
    vfs: &dyn Vfs,
    path: &Path,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<(PathBuf, bool)>, crate::Error> {
    let mut entries = vec![(path.to_owned(), true)];
    let mut stack = vec![path.to_owned()];

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
                            &format!("Error scanning directory {}", dir),
                            None,
                            cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => break crate::vfs::VfsFileList::default(),
                        IssueOutcome::Retry => continue,
                    }
                }
            }
        };

        for file in &file_list.files {
            if file.name == ".." {
                continue;
            }
            let entry_path = dir.join(&file.name);
            let is_dir = file.is_dir && !file.is_symlink;
            if is_dir {
                stack.push(entry_path.clone());
            }
            entries.push((entry_path, is_dir));
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

    // Follow redirect_target so chmod from a SearchVfs hits the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

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
                for (entry, _) in entries {
                    let display = format!("{}:{}", vfs_path.vfs_id, entry);
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

async fn execute_apply_properties(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    patch: crate::vfs::PropertyPatch,
    recursive: bool,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!(
        "execute_apply_properties: {} paths, {} ops, recursive={}",
        paths.len(),
        patch.ops.len(),
        recursive
    );

    if patch.is_empty() {
        return Ok(());
    }

    // Follow redirect_target so applies from a SearchVfs hit the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

    let mut all_entries: Vec<(Arc<dyn Vfs>, PathBuf, String)> = Vec::new();

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;
        let descriptor = vfs.descriptor();
        // On VFSes that can't stat directories (S3), listed "directories"
        // are synthetic prefixes, not objects — nothing to apply to.
        let include_dirs = descriptor.can_stat_directories();

        if recursive {
            let is_dir = probe_is_dir(&*vfs, descriptor, &local_path, &cancel).await?;
            if is_dir {
                let entries = collect_chmod_entries(&*vfs, &local_path, reporter, &cancel).await?;
                for (entry, entry_is_dir) in entries {
                    if entry_is_dir && !include_dirs {
                        continue;
                    }
                    let display = format!("{}:{}", vfs_path.vfs_id, entry);
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

    let mut items_done = 0u64;

    for (vfs, local_path, display) in &all_entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        reporter.maybe_send_progress(0, items_done, display);

        let mut retry = true;
        while retry {
            retry = false;

            if let Err(e) = vfs.apply_properties(local_path, &patch).await {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error applying properties to {}", display),
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

    // Follow redirect_target so deletes from a SearchVfs hit the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

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
            // Fast path: single atomic removal, counts as 1 item.
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

        let display = entry.path.to_string();
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
                        &format!("Error deleting {}", entry.path),
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

async fn execute_trash(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!("execute_trash: {} paths", paths.len());

    // Follow redirect_target so trashing from a SearchVfs hits the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

    // No scan phase: each top-level item is trashed wholesale and counts
    // as one item, like the remove_tree fast path.
    reporter.send_prepared(0, paths.len() as u64);

    let mut items_done = 0u64;

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;

        let display = local_path.to_string();
        reporter.maybe_send_progress(0, items_done, &display);

        let mut retry = true;
        while retry {
            retry = false;

            let result = if vfs.descriptor().can_trash() {
                vfs.trash_item(&local_path).await
            } else {
                Err(crate::Error::not_supported())
            };

            if let Err(e) = result {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error moving {} to Trash", local_path),
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
    // Follow redirect_target so moves from a SearchVfs operate on real files.
    let mut sources = sources;
    for s in sources.iter_mut() {
        *s = context.registry.dereference(s).await;
    }
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

            let file_name = match source.file_name() {
                Some(f) => f,
                None => return Err(crate::Error::custom("source has no file name".to_string())),
            };
            let dest_local = dst_path.join(file_name);
            let source_local = source.path.clone();
            let mut overwrite_approved = false;

            // Check for destination conflicts before renaming (rename silently overwrites)
            if let Ok(dest_file) = src_vfs.file_info(&dest_local).await {
                let source_file = src_vfs.file_info(&source_local).await?;
                if dest_file.is_dir != source_file.is_dir {
                    // Type mismatch (file vs directory) — can only skip
                    let msg = if dest_file.is_dir {
                        format!("Cannot replace directory with file: {}", dest_local)
                    } else {
                        format!("Cannot replace file with directory: {}", dest_local)
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
                            format!("File already exists: {}", dest_local),
                            None,
                            vec![IssueAction::Skip, IssueAction::Overwrite],
                        )
                        .await
                    {
                        Ok(IssueAction::Skip) => continue,
                        Ok(IssueAction::Overwrite) => {
                            // Proceed with rename — an atomic replace on
                            // backends that support it (POSIX rename,
                            // posix-rename SFTP servers). Backends that
                            // refuse report AlreadyExists, handled below.
                            overwrite_approved = true;
                        }
                        Err(e) => return Err(e),
                        _ => unreachable!("not offered"),
                    }
                } else {
                    // Both are directories: merge — the copy machinery
                    // merges into an existing destination; rename can't.
                    needs_copy.push(source.clone());
                    continue;
                }
            }

            let mut retry = true;
            while retry {
                retry = false;
                match src_vfs.rename(&source_local, &dest_local).await {
                    Ok(()) => {
                        debug!("execute_move: renamed {} -> {}", source_local, dest_local);
                        renamed_count += 1;
                    }
                    // Only "rename not supported" — for the backend or for
                    // this particular pair (cross-device in a RootVfs) —
                    // falls back to copy+delete; real failures surface as
                    // issues rather than silently degrading.
                    Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                        debug!(
                            "execute_move: rename unsupported for {}, falling back to copy+delete",
                            source_local
                        );
                        needs_copy.push(source.clone());
                    }
                    // A backend whose rename won't replace an existing
                    // destination (SFTP servers without posix-rename):
                    // the user approved the overwrite, so clear the
                    // destination and retry once. Keyed on the approval —
                    // an unexpected AlreadyExists still surfaces below.
                    Err(e) if e.kind == crate::ErrorKind::AlreadyExists && overwrite_approved => {
                        overwrite_approved = false;
                        match src_vfs.remove_file(&dest_local).await {
                            Ok(()) => retry = true,
                            Err(e) => {
                                match reporter
                                    .handle_io_error(
                                        e,
                                        &format!("Error replacing {}", dest_local),
                                        None,
                                        &cancel,
                                        false,
                                    )
                                    .await?
                                {
                                    IssueOutcome::Skip => {}
                                    IssueOutcome::Retry => unreachable!("not offered"),
                                }
                            }
                        }
                    }
                    Err(e) => {
                        match reporter
                            .handle_io_error(
                                e,
                                &format!("Error renaming {}", source_local),
                                Some(format!("{} -> {}", source_local, dest_local)),
                                &cancel,
                                true,
                            )
                            .await?
                        {
                            IssueOutcome::Skip => {}
                            IssueOutcome::Retry => retry = true,
                        }
                    }
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
        None,
    )
    .await
}

// --- Execute Rename ---

async fn execute_rename(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    source: VfsPath,
    new_name: String,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    // Follow redirect_target so renames from a SearchVfs operate on the
    // real file.
    let source = context.registry.dereference(&source).await;
    let parent = source
        .parent()
        .ok_or_else(|| crate::Error::custom("cannot rename the VFS root"))?;
    let new_path = parent.join(&new_name);
    if new_path.path == source.path {
        reporter.send_prepared(0, 0);
        return Ok(());
    }

    let (vfs, _) = context.registry.resolve(&source)?;
    let descriptor = vfs.descriptor();

    if descriptor.can_rename() {
        // Check for destination conflicts before renaming (rename silently
        // overwrites) — same policy as the Move fast path.
        let mut attempt_rename = true;
        let mut overwrite_approved = false;
        if let Ok(dest_file) = vfs.file_info(&new_path.path).await {
            let source_file = vfs.file_info(&source.path).await?;
            if dest_file.is_dir != source_file.is_dir {
                let msg = if dest_file.is_dir {
                    format!("Cannot replace directory with file: {}", new_path.path)
                } else {
                    format!("Cannot replace file with directory: {}", new_path.path)
                };
                match reporter
                    .raise_issue(IssueKind::AlreadyExists, msg, None, vec![IssueAction::Skip])
                    .await
                {
                    Ok(IssueAction::Skip) => {
                        reporter.send_prepared(0, 0);
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                    _ => unreachable!("not offered"),
                }
            } else if !dest_file.is_dir {
                match reporter
                    .raise_issue(
                        IssueKind::AlreadyExists,
                        format!("File already exists: {}", new_path.path),
                        None,
                        vec![IssueAction::Skip, IssueAction::Overwrite],
                    )
                    .await
                {
                    Ok(IssueAction::Skip) => {
                        reporter.send_prepared(0, 0);
                        return Ok(());
                    }
                    Ok(IssueAction::Overwrite) => {
                        // Proceed with rename — an atomic replace on
                        // backends that support it (POSIX rename,
                        // posix-rename SFTP servers). Backends that refuse
                        // report AlreadyExists, handled below.
                        overwrite_approved = true;
                    }
                    Err(e) => return Err(e),
                    _ => unreachable!("not offered"),
                }
            } else {
                // Both are directories: merge — the copy machinery merges
                // into an existing destination; rename can't.
                attempt_rename = false;
            }
        }

        let mut retry = attempt_rename;
        while retry {
            retry = false;
            match vfs.rename(&source.path, &new_path.path).await {
                Ok(()) => {
                    debug!("execute_rename: renamed {} -> {}", source, new_path);
                    reporter.send_prepared(0, 1);
                    reporter.maybe_send_progress(0, 1, &new_name);
                    return Ok(());
                }
                // "Not supported" — for the backend or this particular pair
                // — falls back to copy+delete below; real failures surface
                // as issues.
                Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                    debug!(
                        "execute_rename: rename unsupported for {}, falling back to copy+delete",
                        source
                    );
                }
                // A backend whose rename won't replace an existing
                // destination (SFTP servers without posix-rename): the
                // user approved the overwrite, so clear the destination
                // and retry once.
                Err(e) if e.kind == crate::ErrorKind::AlreadyExists && overwrite_approved => {
                    overwrite_approved = false;
                    match vfs.remove_file(&new_path.path).await {
                        Ok(()) => retry = true,
                        Err(e) => {
                            match reporter
                                .handle_io_error(
                                    e,
                                    &format!("Error replacing {}", new_path.path),
                                    None,
                                    &cancel,
                                    false,
                                )
                                .await?
                            {
                                IssueOutcome::Skip => {
                                    reporter.send_prepared(0, 0);
                                    return Ok(());
                                }
                                IssueOutcome::Retry => unreachable!("not offered"),
                            }
                        }
                    }
                }
                Err(e) => {
                    match reporter
                        .handle_io_error(
                            e,
                            &format!("Error renaming {}", source),
                            Some(format!("{} -> {}", source, new_path)),
                            &cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => {
                            reporter.send_prepared(0, 0);
                            return Ok(());
                        }
                        IssueOutcome::Retry => retry = true,
                    }
                }
            }
        }
    }

    // No native rename (S3, k8s, …) or it failed: copy to the new name and
    // delete the source. Same-VFS copies take the copy_within fast path
    // (server-side CopyObject on S3), so no data flows through the app.
    // Timestamps are preserved where the VFS allows it — a rename should
    // not look like a fresh file.
    let options = CopyOptions {
        preserve_timestamps: true,
        ..CopyOptions::default()
    };
    execute_copy(
        reporter,
        context,
        vec![source],
        parent,
        options,
        cancel,
        true,
        0,
        Some(&new_name),
    )
    .await
}

#[cfg(test)]
#[path = "operation_tests.rs"]
mod tests;
