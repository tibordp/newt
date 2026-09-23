//! 7z archive VFS over the sans-IO reader in `newt_archive::sevenz` and
//! iluvatar's stream layer.
//!
//! The probe fetches the header in a few bounded reads at first use (and
//! prompts for a password there if the header itself is encrypted). Entry
//! content lives in folders: one compressed stream each, many entries back
//! to back in the solid case. Every folder gets a checkpoint index built
//! lazily, only as far as reads reach, so a cold read deep into a big solid
//! folder decodes once and leaves checkpoints behind; sequential reads
//! resume parked readers instead of restoring anything. Nothing blocks a
//! thread — every upstream access is an awaited read, so dropping a future
//! cancels it.

use std::collections::HashMap;
use std::ops::Range;
// The directory tree is keyed by Unix-style relative path strings built on
// std paths; the `Vfs` surface speaks our `vfs::path::Path`.
use std::path::{Path as StdPath, PathBuf as StdPathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iluvatar::{
    Bcj2Streams, EngineRequest, FixedInterval, StreamIndex, StreamIndexer, StreamReader,
};
use log::info;
use newt_archive::sevenz as sz;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::Error;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{File, FsStats, Mode};
use crate::vfs::{FileChunk, FileDetails};

use super::super::open_read_at;
use super::super::origin::{
    origin_breadcrumbs, origin_format_path, origin_mount_label, origin_try_parse_display_path,
};
use super::super::pipelined_read::PipelinedReader;
use super::super::{
    Breadcrumb, DisplayPathMatch, MetadataTraits, RegisteredDescriptor, Vfs, VfsDescriptor,
    VfsPath, VfsRandomReader,
};
use super::stream::{PACKED_SLICE, ReaderPool, StreamDriver, drive_reader, read_packed};
use super::tree::{DirectoryTree, ensure_ancestors, normalized_to_string};

/// Symlink targets are read eagerly at index time; anything larger than this
/// is not a plausible link target.
const MAX_SYMLINK_TARGET: u64 = 64 * 1024;

/// Checkpoint state a folder's index may hold. An LZMA checkpoint carries
/// the live dictionary (up to 64 MiB at ultra settings), so this decides
/// how many checkpoints a big solid folder gets, not how fine they are.
const INDEX_BUDGET: u64 = 256 << 20;

const MIN_INTERVAL: u64 = 1 << 20;

/// How much of a folder a password is verified against: a wrong key turns
/// LZMA2 into garbage within its first chunk.
const VERIFY_BYTES: u64 = 256 << 10;

// ---------------------------------------------------------------------------
// SevenZArchiveVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SevenZArchiveVfsDescriptor;

impl VfsDescriptor for SevenZArchiveVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "archive_7z"
    }
    fn display_name(&self) -> &'static str {
        "Archive (7z)"
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
    fn metadata_traits(&self, _mount_meta: &[u8]) -> MetadataTraits {
        // Unix-made archives carry a mode in the attributes' high half;
        // there is no owner in the format.
        MetadataTraits {
            unix_owner: true,
            windows_attributes: false,
        }
    }
}

static SEVENZ_ARCHIVE_VFS_DESCRIPTOR: SevenZArchiveVfsDescriptor = SevenZArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&SEVENZ_ARCHIVE_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// SevenZArchiveVfs
// ---------------------------------------------------------------------------

pub struct SevenZArchiveVfs {
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    display_path: String,
    askpass: Option<Arc<dyn crate::askpass::AskpassProvider>>,
    reporter: Arc<dyn super::super::ProgressReporter>,
    /// The archive password once it has unlocked anything. 7z derives one
    /// key per folder from it, so the string is what gets remembered.
    password: tokio::sync::Mutex<Option<String>>,
    /// Bumped when the user dismisses an unlock prompt; reads that started
    /// before the dismissal bail out instead of prompting again, so one
    /// dismissal cancels a whole batch of concurrent reads.
    dismiss_gen: AtomicU64,
    state: tokio::sync::OnceCell<SevenZState>,
}

