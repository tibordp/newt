//! ZIP archive VFS over the sans-IO reader in `newt_archive::zip`.
//!
//! Mirrors the disc-image VFS rather than the tar one: the central directory
//! is a complete index fetched in a few bounded reads at first use, entry
//! content is random-access (stored entries are pure extents; compressed
//! ones stream through a resumable decrypt→decompress cursor), and nothing
//! ever blocks a thread — every upstream access is a plain awaited read, so
//! dropping a future cancels it. Entry reads hold one `open_read_at` handle
//! on the archive for their whole pipeline; the mount-time probe uses
//! one-shot `read_range` (its ranges are few and fetched concurrently).

use std::collections::HashMap;
use std::future::Future;
// The ZIP index/directory tree is keyed by Unix-style relative path
// strings built on std paths; the `Vfs` surface speaks our
// `vfs::path::Path`. Convert at each trait-method boundary via
// `as_wire_str()` (leading `/` stripped by `normalize_dir_path`).
use std::path::{Path as StdPath, PathBuf as StdPathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use log::info;
use newt_archive::zip as zr;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::Error;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{File, FsStats, Mode, UserGroup};
use crate::vfs::{FileChunk, FileDetails};

use super::super::origin::{
    origin_breadcrumbs, origin_format_path, origin_mount_label, origin_try_parse_display_path,
};
use super::super::{
    Breadcrumb, DisplayPathMatch, MetadataTraits, RegisteredDescriptor, Vfs, VfsDescriptor,
    VfsPath, VfsRandomReader,
};
use super::tree::{DirectoryTree, ensure_ancestors, normalized_to_string};

/// Symlink targets are read eagerly at index time; anything larger than this
/// is not a plausible link target.
const MAX_SYMLINK_TARGET: u64 = 64 * 1024;

/// Parked decompression cursors kept per archive. Each holds a decompressor
/// window (tens of KiB) plus up to `OUT_HIGH_WATER` of read-ahead.
const MAX_CURSORS: usize = 6;

// ---------------------------------------------------------------------------
// ZipArchiveVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ZipArchiveVfsDescriptor;

impl VfsDescriptor for ZipArchiveVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "archive_zip"
    }
    fn display_name(&self) -> &'static str {
        "Archive (ZIP)"
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
    fn metadata_traits(&self, _mount_meta: &[u8]) -> MetadataTraits {
        // Unix-made zips carry mode (and often uid/gid via the Info-ZIP
        // extra); DOS-made ones simply leave the columns empty.
        MetadataTraits {
            unix_owner: true,
            windows_attributes: false,
        }
    }
}

static ZIP_ARCHIVE_VFS_DESCRIPTOR: ZipArchiveVfsDescriptor = ZipArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&ZIP_ARCHIVE_VFS_DESCRIPTOR));

#[cfg(test)]
#[path = "zip_tests.rs"]
mod zip_tests;

// ---------------------------------------------------------------------------
// ZipArchiveVfs
// ---------------------------------------------------------------------------

pub struct ZipArchiveVfs {
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    /// Pretty rendering of the archive's origin path, used in askpass
    /// prompts when an encrypted entry needs unlocking.
    display_path: String,
    /// Optional askpass provider used to prompt for the archive password
    /// the first time an encrypted entry is read. Without this, reads of
    /// encrypted entries fail with `PermissionDenied`.
    askpass: Option<Arc<dyn crate::askpass::AskpassProvider>>,
    reporter: Arc<dyn super::super::ProgressReporter>,
    /// Cached password for encrypted entries. Filled on first successful
    /// verification. The ZIP spec allows different passwords per entry; we
    /// remember the most recently successful one and re-prompt if it
    /// fails on a later entry.
    password: tokio::sync::Mutex<Option<Vec<u8>>>,
    /// Bumped every time the user dismisses an unlock prompt. Pending
    /// reads that started before the dismissal observe a higher
    /// generation than the one they captured at entry and bail out with
    /// `Cancelled` instead of opening a fresh prompt — so dismissing a
    /// single dialog cancels the whole "batch" of concurrent reads (e.g.
    /// the chunked range reads the file viewer fans out on F3) rather
    /// than queueing N more prompts behind it.
    dismiss_gen: std::sync::atomic::AtomicU64,
    state: tokio::sync::OnceCell<ZipState>,
    /// Resolved local headers, by entry name — one small upstream read
    /// each, remembered permanently (the archive is immutable while
    /// mounted).
    opens: parking_lot::Mutex<HashMap<String, zr::OpenEntry>>,
    /// Verified per-entry keys (PBKDF2 output / ZipCrypto registers).
    keys: parking_lot::Mutex<HashMap<String, zr::EntryKey>>,
    /// Parked decompression cursors, LRU at the back. Sequential range
    /// reads (the F3 viewer's chunk fan-out) resume the matching cursor
    /// instead of re-decompressing the entry from the start.
    cursors: parking_lot::Mutex<Vec<(String, zr::EntryReader)>>,
}

