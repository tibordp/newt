//! Disc image (ISO 9660 / UDF) VFS — a read-only filesystem layered on the
//! VFS holding the image, in the same way archives are. Unlike archives,
//! file data inside a disc image is stored as raw contiguous extents, so
//! every read maps 1:1 onto `upstream.read_range` — no whole-entry
//! buffering, and a Blu-ray-sized image on S3 streams without ever being
//! downloaded.
//!
//! Directories are parsed lazily per-directory (the sans-IO `newt-disc`
//! parser hands us byte ranges to fetch); the image is immutable, so both
//! the block cache and the directory cache never invalidate.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::Read;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::AsyncRead;
use tokio::sync::{Mutex, OnceCell};

use newt_disc::{Chunk, DiscError, DiscFs, Entry, EntryData, EntryKind, ExtentKind, ProbeOp, Step};

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode, UserGroup};
use crate::vfs::path::{Path, PathBuf};

use super::archive::{
    archive_breadcrumbs, archive_format_path, archive_mount_label, archive_try_parse_display_path,
    build_origin_meta,
};
use super::{Breadcrumb, DisplayPathMatch, RegisteredDescriptor, Vfs, VfsDescriptor, VfsPath};

const DISC_EXTENSIONS: &[&str] = &["iso", "udf"];

pub fn is_disc_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    DISC_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
}

/// Matches Linux MAXSYMLINKS, like the archive resolver.
const MAX_SYMLINK_HOPS: usize = 40;

/// Metadata reads go through an aligned block cache so a directory walk
/// over a high-latency upstream (S3) coalesces into a few range GETs
/// instead of one per structure. File-content reads bypass it.
const CACHE_BLOCK: u64 = 128 * 1024;
const CACHE_MAX_BLOCKS: usize = 128; // 16 MiB

/// Chunk size for the streaming reader behind `open_read_async`.
const STREAM_CHUNK: u64 = 256 * 1024;

pub async fn mount(
    origin: VfsPath,
    ctx: &crate::api::MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    log::info!("mounting disc image VFS for origin={}", origin);
    let (upstream_vfs, image_path) = ctx.registry.resolve(&origin)?;
    let (mount_meta, display_path) = build_origin_meta(upstream_vfs.as_ref(), &origin);
    Ok(Arc::new(DiscVfs {
        upstream: upstream_vfs,
        image_path,
        origin,
        mount_meta,
        display_path,
        reporter: ctx.progress_reporter.clone(),
        state: OnceCell::new(),
    }))
}

fn disc_err(e: DiscError) -> Error {
    Error::custom(e.to_string())
}

fn not_found(msg: impl Into<String>) -> Error {
    Error {
        kind: crate::ErrorKind::NotFound,
        message: msg.into(),
    }
}

// ---------------------------------------------------------------------------
// DiscVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct DiscVfsDescriptor;

impl VfsDescriptor for DiscVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "disc"
    }
    fn display_name(&self) -> &'static str {
        "Disc image"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn origin_kind(&self) -> super::OriginKind {
        super::OriginKind::Entry
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

static DISC_VFS_DESCRIPTOR: DiscVfsDescriptor = DiscVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&DISC_VFS_DESCRIPTOR));

#[cfg(test)]
#[path = "disc_tests.rs"]
mod disc_tests;

// ---------------------------------------------------------------------------
// Block cache
// ---------------------------------------------------------------------------

struct BlockCache {
    upstream: Arc<dyn Vfs>,
    image_path: PathBuf,
    image_size: u64,
    inner: Mutex<CacheInner>,
}

#[derive(Default)]
struct CacheInner {
    blocks: HashMap<u64, Arc<Vec<u8>>>,
    /// Insertion order for eviction. The parser's access pattern is a
    /// forward walk over clustered structures; FIFO is close enough to
    /// LRU here and keeps the bookkeeping trivial.
    order: VecDeque<u64>,
}

