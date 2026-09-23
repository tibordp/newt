use std::collections::HashMap;
// The archive index machinery is keyed by Unix-style relative path
// strings built on std paths; the `Vfs` surface speaks our
// `vfs::path::Path`. Convert at each trait-method boundary via
// `as_wire_str()` (leading `/` stripped by `normalize_dir_path`).
use std::path::Path as StdPath;
use std::sync::Arc;

use log::info;
use tokio::io::AsyncRead;
use tokio::sync::{Notify, mpsc};

use crate::Error;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{File, FsStats, Mode, UserGroup};
use crate::vfs::{FileChunk, FileDetails};

use super::super::open_read_at;
use super::super::origin::{
    origin_breadcrumbs, origin_format_path, origin_mount_label, origin_try_parse_display_path,
};
use super::super::pipelined_read::PipelinedReader;
use super::super::{
    Breadcrumb, DisplayPathMatch, RegisteredDescriptor, VFS_READ_CHUNK_SIZE, Vfs, VfsDescriptor,
    VfsPath,
};
use super::detect_compression_from_name;
use super::stream::{GuardedRead, MAX_READERS, ReaderPool, StreamDriver, drive_reader};
use super::tree::{
    DirectoryTree, SNAPSHOT_INTERVAL, build_directory_tree_from_iluvatar, index_get,
    index_path_str, mtime_to_i64, normalize_dir_path, normalized_to_string,
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
    fn metadata_traits(&self, _mount_meta: &[u8]) -> super::super::MetadataTraits {
        // Tar headers carry mode/uid/gid; zip stays at the default (its
        // unix bits are only present when the archive was made there).
        super::super::MetadataTraits {
            unix_owner: true,
            windows_attributes: false,
        }
    }
    fn auto_mount_request(&self) -> Option<super::super::MountRequest> {
        None
    }
    fn origin_kind(&self) -> super::super::OriginKind {
        super::super::OriginKind::Entry
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
    fn can_read(&self) -> bool {
        true
    }
    fn can_overwrite(&self) -> bool {
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
    fn can_write_range(&self) -> bool {
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
        origin_format_path(path, mount_meta)
    }
    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        origin_breadcrumbs(path, mount_meta)
    }
    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        origin_try_parse_display_path(input, mount_meta)
    }
    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        origin_mount_label(mount_meta)
    }
}