struct SevenZState {
    fs: sz::SevenZFs,
    by_name: HashMap<String, usize>,
    tree: DirectoryTree,
    folders: Vec<Folder>,
}

struct Folder {
    /// Guarded by an async mutex because indexing awaits upstream reads
    /// while holding it.
    state: tokio::sync::Mutex<FolderState>,
    pool: Arc<ReaderPool>,
}

/// Per-folder decoding state.
#[derive(Default)]
struct FolderState {
    key: Option<[u8; 32]>,
    /// BCJ2's side streams, decoded whole at the folder's first read.
    sides: Option<Arc<Bcj2Streams>>,
    /// Built on first read; grows with `extend_index`.
    index: Option<StreamIndex>,
}

impl SevenZArchiveVfs {
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
            dismiss_gen: AtomicU64::new(0),
            state: tokio::sync::OnceCell::new(),
        }
    }

    async fn ensure_state(&self) -> Result<&SevenZState, Error> {
        // Callers queued behind a header prompt the user then dismissed
        // bail out in turn instead of each prompting again.
        let my_gen = self.dismiss_gen.load(Ordering::Acquire);
        self.state
            .get_or_try_init(|| async {
                if self.dismiss_gen.load(Ordering::Acquire) > my_gen {
                    return Err(Error::cancelled());
                }
                info!("archive: indexing 7z archive {}", self.archive_path);
                struct ClearOnDrop<'a>(&'a Arc<dyn super::super::ProgressReporter>);
                impl Drop for ClearOnDrop<'_> {
                    fn drop(&mut self) {
                        self.0.report(None);
                    }
                }
                let _clear = ClearOnDrop(&self.reporter);

                let fs = self.probe().await?;
                info!(
                    "archive: indexed {} entries in {} folders from 7z {}",
                    fs.entries.len(),
                    fs.folders.len(),
                    self.archive_path
                );
                let folders = fs
                    .folders
                    .iter()
                    .map(|info| Folder {
                        state: tokio::sync::Mutex::new(FolderState::default()),
                        pool: Arc::new(ReaderPool::new(ReaderPool::max_for_cost(
                            info.checkpoint_cost(),
                        ))),
                    })
                    .collect::<Vec<_>>();
                let targets = self.read_symlink_targets(&fs, &folders).await;
                let (tree, by_name) = build_tree(&fs, &targets);
                Ok(SevenZState {
                    fs,
                    by_name,
                    tree,
                    folders,
                })
            })
            .await
    }

    /// Drive the probe, prompting for the password if the header is
    /// encrypted (a wrong one re-prompts with a hint; dismissing fails the
    /// mount).
    async fn probe(&self) -> Result<sz::SevenZFs, Error> {
        let details = self.upstream.file_details(&self.archive_path).await?;
        let mut op = sz::SevenZProbeOp::new(details.size);
        let mut fetched = Vec::new();
        let mut hint = false;
        loop {
            self.report_indexing(&op.progress());
            match op.step(fetched) {
                Ok(sz::ProbeStep::Done(fs)) => return Ok(fs),
                Ok(sz::ProbeStep::Need(ranges)) => {
                    fetched = fetch_ranges(&self.upstream, &self.archive_path, ranges).await?;
                }
                Ok(sz::ProbeStep::NeedPassword) | Err(sz::SevenZError::WrongPassword) => {
                    let password = self.prompt_password(hint).await?;
                    let params = op
                        .aes_params()
                        .ok_or_else(|| Error::custom("7z header wants a key without parameters"))?;
                    op.set_key(sz::derive_key(&password, params));
                    *self.password.lock().await = Some(password);
                    hint = true;
                    fetched = Vec::new();
                }
                Err(e) => return Err(sz_err(e)),
            }
        }
    }

    fn report_indexing(&self, progress: &sz::ProbeProgress) {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("path".to_string(), self.display_path.clone());
        self.reporter.report(Some(super::super::VfsProgress {
            stage: "Indexing".into(),
            processed: (progress.header_bytes_total > 0).then_some(progress.header_bytes_done),
            total: (progress.header_bytes_total > 0).then_some(progress.header_bytes_total),
            extra,
        }));
    }

    fn report_folder(&self, folder: usize, total_folders: usize, done: u64, total: u64) {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("path".to_string(), self.display_path.clone());
        extra.insert(
            "folder".to_string(),
            format!("{} of {}", folder + 1, total_folders),
        );
        self.reporter.report(Some(super::super::VfsProgress {
            stage: "Decoding".into(),
            processed: Some(done),
            total: Some(total),
            extra,
        }));
    }

    /// One prompt; `hint` marks a retry after a wrong password.
    async fn prompt_password(&self, hint: bool) -> Result<String, Error> {
        let askpass = self.askpass.as_ref().ok_or_else(|| Error {
            kind: crate::ErrorKind::PermissionDenied,
            message: format!(
                "7z archive {} is encrypted, but no askpass provider is configured",
                self.display_path
            ),
        })?;
        let prompt = if hint {
            format!(
                "Incorrect password — try again. Password for archive {}:",
                self.display_path
            )
        } else {
            format!("Password for archive {}:", self.display_path)
        };
        let resp = askpass
            .prompt(crate::askpass::AskpassRequest {
                prompt_type: crate::askpass::PromptType::Secret,
                prompt,
            })
            .await;
        match resp.0 {
            Some(s) => Ok(s),
            None => {
                self.dismiss_gen.fetch_add(1, Ordering::Release);
                Err(Error::cancelled())
            }
        }
    }

    /// Symlink targets are entry content, so listing an archive with links
    /// needs a read per link. Links in encrypted folders stay unresolved
    /// rather than prompting at mount time.
    async fn read_symlink_targets(
        &self,
        fs: &sz::SevenZFs,
        folders: &[Folder],
    ) -> HashMap<String, String> {
        let mut targets = HashMap::new();
        for entry in &fs.entries {
            let Some(loc) = entry.location else { continue };
            if entry.kind != sz::EntryKind::Symlink
                || entry.size > MAX_SYMLINK_TARGET
                || fs.folders[loc.folder].aes.is_some()
            {
                continue;
            }
            let Ok(mut upstream) = open_read_at(&self.upstream, &self.archive_path).await else {
                continue;
            };
            let folder = &folders[loc.folder];
            let mut state = folder.state.lock().await;
            let Ok(reader) = self
                .prepare_reader(
                    fs,
                    loc.folder,
                    &mut state,
                    &folder.pool,
                    upstream.as_mut(),
                    loc.offset,
                    entry.size,
                    self.dismiss_gen.load(Ordering::Acquire),
                )
                .await
            else {
                continue;
            };
            let info = &fs.folders[loc.folder];
            if let Ok((data, reader)) =
                drive_reader(upstream.as_mut(), &info.packed, reader, entry.size).await
            {
                folder.pool.park(reader);
                targets.insert(
                    entry.name.clone(),
                    String::from_utf8_lossy(&data).into_owned(),
                );
            }
        }
        targets
    }

    fn resolve_entry<'a>(
        &self,
        state: &'a SevenZState,
        path: &Path,
        follow_last: bool,
    ) -> Result<&'a sz::SevenZEntry, Error> {
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

    /// The folder's key, prompting (and verifying by trial decode) when
    /// the folder is encrypted and nothing has unlocked it yet. `my_gen`
    /// is the caller's `dismiss_gen` snapshot from before it queued on
    /// any lock: a read that waited out a prompt the user dismissed bails
    /// instead of prompting again in turn.
    async fn folder_key(
        &self,
        fs: &sz::SevenZFs,
        folder_index: usize,
        folder: &mut FolderState,
        upstream: &mut dyn VfsRandomReader,
        my_gen: u64,
    ) -> Result<Option<[u8; 32]>, Error> {
        let info = &fs.folders[folder_index];
        let Some(params) = &info.aes else {
            return Ok(None);
        };
        if let Some(key) = folder.key {
            return Ok(Some(key));
        }

        // The remembered password first, without a prompt.
        if let Some(pw) = self.password.lock().await.clone() {
            let key = sz::derive_key(&pw, params);
            if verify_key(info, key, upstream).await? {
                folder.key = Some(key);
                return Ok(Some(key));
            }
        }
        let mut guard = self.password.lock().await;
        if self.dismiss_gen.load(Ordering::Acquire) > my_gen {
            return Err(Error::cancelled());
        }
        let mut hint = false;
        loop {
            let password = self.prompt_password(hint).await?;
            let key = sz::derive_key(&password, params);
            if verify_key(info, key, upstream).await? {
                *guard = Some(password);
                folder.key = Some(key);
                return Ok(Some(key));
            }
            hint = true;
        }
    }

    /// A reader positioned at `offset` in the folder's unpacked stream,
    /// serving `len` bytes: a parked reader close before the target, else a
    /// restore from the nearest checkpoint, extending the folder's index
    /// first when the target lies beyond it.
    #[allow(clippy::too_many_arguments)]
    async fn prepare_reader(
        &self,
        fs: &sz::SevenZFs,
        folder_index: usize,
        folder: &mut FolderState,
        pool: &ReaderPool,
        upstream: &mut dyn VfsRandomReader,
        offset: u64,
        len: u64,
        my_gen: u64,
    ) -> Result<StreamReader, Error> {
        let info = &fs.folders[folder_index];
        let key = self
            .folder_key(fs, folder_index, folder, upstream, my_gen)
            .await?;
        if let Some(sides) = &info.bcj2
            && folder.sides.is_none()
        {
            folder.sides = Some(Arc::new(Bcj2Streams {
                call: read_side(upstream, &sides.call, key).await?,
                jump: read_side(upstream, &sides.jump, key).await?,
                rc: read_side(upstream, &sides.rc, key).await?,
            }));
        }
        if folder.index.is_none() {
            let spec = info.codec_spec(key, folder.sides.clone()).map_err(sz_err)?;
            let mut index = StreamIndex::new(spec, Some(info.packed.end - info.packed.start));
            index.unpacked_len = Some(info.unpacked_len);
            folder.index = Some(index);
        }

        if let Some(reader) = pool.take(folder.index.as_ref().unwrap(), offset, len) {
            return Ok(reader);
        }

        let interval = checkpoint_interval(info);
        let index = folder.index.as_mut().unwrap();
        if !index.complete && offset > index.indexed_to.saturating_add(interval) {
            let extended = self
                .extend_index(fs, folder_index, index.clone(), interval, offset, upstream)
                .await?;
            *index = extended;
        }
        StreamReader::new(index, offset, len).map_err(|e| Error::custom(e.to_string()))
    }

    /// Decode from the index's last checkpoint up to `target`, laying
    /// checkpoints, with progress on the mount's channel.
    async fn extend_index(
        &self,
        fs: &sz::SevenZFs,
        folder_index: usize,
        index: StreamIndex,
        interval: u64,
        target: u64,
        upstream: &mut dyn VfsRandomReader,
    ) -> Result<StreamIndex, Error> {
        struct ClearOnDrop<'a>(&'a Arc<dyn super::super::ProgressReporter>);
        impl Drop for ClearOnDrop<'_> {
            fn drop(&mut self) {
                self.0.report(None);
            }
        }
        let _clear = ClearOnDrop(&self.reporter);
        let info = &fs.folders[folder_index];
        let total = info.unpacked_len;
        let mut indexer = StreamIndexer::resume(index, FixedInterval::new(interval))
            .map_err(|e| Error::custom(e.to_string()))?;
        indexer.stop_at(target);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            self.report_folder(
                folder_index,
                fs.folders.len(),
                indexer.progress().unpacked_pos.min(total),
                total,
            );
            match indexer.step() {
                EngineRequest::NeedInput => {
                    let at = indexer.progress().compressed_pos;
                    match read_packed(upstream, &info.packed, at, PACKED_SLICE).await? {
                        Some(data) => indexer.provide_data(&data),
                        None => indexer.signal_eof(),
                    }
                }
                EngineRequest::SeekAndRead { offset, len } => {
                    match read_packed(upstream, &info.packed, offset, len as u64).await? {
                        Some(data) => indexer.provide_data(&data),
                        None => indexer.signal_eof(),
                    }
                }
                EngineRequest::OutputReady => while indexer.read_output(&mut buf) > 0 {},
                EngineRequest::Done => return Ok(indexer.finish()),
                EngineRequest::Error(e) => {
                    return Err(Error::custom(format!("7z folder: {}", e)));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Folder decoding helpers
// ---------------------------------------------------------------------------

/// Bytes between checkpoints for a folder: as many as the budget affords,
/// between 4 and 64 per folder, never denser than `MIN_INTERVAL`.
fn checkpoint_interval(info: &sz::FolderInfo) -> u64 {
    let cost = info.checkpoint_cost().max(1);
    let count = (INDEX_BUDGET / cost).clamp(4, 64);
    (info.unpacked_len / count).max(MIN_INTERVAL)
}

/// Fetch and decode one BCJ2 side stream; they are bounded
/// (`sz::MAX_BCJ2_SIDE`), so whole.
async fn read_side(
    upstream: &mut dyn VfsRandomReader,
    side: &sz::SideStream,
    key: Option<[u8; 32]>,
) -> Result<Vec<u8>, Error> {
    let mut packed = Vec::with_capacity((side.packed.end - side.packed.start) as usize);
    while let Some(data) =
        read_packed(upstream, &side.packed, packed.len() as u64, PACKED_SLICE).await?
    {
        packed.extend_from_slice(&data);
    }
    side.decode(&packed, key).map_err(sz_err)
}

/// Trial-decode the folder's start with `key`. The format has no password
/// verifier; a wrong key makes the decoder reject its input within a chunk.
/// A BCJ2 folder is tried on its main line alone, before the side streams
/// are fetched.
async fn verify_key(
    info: &sz::FolderInfo,
    key: [u8; 32],
    upstream: &mut dyn VfsRandomReader,
) -> Result<bool, Error> {
    let spec = info.verify_spec(Some(key)).map_err(sz_err)?;
    let mut index = StreamIndex::new(spec, Some(info.packed.end - info.packed.start));
    index.unpacked_len = Some(info.verify_len);
    let want = VERIFY_BYTES.min(info.verify_len);
    let reader = StreamReader::new(&index, 0, want).map_err(|e| Error::custom(e.to_string()))?;
    match drive_reader(upstream, &info.packed, reader, want).await {
        Ok((data, _)) => Ok(data.len() as u64 == want),
        Err(_) => Ok(false),
    }
}

/// Map reader errors; password conditions keep their kind so callers can
/// tell them apart.
fn sz_err(e: sz::SevenZError) -> Error {
    match e {
        sz::SevenZError::PasswordRequired | sz::SevenZError::WrongPassword => Error {
            kind: crate::ErrorKind::PermissionDenied,
            message: e.to_string(),
        },
        sz::SevenZError::Unsupported(_) => Error {
            kind: crate::ErrorKind::NotSupported,
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
    ranges: Vec<Range<u64>>,
) -> Result<Vec<sz::Chunk>, Error> {
    futures::future::try_join_all(ranges.into_iter().map(|r| async move {
        let chunk = upstream
            .read_range(archive_path, r.start, r.end - r.start)
            .await?;
        Ok(sz::Chunk {
            offset: r.start,
            data: chunk.data,
        })
    }))
    .await
}

/// Project the entry table into the shared archive `DirectoryTree`, plus a
/// name → entry-index map for reads.
fn build_tree(
    fs: &sz::SevenZFs,
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

        let is_dir = entry.kind == sz::EntryKind::Dir;
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
            is_symlink: entry.kind == sz::EntryKind::Symlink,
            symlink_target: symlink_targets.get(&entry.name).cloned(),
            user: None,
            group: None,
            mode: entry.mode.map(Mode),
            modified: entry.modified,
            accessed: entry.accessed,
            created: entry.created,
            key: None,
            source: None,
        };

        if is_dir && seen_dirs.contains(&entry_path) {
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
impl Vfs for SevenZArchiveVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &SEVENZ_ARCHIVE_VFS_DESCRIPTOR
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
        let is_dir = entry.kind == sz::EntryKind::Dir;
        Ok(FileDetails {
            size: if is_dir { 0 } else { entry.size },
            mime_type: crate::vfs::file::guess_mime_type(StdPath::new(path.as_wire_str())),
            is_dir,
            is_symlink: entry.kind == sz::EntryKind::Symlink,
            symlink_target: None,
            user: None,
            group: None,
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
        let my_gen = self.dismiss_gen.load(Ordering::Acquire);
        let state = self.ensure_state().await?;
        let entry = self.resolve_entry(state, path, true)?;
        let Some(loc) = entry.location else {
            return Ok(Box::new(std::io::Cursor::new(Vec::new())));
        };
        let info = &state.fs.folders[loc.folder];
        info.supported().map_err(sz_err)?;
        let mut upstream = open_read_at(&self.upstream, &self.archive_path).await?;
        let folder = &state.folders[loc.folder];
        let reader = {
            let mut folder_state = folder.state.lock().await;
            self.prepare_reader(
                &state.fs,
                loc.folder,
                &mut folder_state,
                &folder.pool,
                upstream.as_mut(),
                loc.offset,
                entry.size,
                my_gen,
            )
            .await?
        };
        Ok(Box::new(PipelinedReader::new(
            StreamDriver::new(
                reader,
                info.packed.clone(),
                folder.pool.clone(),
                entry.size,
                entry.crc32,
            ),
            Some(upstream),
            "7z archive",
        )))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let my_gen = self.dismiss_gen.load(Ordering::Acquire);
        let state = self.ensure_state().await?;
        let entry = self.resolve_entry(state, path, true)?;
        let total_size = entry.size;
        let Some(loc) = entry.location.filter(|_| offset < total_size && length > 0) else {
            return Ok(FileChunk {
                data: Vec::new(),
                offset,
                total_size,
            });
        };
        let want = length.min(total_size - offset);
        let info = &state.fs.folders[loc.folder];
        info.supported().map_err(sz_err)?;
        let mut upstream = open_read_at(&self.upstream, &self.archive_path).await?;
        let folder = &state.folders[loc.folder];
        let mut folder_state = folder.state.lock().await;
        let reader = self
            .prepare_reader(
                &state.fs,
                loc.folder,
                &mut folder_state,
                &folder.pool,
                upstream.as_mut(),
                loc.offset + offset,
                want,
                my_gen,
            )
            .await?;
        let (mut data, reader) =
            drive_reader(upstream.as_mut(), &info.packed, reader, want).await?;
        folder.pool.park(reader);
        data.truncate(want as usize);
        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }

    /// Entries are listed in unpack order: folder by folder, offsets
    /// ascending within each.
    async fn read_order(&self, paths: &[PathBuf]) -> Result<Option<Vec<u64>>, Error> {
        let state = self.ensure_state().await?;
        Ok(Some(
            paths
                .iter()
                .map(|p| {
                    self.resolve_entry(state, p, true)
                        .ok()
                        .and_then(|e| state.by_name.get(&e.name))
                        .map_or(u64::MAX, |&i| i as u64)
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
#[path = "sevenz_tests.rs"]
mod tests;