impl BlockCache {
    async fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>, Error> {
        if offset
            .checked_add(len)
            .is_none_or(|end| end > self.image_size)
        {
            return Err(Error::custom("disc image structure out of bounds"));
        }
        if len == 0 {
            return Ok(Vec::new());
        }
        let first = offset / CACHE_BLOCK;
        let last = (offset + len - 1) / CACHE_BLOCK;

        // Snapshot present blocks, then fetch the missing ones
        // concurrently. Assembly uses only this local map, so concurrent
        // eviction can't invalidate it.
        let mut local: HashMap<u64, Arc<Vec<u8>>> = HashMap::new();
        let mut missing: Vec<u64> = Vec::new();
        {
            let inner = self.inner.lock().await;
            for b in first..=last {
                match inner.blocks.get(&b) {
                    Some(data) => {
                        local.insert(b, data.clone());
                    }
                    None => missing.push(b),
                }
            }
        }
        if !missing.is_empty() {
            let fetched =
                futures::future::try_join_all(missing.iter().map(|&b| self.fetch_block(b))).await?;
            let mut inner = self.inner.lock().await;
            for (b, data) in missing.into_iter().zip(fetched) {
                local.insert(b, data.clone());
                if inner.blocks.insert(b, data).is_none() {
                    inner.order.push_back(b);
                }
                while inner.order.len() > CACHE_MAX_BLOCKS {
                    if let Some(evict) = inner.order.pop_front() {
                        inner.blocks.remove(&evict);
                    }
                }
            }
        }

        let mut out = Vec::with_capacity(len as usize);
        for b in first..=last {
            let data = &local[&b];
            let block_start = b * CACHE_BLOCK;
            let from = offset.max(block_start) - block_start;
            let to = (offset + len - block_start).min(data.len() as u64);
            if from > to {
                return Err(Error::custom("disc image read out of cached range"));
            }
            out.extend_from_slice(&data[from as usize..to as usize]);
        }
        if out.len() != len as usize {
            return Err(Error::custom(format!(
                "short read from disc image: got {} of {} bytes",
                out.len(),
                len
            )));
        }
        Ok(out)
    }

    async fn fetch_block(&self, block: u64) -> Result<Arc<Vec<u8>>, Error> {
        let start = block * CACHE_BLOCK;
        let len = CACHE_BLOCK.min(self.image_size - start);
        let chunk = self
            .upstream
            .read_range(&self.image_path, start, len)
            .await?;
        Ok(Arc::new(chunk.data))
    }
}

// ---------------------------------------------------------------------------
// DiscVfs
// ---------------------------------------------------------------------------

pub struct DiscVfs {
    upstream: Arc<dyn Vfs>,
    image_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    display_path: String,
    reporter: Arc<dyn super::ProgressReporter>,
    state: OnceCell<DiscState>,
}

struct DiscState {
    fs: DiscFs,
    cache: BlockCache,
    /// Directory listings keyed by canonical resolved path ("" = root).
    /// Never invalidated — the image is immutable.
    dirs: Mutex<HashMap<String, Arc<Vec<Entry>>>>,
}

async fn fetch_chunks(
    cache: &BlockCache,
    ranges: Vec<std::ops::Range<u64>>,
) -> Result<Vec<Chunk>, Error> {
    futures::future::try_join_all(ranges.into_iter().map(|r| async move {
        Ok::<_, Error>(Chunk {
            offset: r.start,
            data: cache.read(r.start, r.end - r.start).await?,
        })
    }))
    .await
}

/// Split an absolute-within-entry byte range across the entry's extents,
/// yielding (image_offset, len, kind) pieces in order.
fn slice_extents(
    extents: &[newt_disc::Extent],
    mut offset: u64,
    mut len: u64,
) -> Vec<(u64, u64, ExtentKind)> {
    let mut out = Vec::new();
    for e in extents {
        if len == 0 {
            break;
        }
        if offset >= e.len {
            offset -= e.len;
            continue;
        }
        let take = (e.len - offset).min(len);
        out.push((e.offset + offset, take, e.kind));
        offset = 0;
        len -= take;
    }
    out
}

fn normalize_components(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c.to_string()),
        }
    }
    out
}

fn entry_to_file(e: &Entry) -> File {
    File {
        name: e.name.clone(),
        size: (e.kind != EntryKind::Dir).then_some(e.size),
        allocated_size: None,
        device_id: None,
        inode: None,
        hard_links: e.nlink.map(u64::from),
        is_dir: e.kind == EntryKind::Dir,
        is_hidden: e.hidden || e.name.starts_with('.'),
        is_symlink: e.kind == EntryKind::Symlink,
        symlink_target: e.link_target.clone(),
        user: e.uid.map(UserGroup::Id),
        group: e.gid.map(UserGroup::Id),
        mode: e.mode.map(Mode),
        modified: e.modified,
        accessed: e.accessed,
        created: e.created,
        key: None,
        source: None,
    }
}

