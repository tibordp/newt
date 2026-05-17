use std::collections::HashMap;
use std::io::Read;
// The archive index machinery is keyed by Unix-style relative path
// strings built on std paths; the `Vfs` surface speaks our
// `vfs::path::Path`. Convert at each trait-method boundary via
// `as_wire_str()` (leading `/` stripped by `normalize_dir_path`).
use std::path::Path as StdPath;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use log::info;
use tokio::io::AsyncRead;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode, UserGroup};
use crate::vfs::path::{Path, PathBuf};

use super::super::{
    Breadcrumb, DisplayPathMatch, RegisteredDescriptor, VFS_READ_CHUNK_SIZE, Vfs, VfsDescriptor,
    VfsPath,
};
use super::{
    DirectoryTree, SNAPSHOT_INTERVAL, archive_breadcrumbs, archive_format_path,
    archive_mount_label, archive_try_parse_display_path, build_directory_tree_from_iluvatar,
    detect_compression_from_name, index_get, index_path_str, mtime_to_i64, normalize_dir_path,
    normalized_to_string, not_found,
};

// ---------------------------------------------------------------------------
// TarArchiveVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct TarArchiveVfsDescriptor;

impl VfsDescriptor for TarArchiveVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "archive"
    }
    fn display_name(&self) -> &'static str {
        "Archive"
    }
    fn auto_mount_request(&self) -> Option<super::super::MountRequest> {
        None
    }
    fn has_origin(&self) -> bool {
        true
    }
    fn is_ephemeral(&self) -> bool {
        true
    }
    fn auto_refresh(&self) -> bool {
        false
    }
    fn can_watch(&self) -> bool {
        false
    }
    fn can_read_sync(&self) -> bool {
        false
    }
    fn can_read_async(&self) -> bool {
        true
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        false
    }
    fn can_create_directory(&self) -> bool {
        false
    }
    fn can_create_symlink(&self) -> bool {
        false
    }
    fn can_touch(&self) -> bool {
        false
    }
    fn can_truncate(&self) -> bool {
        false
    }
    fn can_set_metadata(&self) -> bool {
        false
    }
    fn can_remove(&self) -> bool {
        false
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_stat_directories(&self) -> bool {
        true
    }
    fn can_fs_stats(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        false
    }
    fn can_copy_within(&self) -> bool {
        false
    }
    fn can_hard_link(&self) -> bool {
        false
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        archive_format_path(path, mount_meta)
    }
    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        archive_breadcrumbs(path, mount_meta)
    }
    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        archive_try_parse_display_path(input, mount_meta)
    }
    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        archive_mount_label(mount_meta)
    }
}

static TAR_ARCHIVE_VFS_DESCRIPTOR: TarArchiveVfsDescriptor = TarArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&TAR_ARCHIVE_VFS_DESCRIPTOR));

#[cfg(test)]
#[path = "tar_tests.rs"]
mod tar_tests;

/// Bounded buffering between the iluvatar drive task and the AsyncRead consumer.
/// Each slot holds one decompressed chunk (≤64 KiB).
const STREAM_CHANNEL_CAPACITY: usize = 4;

/// Emit an indexing-progress snapshot via the supplied reporter.
/// `entries` is the running count of archive entries discovered so
/// far; `total_bytes` / `bytes_read` give the determinate ratio so
/// the frontend can render a real progress bar.
fn emit_indexing_progress(
    reporter: &Arc<dyn super::super::ProgressReporter>,
    entries: u64,
    total_bytes: u64,
    bytes_read: u64,
    archive_label: &str,
) {
    let mut extra = std::collections::BTreeMap::new();
    if entries > 0 {
        extra.insert("entries".to_string(), entries.to_string());
    }
    extra.insert("path".to_string(), archive_label.to_string());
    reporter.report(Some(super::super::VfsProgress {
        stage: "Indexing".into(),
        processed: Some(bytes_read),
        total: Some(total_bytes),
        extra,
    }));
}

// ---------------------------------------------------------------------------
// TarArchiveVfs — shared indexing state
// ---------------------------------------------------------------------------