struct ZipState {
    fs: zr::ZipFs,
    by_name: HashMap<String, usize>,
    tree: DirectoryTree,
}

impl ZipArchiveVfs {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        origin: VfsPath,
        mount_meta: Vec<u8>,
        display_path: String,
        askpass: Option<Arc<dyn crate::askpass::AskpassProvider>>,
        reporter: Arc<dyn super::super::ProgressReporter>,
    ) -> Self {
        Self {
            upstream,
            archive_path,
            origin,
            mount_meta,
            display_path,
            askpass,
            reporter,
            password: tokio::sync::Mutex::new(None),
            dismiss_gen: std::sync::atomic::AtomicU64::new(0),
            state: tokio::sync::OnceCell::new(),
            opens: parking_lot::Mutex::new(HashMap::new()),
            keys: parking_lot::Mutex::new(HashMap::new()),
            cursors: parking_lot::Mutex::new(Vec::new()),
        }
    }

    async fn ensure_state(&self) -> Result<&ZipState, Error> {
        self.state
            .get_or_try_init(|| async {
                info!("archive: indexing ZIP archive {}", self.archive_path);
                struct ClearOnDrop<'a>(&'a Arc<dyn super::super::ProgressReporter>);
                impl Drop for ClearOnDrop<'_> {
                    fn drop(&mut self) {
                        self.0.report(None);
                    }
                }
                let _clear = ClearOnDrop(&self.reporter);

                let details = self.upstream.file_details(&self.archive_path).await?;
                let mut op = zr::ZipProbeOp::new(details.size);
                let mut fetched = Vec::new();
                let fs = loop {
                    self.report_indexing(&op.progress());
                    match op.step(fetched).map_err(zip_err)? {
                        zr::Step::Done(fs) => break fs,
                        zr::Step::Need(ranges) => {
                            fetched =
                                fetch_ranges(&self.upstream, &self.archive_path, ranges).await?;
                        }
                    }
                };
                info!(
                    "archive: indexed {} entries from ZIP {}",
                    fs.entries.len(),
                    self.archive_path
                );

                let targets = self.read_symlink_targets(&fs).await;
                let (tree, by_name) = build_tree(&fs, &targets);
                Ok(ZipState { fs, by_name, tree })
            })
            .await
    }

    fn report_indexing(&self, progress: &zr::ProbeProgress) {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("path".to_string(), self.display_path.clone());
        if progress.entries > 0 {
            extra.insert("entries".to_string(), progress.entries.to_string());
        }
        self.reporter.report(Some(super::super::VfsProgress {
            stage: "Indexing".into(),
            processed: (progress.cd_bytes_total > 0).then_some(progress.cd_bytes_done),
            total: (progress.cd_bytes_total > 0).then_some(progress.cd_bytes_total),
            extra,
        }));
    }

    /// Symlink targets are entry *content* in ZIP, so listing an archive
    /// with links needs a read per link. They are tiny and fetched
    /// concurrently; failures (or encrypted links) leave the target unset
    /// and the entry renders as a broken link.
    async fn read_symlink_targets(&self, fs: &zr::ZipFs) -> HashMap<String, String> {
        let candidates = fs.entries.iter().filter(|e| {
            e.kind == zr::EntryKind::Symlink
                && e.size > 0
                && e.size <= MAX_SYMLINK_TARGET
                && matches!(e.encryption, zr::Encryption::None)
        });
        let reads = candidates.map(|entry| async move {
            let mut upstream = self.upstream.open_read_at(&self.archive_path).await.ok()?;
            let open = drive_open(upstream.as_mut(), entry, fs.file_size)
                .await
                .ok()?;
            let reader = zr::EntryReader::new(entry, &open, None, 0).ok()?;
            let (data, _) = drive_reader(upstream.as_mut(), reader, entry.size)
                .await
                .ok()?;
            Some((
                entry.name.clone(),
                String::from_utf8_lossy(&data).into_owned(),
            ))
        });
        futures::future::join_all(reads)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    fn resolve_entry<'a>(
        &self,
        state: &'a ZipState,
        path: &Path,
        follow_last: bool,
    ) -> Result<&'a zr::ZipEntry, Error> {
        let resolved = state
            .tree
            .resolve_path(StdPath::new(path.as_wire_str()), follow_last)?;
        let key = normalized_to_string(&resolved);
        state
            .by_name
            .get(&key)
            .map(|&i| &state.fs.entries[i])
            .ok_or_else(|| Error::not_found(format!("file not found in archive: {}", key)))
    }

    fn cached_open(&self, entry: &zr::ZipEntry) -> Option<zr::OpenEntry> {
        self.opens.lock().get(&entry.name).cloned()
    }

    async fn open_entry(
        &self,
        state: &ZipState,
        entry: &zr::ZipEntry,
        upstream: &mut dyn VfsRandomReader,
    ) -> Result<zr::OpenEntry, Error> {
        if let Some(open) = self.cached_open(entry) {
            return Ok(open);
        }
        let open = drive_open(upstream, entry, state.fs.file_size).await?;
        self.opens.lock().insert(entry.name.clone(), open.clone());
        Ok(open)
    }

    /// Obtain the decryption key for an encrypted entry, prompting for the
    /// archive password if needed. Password verification is against the
    /// entry's cheap verifier — no decompression involved.
    ///
    /// Concurrency: the prompt-and-verify phase is serialised by the
    /// password mutex. To prevent N concurrent reads from all queueing
    /// up their own prompts after the user dismisses one of them, we
    /// snapshot a "dismiss generation" before queueing and bail out if
    /// it has advanced by the time we hold the lock — a fresh read
    /// (started after the dismissal) sees the new generation and is
    /// allowed to prompt.
    async fn entry_key(
        &self,
        entry: &zr::ZipEntry,
        open: &zr::OpenEntry,
    ) -> Result<zr::EntryKey, Error> {
        use std::sync::atomic::Ordering;

        if let Some(key) = self.keys.lock().get(&entry.name) {
            return Ok(key.clone());
        }

        // Snapshot the dismissal counter at task entry — *before* any
        // lock or prompt. This pins our "birth time" relative to
        // dismissal events: peers that increment the counter while
        // we're queued on the prompt lock will then make our post-lock
        // check trip and we'll bail instead of opening a fresh prompt.
        let my_gen = self.dismiss_gen.load(Ordering::Acquire);

        let try_password = |password: &[u8]| -> Option<zr::EntryKey> {
            let key = open.verify_password(entry, password).ok()?;
            self.keys.lock().insert(entry.name.clone(), key.clone());
            Some(key)
        };

        // Fast path: the cached password, without serialising on the
        // prompt lock once the archive has been unlocked.
        if let Some(pw) = self.password.lock().await.clone()
            && let Some(key) = try_password(&pw)
        {
            return Ok(key);
        }

        let mut guard = self.password.lock().await;
        if self.dismiss_gen.load(Ordering::Acquire) > my_gen {
            // A peer dismissed an unlock prompt while we were queued.
            // Treat the whole batch as cancelled rather than queueing N
            // more prompts behind theirs.
            return Err(Error::cancelled());
        }
        // Did a peer set a (different) password while we waited?
        if let Some(pw) = guard.clone()
            && let Some(key) = try_password(&pw)
        {
            return Ok(key);
        }

        let askpass = self.askpass.as_ref().ok_or_else(|| Error {
            kind: crate::ErrorKind::PermissionDenied,
            message: format!(
                "ZIP archive {} entry is encrypted, but no askpass provider is configured",
                self.display_path
            ),
        })?;
        let mut prompt = format!("Password for archive {}:", self.display_path);
        loop {
            let resp = askpass
                .prompt(crate::askpass::AskpassRequest {
                    prompt_type: crate::askpass::PromptType::Secret,
                    prompt: prompt.clone(),
                })
                .await;
            let Some(s) = resp.0 else {
                self.dismiss_gen.fetch_add(1, Ordering::Release);
                return Err(Error::cancelled());
            };
            let bytes = s.into_bytes();
            if let Some(key) = try_password(&bytes) {
                *guard = Some(bytes);
                return Ok(key);
            }
            prompt = format!(
                "Incorrect password — try again. Password for archive {}:",
                self.display_path
            );
        }
    }

    /// Key for the entry when one is needed, `None` for cleartext entries.
    async fn key_if_encrypted(
        &self,
        entry: &zr::ZipEntry,
        open: &zr::OpenEntry,
    ) -> Result<Option<zr::EntryKey>, Error> {
        if !open.needs_password() {
            return Ok(None);
        }
        Ok(Some(self.entry_key(entry, open).await?))
    }

    /// Best parked cursor for a read of `entry` at `offset`: same entry,
    /// positioned at or before the offset, closest to it.
    fn take_cursor(&self, name: &str, offset: u64) -> Option<zr::EntryReader> {
        let mut cursors = self.cursors.lock();
        let best = cursors
            .iter()
            .enumerate()
            .filter(|(_, (n, r))| n == name && r.position() <= offset)
            .max_by_key(|(_, (_, r))| r.position())
            .map(|(i, _)| i)?;
        Some(cursors.remove(best).1)
    }

    fn park_cursor(&self, name: String, reader: zr::EntryReader) {
        let mut cursors = self.cursors.lock();
        cursors.push((name, reader));
        if cursors.len() > MAX_CURSORS {
            cursors.remove(0);
        }
    }
}