static TAR_ARCHIVE_VFS_DESCRIPTOR: TarArchiveVfsDescriptor = TarArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&TAR_ARCHIVE_VFS_DESCRIPTOR));

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
    /// Readers parked after a read, so the next entry in archive order
    /// decodes forward instead of restoring a checkpoint.
    pool: Arc<ReaderPool>,
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
            pool: Arc::new(ReaderPool::new(MAX_READERS)),
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
                        log::error!("archive indexing failed: {:?}", e);
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

        let compression = detect_compression_from_name(archive_path.as_wire_str());
        info!(
            "archive: indexing {} (size={}, compression={:?})",
            archive_path, file_size, compression,
        );

        {
            // Drive the engine over one held-open upstream handle.
            let mut engine = iluvatar::IndexingEngine::new(compression, None, file_size)
                .map_err(|e| Error::custom(format!("failed to create indexing engine: {}", e)))?;

            let mut reader = open_read_at(&upstream, &archive_path).await?;
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
                        // Don't read past the known end: not every upstream
                        // returns an empty chunk there (S3 range GETs at/past
                        // the object size are errors), and the decompressor
                        // may ask for more input after consuming the whole
                        // file (e.g. zstd probing for a concatenated frame).
                        if position >= file_size {
                            engine.signal_eof();
                            continue;
                        }
                        let data = reader.read_at(position, VFS_READ_CHUNK_SIZE as u64).await?;
                        if data.is_empty() {
                            engine.signal_eof();
                        } else {
                            position += data.len() as u64;
                            engine.provide_data(&data);
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
                "archive: indexing finished, {} entries, building final tree",
                index.entries.len()
            );
            let entries: Vec<&iluvatar::IndexEntry> = index.entries.values().collect();
            let tree = build_directory_tree_from_iluvatar(entries);
            *state.tree.write() = tree;
            let _ = state.completed_index.set(index);
            state.updated.notify_waiters();
        }

        info!("archive: indexing complete for {}", archive_path);

        Ok(())
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

        let entry = index_get(index, &resolved_str).ok_or_else(|| {
            Error::not_found(format!("file not found in archive: {}", resolved_str))
        })?;

        // Follow hard links — the target path is the archive path of the
        // original entry that holds the actual data.
        if matches!(entry.entry_type, iluvatar::EntryType::HardLink)
            && let Some(ref target) = entry.link_target
        {
            let target_normalized = normalize_dir_path(StdPath::new(target));
            let target_str = target_normalized.to_string_lossy();
            let target_entry = index_get(index, &target_str).ok_or_else(|| {
                Error::not_found(format!("hard link target not found: {}", target))
            })?;
            let target_path = index_path_str(index, &target_str).ok_or_else(|| {
                Error::not_found(format!("hard link target not found: {}", target))
            })?;
            return Ok((target_path, target_entry));
        }

        let archive_path = index_path_str(index, &resolved_str).ok_or_else(|| {
            Error::not_found(format!("file not found in archive: {}", resolved_str))
        })?;
        Ok((archive_path, entry))
    }

    /// A reader serving `len` bytes at `offset` in the uncompressed
    /// stream: a parked reader close before the target, else a restore
    /// from the nearest checkpoint.
    fn prepare_reader(
        &self,
        index: &iluvatar::ArchiveIndex,
        offset: u64,
        len: u64,
    ) -> Result<iluvatar::StreamReader, Error> {
        if let Some(reader) = self.pool.take(&index.stream, offset, len) {
            return Ok(reader);
        }
        iluvatar::StreamReader::new(&index.stream, offset, len)
            .map_err(|e| Error::custom(format!("failed to create read engine: {}", e)))
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
            mime_type: crate::vfs::file::guess_mime_type(StdPath::new(path.as_wire_str())),
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
        let (_, entry) = self.resolve_for_read(index, path)?;
        let reader = self.prepare_reader(index, entry.uncompressed_offset, entry.size)?;
        let upstream = open_read_at(&self.upstream, &self.archive_path).await?;
        Ok(Box::new(GuardedRead {
            inner: PipelinedReader::new(
                StreamDriver::new(
                    reader,
                    0..index.metadata.archive_size,
                    self.pool.clone(),
                    entry.size,
                    None,
                ),
                Some(upstream),
                "tar archive",
            ),
            _guard: guard,
        }))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let (index, _guard) = self.wait_for_index().await?;
        let (_, entry) = self.resolve_for_read(index, path)?;
        let total_size = entry.size;
        let want = length.min(total_size.saturating_sub(offset));
        if want == 0 {
            return Ok(FileChunk {
                data: Vec::new(),
                offset,
                total_size,
            });
        }
        let reader = self.prepare_reader(index, entry.uncompressed_offset + offset, want)?;
        let mut upstream = open_read_at(&self.upstream, &self.archive_path).await?;
        let packed = 0..index.metadata.archive_size;
        let (mut data, reader) = drive_reader(upstream.as_mut(), &packed, reader, want).await?;
        self.pool.park(reader);
        data.truncate(want as usize);
        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }

    /// Entries sit back to back in the uncompressed stream.
    async fn read_order(&self, paths: &[PathBuf]) -> Result<Option<Vec<u64>>, Error> {
        let (index, _guard) = self.wait_for_index().await?;
        Ok(Some(
            paths
                .iter()
                .map(|p| {
                    self.resolve_for_read(index, p)
                        .map_or(u64::MAX, |(_, e)| e.uncompressed_offset)
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
#[path = "tar_tests.rs"]
mod tests;