/// Shared state for incremental archive indexing.
struct TarIndexingState {
    /// Incremental directory tree, updated periodically during indexing.
    tree: parking_lot::RwLock<DirectoryTree>,
    /// Completed archive index (set once indexing finishes successfully).
    completed_index: tokio::sync::OnceCell<iluvatar::ArchiveIndex>,
    /// Set if indexing failed with an error message.
    error: tokio::sync::OnceCell<String>,
    /// Notified whenever the tree is updated or indexing completes/fails.
    updated: Notify,
}

// ---------------------------------------------------------------------------
// TarArchiveVfs
// ---------------------------------------------------------------------------

pub struct TarArchiveVfs {
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    state: Arc<TarIndexingState>,
    /// Background-job lifecycle: lazy spawn on first consumer (a
    /// streaming `list_files` or any in-flight file read), cancellation
    /// when the last consumer leaves, sticky-Cancelled — the partial
    /// directory tree remains browsable, and `list_files` reports
    /// `partial: true` until unmount.
    job: super::super::BackgroundJob,
    reporter: Arc<dyn super::super::ProgressReporter>,
}

impl TarArchiveVfs {
    pub fn new(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        origin: VfsPath,
        mount_meta: Vec<u8>,
        reporter: Arc<dyn super::super::ProgressReporter>,
    ) -> Self {
        Self {
            upstream,
            archive_path,
            origin,
            mount_meta,
            state: Arc::new(TarIndexingState {
                tree: parking_lot::RwLock::new(DirectoryTree {
                    dirs: HashMap::new(),
                }),
                completed_index: tokio::sync::OnceCell::new(),
                error: tokio::sync::OnceCell::new(),
                updated: Notify::new(),
            }),
            // Tar's partial tree is fully usable as a partial listing,
            // so Sticky: once cancelled, the tree stays as-is and is
            // served with `partial: true`.
            job: super::super::BackgroundJob::new(super::super::RestartPolicy::Sticky),
            reporter,
        }
    }

    /// Acquire a consumer slot tied to indexing. Spawns the indexer if
    /// this is the first consumer for the current run. Held by both
    /// streaming `list_files` callers and file-read callers (so an
    /// in-flight read keeps the indexer alive even if the originating
    /// pane navigated away).
    fn acquire_indexer(&self) -> super::super::ConsumerGuard {
        let upstream = self.upstream.clone();
        let archive_path = self.archive_path.clone();
        let state = self.state.clone();
        let reporter = self.reporter.clone();
        self.job.acquire(move |handle| {
            tokio::spawn(async move {
                let result = Self::run_indexing(
                    upstream,
                    archive_path,
                    state.clone(),
                    reporter.clone(),
                    &handle,
                )
                .await;
                match result {
                    Ok(()) => handle.mark_done(),
                    Err(e) => {
                        log::error!("archive indexing failed: {}", e);
                        let _ = state.error.set(e.to_string());
                    }
                }
                state.updated.notify_waiters();
                reporter.report(None);
            });
        })
    }

