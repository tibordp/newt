//! A bare compressed file (`whatever.gz`, `.bz2`, `.xz`, `.zst`) mounted
//! as a one-entry filesystem: the entry is named by stripping the suffix
//! and carries no attributes, since the container carries none worth
//! showing.
//!
//! The stream is indexed in the background at first use, the way a tar
//! archive is, because nothing but a full pass tells its unpacked length;
//! the listing shows the entry without a size until then, and reads wait
//! for the complete index. Reads then run over the shared reader pool and
//! stream driver in [`super::stream`].

use std::collections::HashMap;
use std::path::Path as StdPath;
use std::sync::Arc;

use iluvatar::{EngineRequest, FixedInterval, StreamIndex, StreamIndexer};
use log::info;
use tokio::io::AsyncRead;
use tokio::sync::{Notify, mpsc};

use crate::Error;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{File, FileChunk, FileDetails, FsStats};

use super::super::open_read_at;
use super::super::origin::{
    origin_breadcrumbs, origin_format_path, origin_mount_label, origin_try_parse_display_path,
};
use super::super::pipelined_read::PipelinedReader;
use super::super::{
    Breadcrumb, DisplayPathMatch, RegisteredDescriptor, Vfs, VfsDescriptor, VfsFileList, VfsPath,
};
use super::detect_compression_from_name;
use super::stream::{
    GuardedRead, MAX_READERS, PACKED_SLICE, ReaderPool, StreamDriver, drive_reader, read_packed,
};
use super::tree::DirectoryTree;

/// How often the indexer parks a snapshot of its progress, so a cancelled
/// run resumes from there instead of from the start.
const SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

// ---------------------------------------------------------------------------
// CompressedFileVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct CompressedFileVfsDescriptor;

impl VfsDescriptor for CompressedFileVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "compressed_file"
    }
    fn display_name(&self) -> &'static str {
        "Compressed file"
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
        false
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

static COMPRESSED_FILE_VFS_DESCRIPTOR: CompressedFileVfsDescriptor = CompressedFileVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&COMPRESSED_FILE_VFS_DESCRIPTOR));

/// The entry name for a compressed file: the name without its suffix, or
/// the whole name when nothing would be left.
pub fn entry_name(archive_name: &str) -> String {
    let lower = archive_name.to_ascii_lowercase();
    for ext in super::COMPRESSED_EXTENSIONS {
        if let Some(stem) = lower.strip_suffix(ext)
            && let Some(stem) = stem.strip_suffix('.')
            && !stem.is_empty()
        {
            return archive_name[..stem.len()].to_string();
        }
    }
    archive_name.to_string()
}

// ---------------------------------------------------------------------------
// CompressedFileVfs
// ---------------------------------------------------------------------------

struct IndexState {
    /// The one entry, sized once the index is complete.
    tree: parking_lot::RwLock<DirectoryTree>,
    /// Where a cancelled run left off; the next run resumes from it.
    partial: parking_lot::Mutex<Option<StreamIndex>>,
    completed: tokio::sync::OnceCell<StreamIndex>,
    error: tokio::sync::OnceCell<String>,
    /// Notified whenever the tree changes or indexing completes or fails.
    updated: Notify,
}

pub struct CompressedFileVfs {
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    display_path: String,
    name: String,
    state: Arc<IndexState>,
    /// Lazy spawn on the first consumer, cancellation when the last one
    /// leaves, resumed from the parked snapshot on the next.
    job: super::super::BackgroundJob,
    reporter: Arc<dyn super::super::ProgressReporter>,
    pool: Arc<ReaderPool>,
}

impl CompressedFileVfs {
    pub fn new(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        origin: VfsPath,
        mount_meta: Vec<u8>,
        display_path: String,
        reporter: Arc<dyn super::super::ProgressReporter>,
    ) -> Self {
        let name = entry_name(archive_path.file_name().unwrap_or(""));
        let tree = single_entry_tree(&name, None);
        Self {
            upstream,
            archive_path,
            origin,
            mount_meta,
            display_path,
            name,
            state: Arc::new(IndexState {
                tree: parking_lot::RwLock::new(tree),
                partial: parking_lot::Mutex::new(None),
                completed: tokio::sync::OnceCell::new(),
                error: tokio::sync::OnceCell::new(),
                updated: Notify::new(),
            }),
            job: super::super::BackgroundJob::new(super::super::RestartPolicy::Resettable),
            reporter,
            pool: Arc::new(ReaderPool::new(MAX_READERS)),
        }
    }