/// Map reader errors; password conditions keep their kind so the askpass
/// retry loop and callers can tell them apart.
fn zip_err(e: zr::ZipError) -> Error {
    match e {
        zr::ZipError::PasswordRequired | zr::ZipError::WrongPassword => Error {
            kind: crate::ErrorKind::PermissionDenied,
            message: e.to_string(),
        },
        other => Error::custom(other.to_string()),
    }
}

/// Fetch a probe batch concurrently — the ranges in one `Need` are
/// independent by contract.
async fn fetch_ranges(
    upstream: &Arc<dyn Vfs>,
    archive_path: &PathBuf,
    ranges: Vec<std::ops::Range<u64>>,
) -> Result<Vec<zr::Chunk>, Error> {
    futures::future::try_join_all(ranges.into_iter().map(|r| async move {
        let chunk = upstream
            .read_range(archive_path, r.start, r.end - r.start)
            .await?;
        Ok(zr::Chunk {
            offset: r.start,
            data: chunk.data,
        })
    }))
    .await
}

/// Fetch a batch over the held handle. Sequential — a batch here is one or
/// two small header reads, not worth a handle per range.
async fn fetch_ranges_at(
    upstream: &mut dyn VfsRandomReader,
    ranges: Vec<std::ops::Range<u64>>,
) -> Result<Vec<zr::Chunk>, Error> {
    let mut out = Vec::with_capacity(ranges.len());
    for r in ranges {
        let data = upstream.read_at(r.start, r.end - r.start).await?;
        out.push(zr::Chunk {
            offset: r.start,
            data,
        });
    }
    Ok(out)
}