    async fn run_indexing(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        state: Arc<TarIndexingState>,
        reporter: Arc<dyn super::super::ProgressReporter>,
        job: &super::super::JobHandle,
    ) -> Result<(), Error> {
        let details = upstream.file_details(&archive_path).await?;
        let file_size = details.size;
        let descriptor = upstream.descriptor();

        let compression = detect_compression_from_name(archive_path.as_wire_str());
        info!(
            "archive: indexing {} (size={}, compression={:?}, sync={})",
            archive_path,
            file_size,
            compression,
            descriptor.can_read_sync()
        );

        if descriptor.can_read_sync() {
            // Sync path: use streaming reader in spawn_blocking
            let reader = upstream.open_read_sync(&archive_path).await?;
            let cancel = job.cancel_token();
            let state_clone = state.clone();
            let reporter_clone = reporter.clone();
            let archive_label = archive_path.as_wire_str().to_string();

            let result = tokio::task::spawn_blocking(move || {
                Self::drive_indexing_sync(
                    reader,
                    file_size,
                    compression,
                    &cancel,
                    &state_clone,
                    &reporter_clone,
                    &archive_label,
                )
            })
            .await
            .map_err(|e| Error::custom(format!("indexing task panicked: {}", e)))??;

            // Build final tree and set completed index
            info!(
                "archive: indexing finished, {} entries, building final tree",
                result.entries.len()
            );
            let entries: Vec<&iluvatar::IndexEntry> = result.entries.values().collect();
            let tree = build_directory_tree_from_iluvatar(entries);
            *state.tree.write() = tree;
            let _ = state.completed_index.set(result);
            state.updated.notify_waiters();
        } else if descriptor.can_read_async() {
            // Async path: drive engine with read_range on the async runtime
            let mut engine = iluvatar::IndexingEngine::new(compression, None, file_size)
                .map_err(|e| Error::custom(format!("failed to create indexing engine: {}", e)))?;

            let mut position: u64 = 0;
            let mut last_snapshot = tokio::time::Instant::now();
            let mut last_snapshot_entries = 0usize;
            let archive_label = archive_path.as_wire_str().to_string();
            // Initial progress report so the spinner is replaced with
            // a live "Indexing · 0 entries" line immediately on mount.
            emit_indexing_progress(&reporter, 0, file_size, position, &archive_label);

            loop {
                if job.is_cancelled() {
                    info!("archive: indexing cancelled for {}", archive_path);
                    let partial = engine.cancel();
                    let entries: Vec<&iluvatar::IndexEntry> = partial.entries.values().collect();
                    let tree = build_directory_tree_from_iluvatar(entries);
                    *state.tree.write() = tree;
                    state.updated.notify_waiters();
                    return Ok(());
                }

                match engine.step() {
                    iluvatar::EngineRequest::NeedInput => {
                        let chunk = upstream
                            .read_range(&archive_path, position, VFS_READ_CHUNK_SIZE as u64)
                            .await?;
                        if chunk.data.is_empty() {
                            engine.signal_eof();
                        } else {
                            position += chunk.data.len() as u64;
                            engine.provide_data(&chunk.data);
                        }
                    }
                    iluvatar::EngineRequest::Done => break,
                    iluvatar::EngineRequest::Error(e) => {
                        return Err(Error::custom(format!("failed to index archive: {}", e)));
                    }
                    _ => {}
                }

                let progress = engine.progress();
                if progress.entries_found > last_snapshot_entries
                    && last_snapshot.elapsed() >= SNAPSHOT_INTERVAL
                {
                    info!(
                        "archive: async partial snapshot at {} entries (+{}, {:.1}s)",
                        progress.entries_found,
                        progress.entries_found - last_snapshot_entries,
                        last_snapshot.elapsed().as_secs_f64()
                    );
                    last_snapshot_entries = progress.entries_found;
                    last_snapshot = tokio::time::Instant::now();
                    let partial_index = engine.snapshot_index();
                    let entries: Vec<&iluvatar::IndexEntry> =
                        partial_index.entries.values().collect();
                    let tree = build_directory_tree_from_iluvatar(entries);
                    *state.tree.write() = tree;
                    state.updated.notify_waiters();
                    emit_indexing_progress(
                        &reporter,
                        progress.entries_found as u64,
                        file_size,
                        position,
                        &archive_label,
                    );
                }
            }

            let index = engine.finish();
            info!(
                "archive: async indexing finished, {} entries, building final tree",
                index.entries.len()
            );
            let entries: Vec<&iluvatar::IndexEntry> = index.entries.values().collect();
            let tree = build_directory_tree_from_iluvatar(entries);
            *state.tree.write() = tree;
            let _ = state.completed_index.set(index);
            state.updated.notify_waiters();
        } else {
            return Err(Error::custom(
                "upstream VFS supports neither sync nor async read",
            ));
        }

        info!("archive: indexing complete for {}", archive_path);

        Ok(())
    }