    fn acquire_indexer(&self) -> super::super::ConsumerGuard {
        let upstream = self.upstream.clone();
        let archive_path = self.archive_path.clone();
        let display_path = self.display_path.clone();
        let name = self.name.clone();
        let state = self.state.clone();
        let reporter = self.reporter.clone();
        self.job.acquire(move |handle| {
            tokio::spawn(async move {
                let result = Self::run_indexing(
                    upstream,
                    archive_path,
                    &display_path,
                    &name,
                    state.clone(),
                    reporter.clone(),
                    &handle,
                )
                .await;
                match result {
                    Ok(()) => handle.mark_done(),
                    Err(e) => {
                        log::error!("compressed file indexing failed: {:?}", e);
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
        display_path: &str,
        name: &str,
        state: Arc<IndexState>,
        reporter: Arc<dyn super::super::ProgressReporter>,
        job: &super::super::JobHandle,
    ) -> Result<(), Error> {
        if state.completed.get().is_some() {
            return Ok(());
        }
        let file_size = upstream.file_details(&archive_path).await?.size;
        let format = detect_compression_from_name(archive_path.as_wire_str());
        let strategy = FixedInterval::new(iluvatar::default_interval_for_format(format));
        let mut indexer = match state.partial.lock().take() {
            Some(index) => StreamIndexer::resume(index, strategy),
            None => StreamIndexer::new(format.into(), strategy, Some(file_size)),
        }
        .map_err(|e| Error::custom(format!("failed to create indexer: {}", e)))?;
        indexer.emit_output(false);
        info!(
            "archive: indexing compressed file {} (size={}, {:?})",
            archive_path, file_size, format
        );

        let mut upstream_reader = open_read_at(&upstream, &archive_path).await?;
        let packed = 0..file_size;
        let mut last_snapshot = tokio::time::Instant::now();
        let report = |at: u64| {
            let mut extra = std::collections::BTreeMap::new();
            extra.insert("path".to_string(), display_path.to_string());
            reporter.report(Some(super::super::VfsProgress {
                stage: "Indexing".into(),
                processed: Some(at),
                total: Some(file_size),
                extra,
            }));
        };
        report(0);
        loop {
            if job.is_cancelled() {
                info!("archive: indexing cancelled for {}", archive_path);
                *state.partial.lock() = Some(indexer.finish());
                return Ok(());
            }
            match indexer.step() {
                EngineRequest::NeedInput => {
                    let at = indexer.progress().compressed_pos;
                    match read_packed(upstream_reader.as_mut(), &packed, at, PACKED_SLICE).await? {
                        Some(data) => indexer.provide_data(&data),
                        None => indexer.signal_eof(),
                    }
                }
                EngineRequest::SeekAndRead { offset, len } => {
                    match read_packed(upstream_reader.as_mut(), &packed, offset, len as u64).await?
                    {
                        Some(data) => indexer.provide_data(&data),
                        None => indexer.signal_eof(),
                    }
                }
                EngineRequest::OutputReady => {}
                EngineRequest::Done => break,
                EngineRequest::Error(e) => {
                    return Err(Error::custom(format!("failed to index {}: {}", name, e)));
                }
            }
            if last_snapshot.elapsed() >= SNAPSHOT_INTERVAL {
                last_snapshot = tokio::time::Instant::now();
                *state.partial.lock() = Some(indexer.snapshot());
                report(indexer.progress().compressed_pos);
            }
        }
        let index = indexer.finish();
        let len = index.unpacked_len.unwrap_or(0);
        info!("archive: indexed {} ({} bytes unpacked)", archive_path, len);
        *state.tree.write() = single_entry_tree(name, Some(len));
        let _ = state.completed.set(index);
        state.updated.notify_waiters();
        Ok(())
    }

    /// The complete index, holding a consumer slot on the indexer for as
    /// long as the guard lives.
    async fn wait_for_index(&self) -> Result<(&StreamIndex, super::super::ConsumerGuard), Error> {
        let guard = self.acquire_indexer();
        loop {
            let notified = self.state.updated.notified();
            if let Some(index) = self.state.completed.get() {
                return Ok((index, guard));
            }
            if let Some(err) = self.state.error.get() {
                return Err(Error::custom(err.clone()));
            }
            notified.await;
        }
    }

    /// The entry a path names, or the error for anything else.
    fn resolve(&self, path: &Path) -> Result<(), Error> {
        self.state
            .tree
            .read()
            .file_info(StdPath::new(path.as_wire_str()))
            .map(|_| ())
    }

    fn prepare_reader(
        &self,
        index: &StreamIndex,
        offset: u64,
        len: u64,
    ) -> Result<iluvatar::StreamReader, Error> {
        if let Some(reader) = self.pool.take(index, offset, len) {
            return Ok(reader);
        }
        iluvatar::StreamReader::new(index, offset, len)
            .map_err(|e| Error::custom(format!("failed to create reader: {}", e)))
    }
}

fn single_entry_tree(name: &str, size: Option<u64>) -> DirectoryTree {
    let file = File {
        name: name.to_string(),
        size,
        allocated_size: None,
        device_id: None,
        inode: None,
        hard_links: None,
        is_dir: false,
        is_hidden: name.starts_with('.'),
        is_symlink: false,
        symlink_target: None,
        user: None,
        group: None,
        mode: None,
        modified: None,
        accessed: None,
        created: None,
        key: None,
        source: None,
        attributes: None,
    };
    let mut dirs = HashMap::new();
    dirs.insert(std::path::PathBuf::from(""), vec![file]);
    DirectoryTree { dirs }
}

#[async_trait::async_trait]
impl Vfs for CompressedFileVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &COMPRESSED_FILE_VFS_DESCRIPTOR
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
    ) -> Result<VfsFileList, Error> {
        let std_path = StdPath::new(path.as_wire_str());
        let _guard = self.acquire_indexer();
        let files = self.state.tree.read().list(std_path)?;
        if self.state.completed.get().is_some() {
            return Ok(files.into());
        }
        // The entry is listable before its size is known; the final list
        // lands once the index does.
        if let Some(tx) = &batch_tx
            && tx.send(files).await.is_err()
        {
            return Err(Error::cancelled());
        }
        loop {
            let notified = self.state.updated.notified();
            if self.state.completed.get().is_some() {
                return Ok(self.state.tree.read().list(std_path)?.into());
            }
            if let Some(err) = self.state.error.get() {
                return Err(Error::custom(err.clone()));
            }
            if self.job.status() == super::super::JobStatus::Cancelled {
                return Ok(VfsFileList {
                    files: self.state.tree.read().list(std_path)?,
                    partial: Some("indexing cancelled".to_string()),
                });
            }
            notified.await;
        }
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        std::future::pending().await
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let (index, _guard) = self.wait_for_index().await?;
        let file = self
            .state
            .tree
            .read()
            .file_info(StdPath::new(path.as_wire_str()))?;
        Ok(FileDetails {
            size: if file.is_dir {
                0
            } else {
                index.unpacked_len.unwrap_or(0)
            },
            mime_type: crate::vfs::file::guess_mime_type(StdPath::new(path.as_wire_str())),
            is_dir: file.is_dir,
            is_symlink: false,
            symlink_target: None,
            user: None,
            group: None,
            mode: None,
            modified: None,
            accessed: None,
            created: None,
        })
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let (_, _guard) = self.wait_for_index().await?;
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
        self.resolve(path)?;
        let len = index.unpacked_len.unwrap_or(0);
        let reader = self.prepare_reader(index, 0, len)?;
        let upstream = open_read_at(&self.upstream, &self.archive_path).await?;
        let file_size = index.compressed_len.unwrap_or(u64::MAX);
        Ok(Box::new(GuardedRead {
            inner: PipelinedReader::new(
                StreamDriver::new(reader, 0..file_size, self.pool.clone(), len, None),
                Some(upstream),
                "compressed file",
            ),
            _guard: guard,
        }))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let (index, _guard) = self.wait_for_index().await?;
        self.resolve(path)?;
        let total_size = index.unpacked_len.unwrap_or(0);
        let want = length.min(total_size.saturating_sub(offset));
        if want == 0 {
            return Ok(FileChunk {
                data: Vec::new(),
                offset,
                total_size,
            });
        }
        let reader = self.prepare_reader(index, offset, want)?;
        let mut upstream = open_read_at(&self.upstream, &self.archive_path).await?;
        let packed = 0..index.compressed_len.unwrap_or(u64::MAX);
        let (mut data, reader) = drive_reader(upstream.as_mut(), &packed, reader, want).await?;
        self.pool.park(reader);
        data.truncate(want as usize);
        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }
}

#[cfg(test)]
#[path = "compressed_tests.rs"]
mod tests;