fn dotdot() -> File {
    File {
        name: "..".to_string(),
        size: None,
        allocated_size: None,
        device_id: None,
        inode: None,
        hard_links: None,
        is_dir: true,
        is_hidden: false,
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
    }
}

impl DiscVfs {
    async fn ensure_state(&self) -> Result<&DiscState, Error> {
        self.state
            .get_or_try_init(|| async {
                // One-shot progress message while the volume structures are
                // probed; clear on exit.
                let mut extra = std::collections::BTreeMap::new();
                extra.insert("path".to_string(), self.display_path.clone());
                self.reporter.report(Some(super::VfsProgress {
                    stage: "Reading disc image".into(),
                    processed: None,
                    total: None,
                    extra,
                }));
                struct ClearOnDrop<'a>(&'a Arc<dyn super::ProgressReporter>);
                impl Drop for ClearOnDrop<'_> {
                    fn drop(&mut self) {
                        self.0.report(None);
                    }
                }
                let _clear = ClearOnDrop(&self.reporter);

                let details = self.upstream.file_details(&self.image_path).await?;
                let cache = BlockCache {
                    upstream: self.upstream.clone(),
                    image_path: self.image_path.clone(),
                    image_size: details.size,
                    inner: Mutex::new(CacheInner::default()),
                };

                let mut op = ProbeOp::new(details.size);
                let mut fetched = Vec::new();
                let fs = loop {
                    match op.step(fetched).map_err(disc_err)? {
                        Step::Done(fs) => break fs,
                        Step::Need(ranges) => fetched = fetch_chunks(&cache, ranges).await?,
                    }
                };
                log::info!(
                    "disc: {} recognized as {} (label {:?})",
                    self.display_path,
                    fs.describe(),
                    fs.volume_label()
                );

                Ok(DiscState {
                    fs,
                    cache,
                    dirs: Mutex::new(HashMap::new()),
                })
            })
            .await
    }

    /// List a directory whose canonical resolved path is `key`, through the
    /// permanent cache.
    async fn list_resolved(
        &self,
        state: &DiscState,
        key: &str,
        dir: &Entry,
    ) -> Result<Arc<Vec<Entry>>, Error> {
        if let Some(hit) = state.dirs.lock().await.get(key) {
            return Ok(hit.clone());
        }
        let mut op = state.fs.list_dir(dir);
        let mut fetched = Vec::new();
        let entries = loop {
            match op.step(fetched).map_err(disc_err)? {
                Step::Done(entries) => break entries,
                Step::Need(ranges) => fetched = fetch_chunks(&state.cache, ranges).await?,
            }
        };
        let entries = Arc::new(entries);
        state
            .dirs
            .lock()
            .await
            .insert(key.to_string(), entries.clone());
        Ok(entries)
    }

    /// Resolve a path to (canonical key, entry), following intermediate
    /// symlinks always and the final one when `follow_last`.
    async fn resolve(
        &self,
        state: &DiscState,
        path: &Path,
        follow_last: bool,
    ) -> Result<(String, Entry), Error> {
        let mut comps = normalize_components(path.as_wire_str());
        let mut hops = 0usize;

        'restart: loop {
            let mut cur = state.fs.root().clone();
            let mut resolved: Vec<String> = Vec::new();
            let mut idx = 0usize;
            while idx < comps.len() {
                if cur.kind != EntryKind::Dir {
                    return Err(Error {
                        kind: crate::ErrorKind::NotADirectory,
                        message: format!("not a directory: /{}", resolved.join("/")),
                    });
                }
                let entries = self.list_resolved(state, &resolved.join("/"), &cur).await?;
                let name = &comps[idx];
                let entry = entries
                    .iter()
                    .find(|e| e.name == *name)
                    .ok_or_else(|| not_found(format!("no such file in disc image: {}", name)))?
                    .clone();
                let is_last = idx == comps.len() - 1;
                if entry.kind == EntryKind::Symlink
                    && (!is_last || follow_last)
                    && entry.link_target.as_deref().is_some_and(|t| !t.is_empty())
                {
                    hops += 1;
                    if hops > MAX_SYMLINK_HOPS {
                        return Err(Error::custom("too many levels of symbolic links"));
                    }
                    let target = entry.link_target.as_deref().unwrap_or_default();
                    let mut next = if target.starts_with('/') {
                        Vec::new()
                    } else {
                        resolved.clone()
                    };
                    for c in target.split('/') {
                        match c {
                            "" | "." => {}
                            ".." => {
                                next.pop();
                            }
                            c => next.push(c.to_string()),
                        }
                    }
                    next.extend(comps[idx + 1..].iter().cloned());
                    comps = next;
                    continue 'restart;
                }
                resolved.push(name.clone());
                cur = entry;
                idx += 1;
            }
            return Ok((resolved.join("/"), cur));
        }
    }

    /// For a symlink listing row, stat the target and mirror its `is_dir` /
    /// `size` — the lstat+stat pattern the local VFS and archives use.
    async fn fill_symlink_target(&self, state: &DiscState, dir_key: &str, file: &mut File) {
        let full = if dir_key.is_empty() {
            file.name.clone()
        } else {
            format!("{}/{}", dir_key, file.name)
        };
        let path = PathBuf::from_wire_str(&full);
        if let Ok((_, target)) = self.resolve(state, &path, true).await {
            file.is_dir = target.kind == EntryKind::Dir;
            file.size = (target.kind != EntryKind::Dir).then_some(target.size);
        }
    }

    /// Read `length` bytes at `offset` of a resolved file entry.
    async fn read_entry_range(
        &self,
        entry: &Entry,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, Error> {
        let total_size = entry.size;
        if offset >= total_size || length == 0 {
            return Ok(FileChunk {
                data: Vec::new(),
                offset,
                total_size,
            });
        }
        let length = length.min(total_size - offset);
        let data = match &entry.data {
            EntryData::Inline(data) => {
                let start = (offset as usize).min(data.len());
                let end = ((offset + length) as usize).min(data.len());
                data[start..end].to_vec()
            }
            EntryData::Extents(extents) => {
                let mut out = Vec::with_capacity(length as usize);
                for (piece_off, piece_len, kind) in slice_extents(extents, offset, length) {
                    match kind {
                        ExtentKind::Recorded => {
                            let chunk = self
                                .upstream
                                .read_range(&self.image_path, piece_off, piece_len)
                                .await?;
                            if (chunk.data.len() as u64) < piece_len {
                                return Err(Error::custom(
                                    "disc image truncated: extent read came up short",
                                ));
                            }
                            out.extend_from_slice(&chunk.data[..piece_len as usize]);
                        }
                        ExtentKind::Sparse => {
                            out.resize(out.len() + piece_len as usize, 0);
                        }
                    }
                }
                out
            }
        };
        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }

    async fn resolve_file(&self, path: &Path) -> Result<Entry, Error> {
        let state = self.ensure_state().await?;
        let (_, entry) = self.resolve(state, path, true).await?;
        if entry.kind == EntryKind::Dir {
            return Err(Error {
                kind: crate::ErrorKind::IsADirectory,
                message: format!("is a directory: {}", path),
            });
        }
        Ok(entry)
    }
}