    /// Drive the IndexingEngine synchronously using a streaming reader.
    /// Called from within spawn_blocking.
    #[allow(clippy::too_many_arguments)]
    fn drive_indexing_sync(
        mut reader: Box<dyn Read + Send>,
        file_size: u64,
        compression: iluvatar::CompressionFormat,
        cancel: &CancellationToken,
        state: &TarIndexingState,
        reporter: &Arc<dyn super::super::ProgressReporter>,
        archive_label: &str,
    ) -> Result<iluvatar::ArchiveIndex, Error> {
        let mut engine = iluvatar::IndexingEngine::new(compression, None, file_size)
            .map_err(|e| Error::custom(format!("failed to create indexing engine: {}", e)))?;

        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
        let mut last_snapshot = std::time::Instant::now();
        let mut last_snapshot_entries = 0usize;
        let mut bytes_read: u64 = 0;
        // Initial progress so the spinner is replaced immediately.
        emit_indexing_progress(reporter, 0, file_size, bytes_read, archive_label);

        loop {
            if cancel.is_cancelled() {
                info!("archive: indexing cancelled");
                return Ok(engine.cancel());
            }

            match engine.step() {
                iluvatar::EngineRequest::NeedInput => {
                    let n = reader
                        .read(&mut buf)
                        .map_err(|e| Error::custom(format!("read error: {}", e)))?;
                    if n == 0 {
                        engine.signal_eof();
                    } else {
                        bytes_read += n as u64;
                        engine.provide_data(&buf[..n]);
                    }
                }
                iluvatar::EngineRequest::Done => {
                    return Ok(engine.finish());
                }
                iluvatar::EngineRequest::Error(e) => {
                    return Err(Error::custom(format!("failed to index archive: {}", e)));
                }
                _ => {}
            }

            let progress = engine.progress();
            if progress.entries_found > last_snapshot_entries
                && last_snapshot.elapsed() >= SNAPSHOT_INTERVAL
            {
                info!(
                    "archive: sync partial snapshot at {} entries (+{}, {:.1}s)",
                    progress.entries_found,
                    progress.entries_found - last_snapshot_entries,
                    last_snapshot.elapsed().as_secs_f64()
                );
                last_snapshot_entries = progress.entries_found;
                last_snapshot = std::time::Instant::now();
                let partial_index = engine.snapshot_index();
                let entries: Vec<&iluvatar::IndexEntry> = partial_index.entries.values().collect();
                let tree = build_directory_tree_from_iluvatar(entries);
                *state.tree.write() = tree;
                state.updated.notify_waiters();
                emit_indexing_progress(
                    reporter,
                    progress.entries_found as u64,
                    file_size,
                    bytes_read,
                    archive_label,
                );
            }
        }
    }

    /// Wait for the completed archive index (needed for file reads).
    /// The returned `ConsumerGuard` keeps the indexer alive across this
    /// call; the caller must hold it for the *entire* downstream use
    /// of the index — typically by binding it to a local and letting
    /// it drop with the function scope.
    async fn wait_for_index(
        &self,
    ) -> Result<(&iluvatar::ArchiveIndex, super::super::ConsumerGuard), Error> {
        let guard = self.acquire_indexer();

        loop {
            if let Some(index) = self.state.completed_index.get() {
                return Ok((index, guard));
            }
            if let Some(err) = self.state.error.get() {
                return Err(Error::custom(err.clone()));
            }
            self.state.updated.notified().await;
        }
    }

