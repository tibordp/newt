use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::info;
use tokio::io::AsyncRead;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode, UserGroup};

use super::super::{Breadcrumb, RegisteredDescriptor, Vfs, VfsDescriptor, VfsPath};
use super::{
    DirectoryTree, SNAPSHOT_INTERVAL, archive_breadcrumbs, archive_format_path,
    archive_mount_label, archive_try_parse_display_path, build_directory_tree_from_iluvatar,
    detect_compression_from_name, index_get, index_path_str, mtime_to_i128, normalize_dir_path,
    not_found,
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
    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<PathBuf> {
        archive_try_parse_display_path(input, mount_meta)
    }
    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        archive_mount_label(mount_meta)
    }
}

static TAR_ARCHIVE_VFS_DESCRIPTOR: TarArchiveVfsDescriptor = TarArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&TAR_ARCHIVE_VFS_DESCRIPTOR));

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
    /// Cancellation token — cancelled on VFS drop to abort indexing.
    cancel: CancellationToken,
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
    /// Ensures the indexing task is spawned at most once.
    indexing_started: tokio::sync::OnceCell<()>,
}

impl Drop for TarArchiveVfs {
    fn drop(&mut self) {
        self.state.cancel.cancel();
    }
}

impl TarArchiveVfs {
    pub fn new(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        origin: VfsPath,
        mount_meta: Vec<u8>,
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
                cancel: CancellationToken::new(),
            }),
            indexing_started: tokio::sync::OnceCell::new(),
        }
    }

    /// Spawn the indexing task. Called at most once via `start_indexing`.
    fn start_indexing(&self) {
        let upstream = self.upstream.clone();
        let archive_path = self.archive_path.clone();
        let state = self.state.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::run_indexing(upstream, archive_path, state.clone()).await {
                log::error!("archive indexing failed: {}", e);
                let _ = state.error.set(e.to_string());
                state.updated.notify_waiters();
            }
        });
    }

    async fn run_indexing(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        state: Arc<TarIndexingState>,
    ) -> Result<(), Error> {
        let details = upstream.file_details(&archive_path).await?;
        let file_size = details.size;
        let descriptor = upstream.descriptor();

        let compression = detect_compression_from_name(&archive_path.to_string_lossy());
        info!(
            "archive: indexing {} (size={}, compression={:?}, sync={})",
            archive_path.display(),
            file_size,
            compression,
            descriptor.can_read_sync()
        );

        if descriptor.can_read_sync() {
            // Sync path: use streaming reader in spawn_blocking
            let reader = upstream.open_read_sync(&archive_path).await?;
            let cancel = state.cancel.clone();
            let state_clone = state.clone();

            let result = tokio::task::spawn_blocking(move || {
                Self::drive_indexing_sync(reader, file_size, compression, &cancel, &state_clone)
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

            loop {
                if state.cancel.is_cancelled() {
                    info!("archive: indexing cancelled for {}", archive_path.display());
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
                            .read_range(&archive_path, position, 64 * 1024)
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

        info!("archive: indexing complete for {}", archive_path.display());

        Ok(())
    }

    /// Drive the IndexingEngine synchronously using a streaming reader.
    /// Called from within spawn_blocking.
    fn drive_indexing_sync(
        mut reader: Box<dyn Read + Send>,
        file_size: u64,
        compression: iluvatar::CompressionFormat,
        cancel: &CancellationToken,
        state: &TarIndexingState,
    ) -> Result<iluvatar::ArchiveIndex, Error> {
        let mut engine = iluvatar::IndexingEngine::new(compression, None, file_size)
            .map_err(|e| Error::custom(format!("failed to create indexing engine: {}", e)))?;

        let mut buf = vec![0u8; 64 * 1024];
        let mut last_snapshot = std::time::Instant::now();
        let mut last_snapshot_entries = 0usize;

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
            }
        }
    }

    /// Wait for the completed archive index (needed for file reads).
    async fn wait_for_index(&self) -> Result<&iluvatar::ArchiveIndex, Error> {
        // Start indexing if not already started
        self.start_indexing_once();

        loop {
            if let Some(index) = self.state.completed_index.get() {
                return Ok(index);
            }
            if let Some(err) = self.state.error.get() {
                return Err(Error::custom(err.clone()));
            }
            self.state.updated.notified().await;
        }
    }

    /// Start indexing exactly once.
    fn start_indexing_once(&self) {
        if self.indexing_started.initialized() {
            return;
        }
        // Race-safe: OnceCell set() ensures only one succeeds
        if self.indexing_started.set(()).is_ok() {
            self.start_indexing();
        }
    }

    /// Drive the sans-I/O ReadEngine using upstream's read_range for I/O.
    async fn drive_read_engine(
        &self,
        path_in_archive: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Vec<u8>, Error> {
        let index = self.wait_for_index().await?;

        let mut engine = if let Some((offset, len)) = range {
            iluvatar::ReadEngine::new_range(index, path_in_archive, offset, len)
        } else {
            iluvatar::ReadEngine::new(index, path_in_archive)
        }
        .map_err(|e| Error::custom(format!("failed to create read engine: {}", e)))?;

        let mut output = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
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
    ) -> Result<Vec<File>, Error> {
        self.start_indexing_once();

        // If indexing is already complete, return immediately
        if self.state.completed_index.get().is_some() {
            log::debug!(
                "archive: list_files {} — index ready, returning immediately",
                path.display()
            );
            return self.state.tree.read().list(path);
        }
        if let Some(err) = self.state.error.get() {
            return Err(Error::custom(err.clone()));
        }

        log::debug!(
            "archive: list_files {} — waiting for indexing (batch_tx={})",
            path.display(),
            batch_tx.is_some()
        );

        // Stream updates while indexing is in progress
        let mut sent_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut update_count = 0usize;
        loop {
            // Register the notification future BEFORE checking state to avoid races
            let notified = self.state.updated.notified();

            // Check completion/error
            if self.state.completed_index.get().is_some() {
                log::debug!(
                    "archive: list_files {} — indexing completed after {} updates",
                    path.display(),
                    update_count
                );
                break;
            }
            if let Some(err) = self.state.error.get() {
                return Err(Error::custom(err.clone()));
            }

            // Send only NEW files as a delta batch
            if let Some(ref tx) = batch_tx {
                let new_files = {
                    let tree = self.state.tree.read();
                    tree.list(path).ok().map(|files| {
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
                        path.display(),
                        new_files.len(),
                        sent_names.len()
                    );
                    if tx.send(new_files).await.is_err() {
                        log::debug!("archive: list_files {} — receiver dropped", path.display());
                        break;
                    }
                }
            }

            update_count += 1;
            notified.await;
        }

        let result = self.state.tree.read().list(path);
        log::debug!(
            "archive: list_files {} — returning final result ({} files)",
            path.display(),
            result.as_ref().map(|f| f.len()).unwrap_or(0)
        );
        result
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        // Archive is immutable while mounted — block forever.
        std::future::pending().await
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let index = self.wait_for_index().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();

        let entry = index_get(index, &path_str)
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;

        Ok(FileDetails {
            size: entry.size,
            mime_type: crate::file_reader::guess_mime_type(path),
            is_dir: entry.entry_type.is_directory(),
            is_symlink: matches!(entry.entry_type, iluvatar::EntryType::SymLink),
            symlink_target: entry.link_target.as_ref().map(PathBuf::from),
            user: Some(UserGroup::Id(entry.uid as u32)),
            group: Some(UserGroup::Id(entry.gid as u32)),
            mode: Some(Mode(entry.mode)),
            modified: mtime_to_i128(entry.mtime),
            accessed: None,
            created: None,
        })
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        // Wait for indexing to complete for accurate file info
        self.wait_for_index().await?;
        self.state.tree.read().file_info(path)
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        let index = self.wait_for_index().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();
        let archive_path = index_path_str(index, &path_str)
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;
        let data = self.drive_read_engine(&archive_path, None).await?;
        Ok(Box::new(std::io::Cursor::new(data)))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let index = self.wait_for_index().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();

        let entry = index_get(index, &path_str)
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;
        let total_size = entry.size;
        let archive_path = index_path_str(index, &path_str).unwrap();

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