#[async_trait::async_trait]
impl Vfs for DiscVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &DISC_VFS_DESCRIPTOR
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
        _batch_tx: Option<tokio::sync::mpsc::Sender<Vec<File>>>,
    ) -> Result<super::VfsFileList, Error> {
        let state = self.ensure_state().await?;
        let (key, dir) = self.resolve(state, path, true).await?;
        if dir.kind != EntryKind::Dir {
            return Err(Error {
                kind: crate::ErrorKind::NotADirectory,
                message: format!("not a directory: {}", path),
            });
        }
        let entries = self.list_resolved(state, &key, &dir).await?;
        let mut files = vec![dotdot()];
        for e in entries.iter() {
            let mut file = entry_to_file(e);
            if file.is_symlink {
                self.fill_symlink_target(state, &key, &mut file).await;
            }
            files.push(file);
        }
        Ok(files.into())
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        std::future::pending().await
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let state = self.ensure_state().await?;
        let (_, entry) = self.resolve(state, path, true).await?;
        Ok(FileDetails {
            size: if entry.kind == EntryKind::Dir {
                0
            } else {
                entry.size
            },
            mime_type: crate::file_reader::guess_mime_type(std::path::Path::new(
                path.as_wire_str(),
            )),
            is_dir: entry.kind == EntryKind::Dir,
            is_symlink: entry.kind == EntryKind::Symlink,
            symlink_target: entry.link_target.clone(),
            user: entry.uid.map(UserGroup::Id),
            group: entry.gid.map(UserGroup::Id),
            mode: entry.mode.map(Mode),
            modified: entry.modified,
            accessed: entry.accessed,
            created: entry.created,
        })
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let state = self.ensure_state().await?;
        let (key, entry) = self.resolve(state, path, false).await?;
        let mut file = entry_to_file(&entry);
        if file.is_symlink {
            let parent_key = match key.rfind('/') {
                Some(pos) => key[..pos].to_string(),
                None => String::new(),
            };
            self.fill_symlink_target(state, &parent_key, &mut file)
                .await;
        }
        Ok(file)
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let entry = self.resolve_file(path).await?;
        self.read_entry_range(&entry, offset, length).await
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>, Error> {
        let entry = self.resolve_file(path).await?;
        Ok(Box::new(ExtentReader::new(
            self.upstream.clone(),
            self.image_path.clone(),
            entry,
        )))
    }
}