async fn drive_open(
    upstream: &mut dyn VfsRandomReader,
    entry: &zr::ZipEntry,
    file_size: u64,
) -> Result<zr::OpenEntry, Error> {
    let mut op = zr::EntryOpenOp::new(entry, file_size).map_err(zip_err)?;
    let mut fetched = Vec::new();
    loop {
        match op.step(fetched).map_err(zip_err)? {
            zr::Step::Done(open) => return Ok(open),
            zr::Step::Need(ranges) => {
                fetched = fetch_ranges_at(upstream, ranges).await?;
            }
        }
    }
}

/// Pull up to `want` bytes from the reader's current position, returning the
/// reader for the caller to park.
async fn drive_reader(
    upstream: &mut dyn VfsRandomReader,
    mut reader: zr::EntryReader,
    want: u64,
) -> Result<(Vec<u8>, zr::EntryReader), Error> {
    let mut out: Vec<u8> = Vec::new();
    let mut pending: Option<zr::Chunk> = None;
    loop {
        if reader.buffered() > 0 {
            out.extend(reader.take_output(want as usize - out.len()));
            if out.len() as u64 >= want {
                return Ok((out, reader));
            }
            continue;
        }
        match reader.step(pending.take()).map_err(zip_err)? {
            zr::ReadStep::Need(range) => {
                let len = range.end - range.start;
                let data = upstream.read_at(range.start, len).await?;
                if (data.len() as u64) < len {
                    return Err(Error::custom("ZIP archive truncated: read came up short"));
                }
                pending = Some(zr::Chunk {
                    offset: range.start,
                    data,
                });
            }
            zr::ReadStep::Output => {}
            zr::ReadStep::Done => return Ok((out, reader)),
        }
    }
}