    /// Resolve a path for reading: follow symlinks in the directory tree,
    /// then follow hard links in the iluvatar index. Returns the index path
    /// string and the resolved index entry.
    fn resolve_for_read<'a>(
        &self,
        index: &'a iluvatar::ArchiveIndex,
        path: &Path,
    ) -> Result<(String, &'a iluvatar::IndexEntry), Error> {
        // Archive index keys are Unix-style relative strings; feed the
        // wire form to the std-path-based machinery (leading `/` stripped
        // by `normalize_dir_path` inside `resolve_path`).
        let std_path = StdPath::new(path.as_wire_str());
        let resolved = self.state.tree.read().resolve_path(std_path, true)?;
        let resolved_str = normalized_to_string(&resolved);

        let entry = index_get(index, &resolved_str)
            .ok_or_else(|| not_found(format!("file not found in archive: {}", resolved_str)))?;

        // Follow hard links — the target path is the archive path of the
        // original entry that holds the actual data.
        if matches!(entry.entry_type, iluvatar::EntryType::HardLink)
            && let Some(ref target) = entry.link_target
        {
            let target_normalized = normalize_dir_path(StdPath::new(target));
            let target_str = target_normalized.to_string_lossy();
            let target_entry = index_get(index, &target_str)
                .ok_or_else(|| not_found(format!("hard link target not found: {}", target)))?;
            let target_path = index_path_str(index, &target_str)
                .ok_or_else(|| not_found(format!("hard link target not found: {}", target)))?;
            return Ok((target_path, target_entry));
        }

        let archive_path = index_path_str(index, &resolved_str)
            .ok_or_else(|| not_found(format!("file not found in archive: {}", resolved_str)))?;
        Ok((archive_path, entry))
    }

    /// Drive the sans-I/O ReadEngine using upstream's read_range for I/O.
    async fn drive_read_engine(
        &self,
        path_in_archive: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Vec<u8>, Error> {
        let (index, _guard) = self.wait_for_index().await?;

        let mut engine = if let Some((offset, len)) = range {
            iluvatar::ReadEngine::new_range(index, path_in_archive, offset, len)
        } else {
            iluvatar::ReadEngine::new(index, path_in_archive)
        }
        .map_err(|e| Error::custom(format!("failed to create read engine: {}", e)))?;

        let mut output = Vec::new();
        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
        let mut position: u64 = 0;
        loop {
            match engine.step() {
                iluvatar::EngineRequest::NeedInput => {
                    let chunk = self
                        .upstream
                        .read_range(&self.archive_path, position, buf.len() as u64)
                        .await?;
                    if chunk.data.is_empty() {
                        engine.signal_eof();
                    } else {
                        position += chunk.data.len() as u64;
                        engine.provide_data(&chunk.data);
                    }
                }
                iluvatar::EngineRequest::SeekAndRead { offset, len } => {
                    position = offset;
                    let chunk = self
                        .upstream
                        .read_range(&self.archive_path, position, len as u64)
                        .await?;
                    if chunk.data.is_empty() {
                        engine.signal_eof();
                    } else {
                        position += chunk.data.len() as u64;
                        engine.provide_data(&chunk.data);
                    }
                }
                iluvatar::EngineRequest::OutputReady => loop {
                    let n = engine.read_output(&mut buf);
                    if n == 0 {
                        break;
                    }
                    output.extend_from_slice(&buf[..n]);
                },
                iluvatar::EngineRequest::Done => break,
                iluvatar::EngineRequest::Error(e) => {
                    return Err(Error::custom(format!(
                        "failed to read file from archive: {}",
                        e
                    )));
                }
            }
        }

        Ok(output)
    }
}