// ---------------------------------------------------------------------------
// ExtentReader — AsyncRead over an entry's extents
// ---------------------------------------------------------------------------

type ChunkFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, Error>> + Send>>;

/// Streams a file out of the image by issuing `read_range` calls against
/// the upstream VFS, one `STREAM_CHUNK` at a time. Dropping the reader
/// drops any in-flight read — cancellation propagates naturally.
struct ExtentReader {
    upstream: Arc<dyn Vfs>,
    image_path: PathBuf,
    /// Remaining (image_offset, len, kind) pieces; inline data is
    /// pre-buffered instead.
    pieces: VecDeque<(u64, u64, ExtentKind)>,
    buf: std::io::Cursor<Vec<u8>>,
    inflight: Option<ChunkFuture>,
}

impl ExtentReader {
    fn new(upstream: Arc<dyn Vfs>, image_path: PathBuf, entry: Entry) -> Self {
        let (pieces, buf) = match entry.data {
            EntryData::Inline(mut data) => {
                data.truncate(entry.size as usize);
                (VecDeque::new(), data)
            }
            EntryData::Extents(extents) => {
                (slice_extents(&extents, 0, entry.size).into(), Vec::new())
            }
        };
        ExtentReader {
            upstream,
            image_path,
            pieces,
            buf: std::io::Cursor::new(buf),
            inflight: None,
        }
    }

    /// Pop up to `STREAM_CHUNK` bytes off the front of the piece queue and
    /// start a read for them.
    fn start_next(&mut self) -> Option<ChunkFuture> {
        let (off, len, kind) = self.pieces.pop_front()?;
        let take = len.min(STREAM_CHUNK);
        if take < len {
            self.pieces.push_front((off + take, len - take, kind));
        }
        match kind {
            ExtentKind::Sparse => Some(Box::pin(async move { Ok(vec![0u8; take as usize]) })),
            ExtentKind::Recorded => {
                let upstream = self.upstream.clone();
                let path = self.image_path.clone();
                Some(Box::pin(async move {
                    let chunk = upstream.read_range(&path, off, take).await?;
                    if (chunk.data.len() as u64) < take {
                        return Err(Error::custom(
                            "disc image truncated: extent read came up short",
                        ));
                    }
                    let mut data = chunk.data;
                    data.truncate(take as usize);
                    Ok(data)
                }))
            }
        }
    }
}

impl AsyncRead for ExtentReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            // Drain the current buffer first.
            let n = self
                .buf
                .read(out.initialize_unfilled())
                .expect("cursor read");
            if n > 0 {
                out.advance(n);
                return Poll::Ready(Ok(()));
            }

            let mut fut = match self.inflight.take() {
                Some(fut) => fut,
                None => match self.start_next() {
                    Some(fut) => fut,
                    None => return Poll::Ready(Ok(())), // EOF
                },
            };
            match fut.as_mut().poll(cx) {
                Poll::Pending => {
                    self.inflight = Some(fut);
                    return Poll::Pending;
                }
                Poll::Ready(Ok(data)) => {
                    self.buf = std::io::Cursor::new(data);
                }
                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
            }
        }
    }
}