/// Project the entry table into the shared archive `DirectoryTree`, plus a
/// name → entry-index map for reads.
fn build_tree(
    fs: &zr::ZipFs,
    symlink_targets: &HashMap<String, String>,
) -> (DirectoryTree, HashMap<String, usize>) {
    let mut dirs: HashMap<StdPathBuf, Vec<File>> = HashMap::new();
    let mut seen_dirs: std::collections::HashSet<StdPathBuf> = std::collections::HashSet::new();
    let mut by_name = HashMap::new();

    dirs.insert(StdPathBuf::from(""), Vec::new());
    seen_dirs.insert(StdPathBuf::from(""));

    for (index, entry) in fs.entries.iter().enumerate() {
        let entry_path = StdPathBuf::from(&entry.name);
        let parent = entry_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let Some(name) = entry_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
        else {
            continue;
        };
        by_name.insert(entry.name.clone(), index);

        ensure_ancestors(&mut dirs, &mut seen_dirs, &parent);

        let is_dir = entry.kind == zr::EntryKind::Dir;
        let file = File {
            attributes: None,
            name: name.clone(),
            size: (!is_dir).then_some(entry.size),
            allocated_size: None,
            device_id: None,
            inode: None,
            hard_links: None,
            is_dir,
            is_hidden: name.starts_with('.') || entry.hidden,
            is_symlink: entry.kind == zr::EntryKind::Symlink,
            symlink_target: symlink_targets.get(&entry.name).cloned(),
            user: entry.uid.map(UserGroup::Id),
            group: entry.gid.map(UserGroup::Id),
            mode: entry.mode.map(Mode),
            modified: entry.modified,
            accessed: entry.accessed,
            created: entry.created,
            key: None,
            source: None,
        };

        if is_dir && seen_dirs.contains(&entry_path) {
            // Already added as an implicit ancestor — replace the synthetic
            // entry with real metadata.
            if let Some(children) = dirs.get_mut(&parent)
                && let Some(existing) = children.iter_mut().find(|f| f.name == name)
            {
                *existing = file;
            }
            continue;
        }

        dirs.entry(parent).or_default().push(file);
        if is_dir {
            seen_dirs.insert(entry_path.clone());
            dirs.entry(entry_path).or_default();
        }
    }

    (DirectoryTree { dirs }, by_name)
}