#[async_trait::async_trait]
impl Vfs for TarArchiveVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &TAR_ARCHIVE_VFS_DESCRIPTOR
    }

    fn origin(&self) -> Option<&VfsPath> {
        Some(&self.origin)
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.mount_meta.clone()
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::super::VfsFileList, Error> {
        // The directory tree is keyed by Unix-style relative strings;
        // feed the wire form to its std-path-based lookups.
        let std_path = StdPath::new(path.as_wire_str());

        // Acquire a consumer slot for the indexer. The guard is held
        // for the entirety of this call — if the navigation that
        // originated us is cancelled, dropping the guard cancels the
        // indexer (provided no other consumer is holding one).
        let _consumer = self.acquire_indexer();

        // If indexing is already complete (Done), return immediately.
        // Also honor a previously-cancelled state: the partial tree
        // remains browsable, but we stamp `partial: true` so the
        // status bar shows the badge.
        let job_status = self.job.status();
        if self.state.completed_index.get().is_some()
            || job_status == super::super::JobStatus::Cancelled
        {
            log::debug!(
                "archive: list_files {} — index ready ({:?}), returning immediately",
                path,
                job_status,
            );
            return self
                .state
                .tree
                .read()
                .list(std_path)
                .map(|files| super::super::VfsFileList {
                    files,
                    partial: job_status == super::super::JobStatus::Cancelled,
                });
        }
        if let Some(err) = self.state.error.get() {
            return Err(Error::custom(err.clone()));
        }

        log::debug!(
            "archive: list_files {} — waiting for indexing (batch_tx={})",
            path,
            batch_tx.is_some()
        );

        // Stream updates while indexing is in progress
        let mut sent_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut update_count = 0usize;
        loop {
            // Register the notification future BEFORE checking state to avoid races
            let notified = self.state.updated.notified();

            // Check completion/error/cancellation.
            if self.state.completed_index.get().is_some() {
                log::debug!(
                    "archive: list_files {} — indexing completed after {} updates",
                    path,
                    update_count
                );
                break;
            }
            if let Some(err) = self.state.error.get() {
                return Err(Error::custom(err.clone()));
            }
            if self.job.status() == super::super::JobStatus::Cancelled {
                log::debug!(
                    "archive: list_files {} — indexer cancelled, returning partial tree",
                    path,
                );
                break;
            }

            // Send only NEW files as a delta batch
            if let Some(ref tx) = batch_tx {
                let new_files = {
                    let tree = self.state.tree.read();
                    tree.list(std_path).ok().map(|files| {
                        files
                            .into_iter()
                            .filter(|f| sent_names.insert(f.name.clone()))
                            .collect::<Vec<File>>()
                    })
                };
                if let Some(new_files) = new_files
                    && !new_files.is_empty()
                {
                    log::debug!(
                        "archive: list_files {} — sending delta batch ({} new files, {} total sent)",
                        path,
                        new_files.len(),
                        sent_names.len()
                    );
                    if tx.send(new_files).await.is_err() {
                        log::debug!("archive: list_files {} — receiver dropped", path);
                        break;
                    }
                }
            }

            update_count += 1;
            notified.await;
        }

        let result = self.state.tree.read().list(std_path);
        log::debug!(
            "archive: list_files {} — returning final result ({} files)",
            path,
            result.as_ref().map(|f| f.len()).unwrap_or(0)
        );
        // Cancelled during the streaming wait → partial; Done → full.
        let partial = self.job.status() == super::super::JobStatus::Cancelled;
        result.map(|files| super::super::VfsFileList { files, partial })
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        // Archive is immutable while mounted — block forever.
        std::future::pending().await
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let (index, _guard) = self.wait_for_index().await?;

        // Try direct lookup for symlink identity (lstat equivalent).
        // This may return None if the path traverses through a symlink
        // directory (e.g. "/symlink_dir/file.txt" — only the resolved
        // path exists in the index).
        let normalized = normalize_dir_path(StdPath::new(path.as_wire_str()));
        let path_str = normalized.to_string_lossy();
        let original_entry = index_get(index, &path_str);

        let is_symlink =
            original_entry.is_some_and(|e| matches!(e.entry_type, iluvatar::EntryType::SymLink));
        let symlink_target = if is_symlink {
            original_entry.and_then(|e| e.link_target.clone())
        } else {
            None
        };

        // Resolve symlinks/hardlinks for actual metadata (stat equivalent).
        // Fall back to the original entry if the target is broken.
        let (_, resolved_entry) = match self.resolve_for_read(index, path) {
            Ok(resolved) => resolved,
            Err(e) => match original_entry {
                Some(fallback) => {
                    let ap = index_path_str(index, &path_str).unwrap_or_default();
                    (ap, fallback)
                }
                None => return Err(e),
            },
        };

        Ok(FileDetails {
            size: resolved_entry.size,
            mime_type: crate::file_reader::guess_mime_type(StdPath::new(path.as_wire_str())),
            is_dir: resolved_entry.entry_type.is_directory(),
            is_symlink,
            symlink_target,
            user: Some(UserGroup::Id(resolved_entry.uid as u32)),
            group: Some(UserGroup::Id(resolved_entry.gid as u32)),
            mode: Some(Mode(resolved_entry.mode)),
            modified: mtime_to_i64(resolved_entry.mtime),
            accessed: None,
            created: None,
        })
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        // Wait for indexing to complete for accurate file info
        let (_index, _guard) = self.wait_for_index().await?;
        self.state
            .tree
            .read()
            .file_info(StdPath::new(path.as_wire_str()))
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        let (index, guard) = self.wait_for_index().await?;
        let (archive_path_in_index, _entry) = self.resolve_for_read(index, path)?;

        // Construct the engine eagerly so any setup error (file not found,
        // unknown compression, etc.) is reported synchronously to the caller
        // rather than swallowed by the streaming task.
        let mut engine = iluvatar::ReadEngine::new(index, &archive_path_in_index)
            .map_err(|e| Error::custom(format!("failed to create read engine: {}", e)))?;

        let (tx, rx) = mpsc::channel::<std::io::Result<Vec<u8>>>(STREAM_CHANNEL_CAPACITY);
        let upstream = self.upstream.clone();
        let archive_file_path = self.archive_path.clone();
        // The read holds the consumer guard for its entire lifetime
        // (moved into the streaming task) — if the navigation that
        // originated this read is cancelled, the indexer stays alive
        // until the read completes. The cancel token (sourced from
        // the same job) makes the read itself abort on VFS unmount.
        let cancel = self.job.cancel_token();

        tokio::spawn(async move {
            let _indexer_guard = guard;
            let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
            let mut position: u64 = 0;
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                match engine.step() {
                    iluvatar::EngineRequest::NeedInput => {
                        match upstream
                            .read_range(&archive_file_path, position, buf.len() as u64)
                            .await
                        {
                            Ok(chunk) => {
                                if chunk.data.is_empty() {
                                    engine.signal_eof();
                                } else {
                                    position += chunk.data.len() as u64;
                                    engine.provide_data(&chunk.data);
                                }
                            }
                            Err(e) => {
                                let _ = tx
                                    .send(Err(std::io::Error::other(format!(
                                        "upstream read error: {}",
                                        e
                                    ))))
                                    .await;
                                return;
                            }
                        }
                    }
                    iluvatar::EngineRequest::SeekAndRead { offset, len } => {
                        position = offset;
                        match upstream
                            .read_range(&archive_file_path, position, len as u64)
                            .await
                        {
                            Ok(chunk) => {
                                if chunk.data.is_empty() {
                                    engine.signal_eof();
                                } else {
                                    position += chunk.data.len() as u64;
                                    engine.provide_data(&chunk.data);
                                }
                            }
                            Err(e) => {
                                let _ = tx
                                    .send(Err(std::io::Error::other(format!(
                                        "upstream read error: {}",
                                        e
                                    ))))
                                    .await;
                                return;
                            }
                        }
                    }
                    iluvatar::EngineRequest::OutputReady => loop {
                        let n = engine.read_output(&mut buf);
                        if n == 0 {
                            break;
                        }
                        if tx.send(Ok(buf[..n].to_vec())).await.is_err() {
                            // Consumer dropped — abort.
                            return;
                        }
                    },
                    iluvatar::EngineRequest::Done => return,
                    iluvatar::EngineRequest::Error(e) => {
                        let _ = tx
                            .send(Err(std::io::Error::other(format!(
                                "read engine error: {}",
                                e
                            ))))
                            .await;
                        return;
                    }
                }
            }
        });

        Ok(Box::new(TarStreamingReader {
            rx,
            current: Vec::new(),
            offset: 0,
        }))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let (index, _guard) = self.wait_for_index().await?;
        let (archive_path, entry) = self.resolve_for_read(index, path)?;
        let total_size = entry.size;

        let data = self
            .drive_read_engine(&archive_path, Some((offset, length)))
            .await?;

        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }
}

// ---------------------------------------------------------------------------
// TarStreamingReader — turns an mpsc stream of decompressed chunks into AsyncRead.
//
// The engine drive task feeds `Ok(Vec<u8>)` chunks for output, `Err(...)` for
// any failure (upstream or decompression), and closes the channel to signal EOF.
// ---------------------------------------------------------------------------

struct TarStreamingReader {
    rx: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    current: Vec<u8>,
    offset: usize,
}

impl AsyncRead for TarStreamingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.offset < self.current.len() {
            let remaining = &self.current[self.offset..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.offset += n;
            return Poll::Ready(Ok(()));
        }

        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                let n = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.current = chunk;
                    self.offset = n;
                } else {
                    self.current = Vec::new();
                    self.offset = 0;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Err(e)),
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}
