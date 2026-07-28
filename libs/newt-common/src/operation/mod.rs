use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::{debug, info, warn};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::proc::NoConsoleWindow;
use crate::rpc::Communicator;
use crate::vfs::File;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{VFS_READ_CHUNK_SIZE, Vfs, VfsDescriptor, VfsPath, VfsRegistry};

mod archive;
mod copy;
mod delete;
mod metadata;
mod move_rename;
mod progress;
mod run_command;
mod walk;

use archive::*;
use copy::*;
use delete::*;
use metadata::*;
use move_rename::*;
use progress::*;
use run_command::*;
use walk::*;

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
        /// Copy a single source under a different leaf name in
        /// `destination` (shell `cp src dest-that-does-not-exist`).
        #[serde(default)]
        rename_to: Option<String>,
    },
    Move {
        sources: Vec<VfsPath>,
        destination: VfsPath,
        #[serde(default)]
        options: CopyOptions,
        /// Move a single source under a different leaf name in
        /// `destination` (shell `mv src dest-that-does-not-exist`).
        #[serde(default)]
        rename_to: Option<String>,
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
    /// When present, `RunCommand` children get the `newt` CLI env/PATH, so
    /// operation-mode user commands can control the session too.
    pub shell_integration: Option<Arc<crate::shell_control::ShellIntegration>>,
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
            rename_to,
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
                rename_to.as_deref(),
            )
            .await
        }
        OperationRequest::Move {
            sources,
            destination,
            options,
            rename_to,
        } => {
            execute_move(
                &mut reporter,
                &context,
                sources,
                destination,
                options,
                cancel.clone(),
                rename_to.as_deref(),
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
                context.shell_integration.as_deref(),
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

#[cfg(test)]
mod tests;