#[async_trait::async_trait]
impl Vfs for ZipArchiveVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &ZIP_ARCHIVE_VFS_DESCRIPTOR
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
        _batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::super::VfsFileList, Error> {
        let state = self.ensure_state().await?;
        // The directory tree is keyed by Unix-style relative strings;
        // feed the wire form to its std-path-based lookups.
        Ok(state.tree.list(StdPath::new(path.as_wire_str()))?.into())
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        std::future::pending().await
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let state = self.ensure_state().await?;
        let entry = self.resolve_entry(state, path, true)?;
        let is_dir = entry.kind == zr::EntryKind::Dir;
        Ok(FileDetails {
            size: if is_dir { 0 } else { entry.size },
            mime_type: crate::vfs::file::guess_mime_type(StdPath::new(path.as_wire_str())),
            is_dir,
            is_symlink: entry.kind == zr::EntryKind::Symlink,
            symlink_target: None,
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
        state.tree.file_info(StdPath::new(path.as_wire_str()))
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        let state = self.ensure_state().await?;
        let entry = self.resolve_entry(state, path, true)?;
        let mut upstream = self.upstream.open_read_at(&self.archive_path).await?;
        let open = self.open_entry(state, entry, upstream.as_mut()).await?;
        let key = self.key_if_encrypted(entry, &open).await?;
        let reader = zr::EntryReader::new(entry, &open, key.as_ref(), 0).map_err(zip_err)?;
        Ok(Box::new(ZipStreamingReader {
            upstream: Some(upstream),
            reader,
            inflight: None,
            done: false,
        }))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let state = self.ensure_state().await?;
        let entry = self.resolve_entry(state, path, true)?;
        let total_size = entry.size;
        if offset >= total_size || length == 0 {
            return Ok(FileChunk {
                data: Vec::new(),
                offset,
                total_size,
            });
        }
        let want = length.min(total_size - offset);

        // The handle is opened only when something actually pipelines
        // through it — a cached header + stored extent stays one-shot.
        let mut upstream: Option<Box<dyn VfsRandomReader>> = None;
        let open = match self.cached_open(entry) {
            Some(open) => open,
            None => {
                let handle = upstream
                    .insert(self.upstream.open_read_at(&self.archive_path).await?)
                    .as_mut();
                let open = drive_open(handle, entry, state.fs.file_size).await?;
                self.opens.lock().insert(entry.name.clone(), open.clone());
                open
            }
        };

        // Stored, unencrypted entries are extents: one upstream read, no
        // pipeline (mirroring how the disc VFS reads file content).
        if let Some(extent) = open.plain_extent(entry) {
            let chunk = self
                .upstream
                .read_range(&self.archive_path, extent.start + offset, want)
                .await?;
            let mut data = chunk.data;
            data.truncate(want as usize);
            return Ok(FileChunk {
                data,
                offset,
                total_size,
            });
        }

        let key = self.key_if_encrypted(entry, &open).await?;
        let mut upstream = match upstream {
            Some(handle) => handle,
            None => self.upstream.open_read_at(&self.archive_path).await?,
        };
        let reader = match self.take_cursor(&entry.name, offset) {
            Some(mut cursor) => {
                cursor.seek_forward(offset).map_err(zip_err)?;
                cursor
            }
            None => zr::EntryReader::new(entry, &open, key.as_ref(), offset).map_err(zip_err)?,
        };
        let (data, reader) = drive_reader(upstream.as_mut(), reader, want).await?;
        self.park_cursor(entry.name.clone(), reader);
        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }
}

// ---------------------------------------------------------------------------
// ZipStreamingReader — AsyncRead over an entry's plaintext
// ---------------------------------------------------------------------------

type ChunkFuture =
    Pin<Box<dyn Future<Output = (Box<dyn VfsRandomReader>, Result<zr::Chunk, Error>)> + Send>>;

/// Drives an [`zr::EntryReader`] with positioned reads on one held-open
/// upstream handle. The in-flight future owns the handle and hands it back
/// with the result — exactly one of `upstream`/`inflight` holds it at any
/// time. Dropping the reader drops any in-flight read (and with it the
/// handle) — cancellation propagates naturally. Streams from offset 0, so
/// the entry's CRC (and AES HMAC) are verified when the stream is read to
/// completion; failures surface as read errors.
struct ZipStreamingReader {
    upstream: Option<Box<dyn VfsRandomReader>>,
    reader: zr::EntryReader,
    inflight: Option<ChunkFuture>,
    done: bool,
}

impl AsyncRead for ZipStreamingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if self.reader.buffered() > 0 {
                let data = self.reader.take_output(out.remaining());
                out.put_slice(&data);
                return Poll::Ready(Ok(()));
            }
            if self.done {
                return Poll::Ready(Ok(())); // EOF
            }

            let fetched = if let Some(mut fut) = self.inflight.take() {
                match fut.as_mut().poll(cx) {
                    Poll::Pending => {
                        self.inflight = Some(fut);
                        return Poll::Pending;
                    }
                    Poll::Ready((handle, result)) => {
                        self.upstream = Some(handle);
                        match result {
                            Ok(chunk) => Some(chunk),
                            Err(e) => {
                                return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                            }
                        }
                    }
                }
            } else {
                None
            };

            match self.reader.step(fetched) {
                Ok(zr::ReadStep::Need(range)) => {
                    let mut handle = self
                        .upstream
                        .take()
                        .expect("zip streaming reader lost its upstream handle");
                    self.inflight = Some(Box::pin(async move {
                        let len = range.end - range.start;
                        let result = match handle.read_at(range.start, len).await {
                            Ok(data) if (data.len() as u64) < len => {
                                Err(Error::custom("ZIP archive truncated: read came up short"))
                            }
                            Ok(data) => Ok(zr::Chunk {
                                offset: range.start,
                                data,
                            }),
                            Err(e) => Err(e),
                        };
                        (handle, result)
                    }));
                }
                Ok(zr::ReadStep::Output) => {}
                Ok(zr::ReadStep::Done) => {
                    self.done = true;
                }
                Err(e) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
            }
        }
    }
}
