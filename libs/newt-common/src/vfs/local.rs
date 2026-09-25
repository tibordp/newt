#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::prelude::MetadataExt;
use std::path::Path as StdPath;
use std::sync::Arc;

use log::{debug, warn};
use notify::event::RemoveKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
#[cfg(unix)]
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{File, FileChunk, FileDetails, FsStats, Mode, ToUnix, UserGroup};

use super::path_style::{
    local_breadcrumbs, local_display_path, metadata_traits_from_meta, navigable_parent,
    roots_from_meta, unified_root_from_meta,
};
use super::{
    Breadcrumb, DisplayPathMatch, MetadataTraits, PathStyle, RegisteredDescriptor, RootInfo, Vfs,
    VfsAsyncWriter, VfsDescriptor, VfsMetadata, VfsRandomReader, VfsSpaceInfo,
};

/// Bytes read from a file head when sniffing for a MIME type without an
/// extension match. Bigger reads catch more formats but cost more I/O per
/// directory listing entry; 8 KiB is enough for every magic-number signature
/// in `mimetype-detector` while staying inside one filesystem block.
const MIME_SNIFF_BUFFER_SIZE: usize = 8192;

/// Files-per-batch streamed to the host during a directory listing. Smaller
/// batches reduce first-paint latency on huge directories; larger batches
/// reduce IPC overhead. 500 lands in the sweet spot for both.
const LIST_FILES_BATCH_SIZE: usize = 500;

// ---------------------------------------------------------------------------
// LocalVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LocalVfsDescriptor;

impl VfsDescriptor for LocalVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "local"
    }
    fn display_name(&self) -> &'static str {
        "Local"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read(&self) -> bool {
        true
    }
    fn can_overwrite(&self) -> bool {
        true
    }
    fn can_create_directory(&self) -> bool {
        true
    }
    fn can_create_symlink(&self) -> bool {
        true
    }
    fn can_touch(&self) -> bool {
        true
    }
    fn can_truncate(&self) -> bool {
        true
    }
    fn can_write_range(&self) -> bool {
        true
    }
    fn can_set_metadata(&self) -> bool {
        true
    }
    fn can_remove(&self) -> bool {
        true
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn can_trash(&self) -> bool {
        true
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_stat_directories(&self) -> bool {
        true
    }
    fn can_fs_stats(&self) -> bool {
        true
    }
    fn can_rename(&self) -> bool {
        true
    }
    fn can_copy_within(&self) -> bool {
        true
    }
    fn can_hard_link(&self) -> bool {
        true
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        local_display_path(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        local_breadcrumbs(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn navigable_parent(&self, path: &Path, mount_meta: &[u8]) -> Option<PathBuf> {
        navigable_parent(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        None
    }

    fn roots(&self, mount_meta: &[u8]) -> Vec<RootInfo> {
        roots_from_meta(mount_meta)
    }
    fn has_unified_root(&self, mount_meta: &[u8]) -> bool {
        // Style-based, not root-count: a single-drive Windows host is
        // still split-root. `initial_path` (trait default) then lands on
        // the first drive instead of the unlistable `/`.
        unified_root_from_meta(mount_meta)
    }
    fn metadata_traits(&self, mount_meta: &[u8]) -> MetadataTraits {
        metadata_traits_from_meta(mount_meta)
    }
}

/// Root paths of the local filesystem, enumerated on the side that owns
/// it (the host for a local session, the agent for a remote one; also
/// the host for its FS exposed into a remote session). Unix has the
/// single `/`; Windows has one per logical drive (`\\?\C:`, …), each
/// classified via [`volume::probe_native`]. Baked into `mount_meta` at
/// mount time so root lookups stay descriptor-only (no per-call RPC);
/// drive changes re-enumerate via `VfsManager::remount`, driven by the
/// host's device-change/focus triggers.
#[cfg(unix)]
pub fn local_roots() -> Vec<RootInfo> {
    vec![RootInfo::root()]
}

#[cfg(windows)]
pub fn local_roots() -> Vec<RootInfo> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDriveStringsW;

    // First call with a zero length returns the required buffer size.
    let needed = unsafe { GetLogicalDriveStringsW(0, std::ptr::null_mut()) };
    if needed == 0 {
        return vec![RootInfo::root()];
    }
    let mut buf = vec![0u16; needed as usize];
    let written = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if written == 0 {
        return vec![RootInfo::root()];
    }
    // Buffer is a sequence of NUL-terminated `X:\` strings, double-NUL
    // terminated. Decode each into the `["?","X:"]` sentinel form.
    let roots: Vec<RootInfo> = buf[..written as usize]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let native = String::from_utf16_lossy(s);
            let native = StdPath::new(&native);
            RootInfo {
                path: PathBuf::from_native(native),
                volume: super::volume::probe_native(native),
            }
        })
        .collect();
    if roots.is_empty() {
        vec![RootInfo::root()]
    } else {
        roots
    }
}

pub static LOCAL_VFS_DESCRIPTOR: LocalVfsDescriptor = LocalVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&LOCAL_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// LocalVfs
// ---------------------------------------------------------------------------

/// Memoized uid/gid → name resolution over the local user/group database,
/// producing the mode/owner/group fields of a listing entry. Windows has no
/// POSIX owner concept, so there the type is an empty shim and
/// [`UidGidCache::owner_bits`] yields `None`s.
#[derive(Default)]
struct UidGidCache {
    #[cfg(unix)]
    users: RwLock<HashMap<u32, UserGroup>>,
    #[cfg(unix)]
    groups: RwLock<HashMap<u32, UserGroup>>,
}

impl UidGidCache {
    #[cfg(unix)]
    fn owner_bits(
        &self,
        meta: &std::fs::Metadata,
    ) -> (Option<Mode>, Option<UserGroup>, Option<UserGroup>) {
        (
            Some(Mode(meta.mode())),
            self.user_name(meta.uid()).ok(),
            self.group_name(meta.gid()).ok(),
        )
    }

    #[cfg(windows)]
    fn owner_bits(
        &self,
        _meta: &std::fs::Metadata,
    ) -> (Option<Mode>, Option<UserGroup>, Option<UserGroup>) {
        (None, None, None)
    }

    #[cfg(unix)]
    fn user_name(&self, uid: u32) -> Result<UserGroup, Error> {
        if let Some(user) = self.users.read().get(&uid) {
            return Ok(user.clone());
        }
        let user = match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))? {
            Some(u) => UserGroup::Name(u.name),
            None => UserGroup::Id(uid),
        };
        self.users.write().insert(uid, user.clone());
        Ok(user)
    }

    #[cfg(unix)]
    fn group_name(&self, gid: u32) -> Result<UserGroup, Error> {
        if let Some(group) = self.groups.read().get(&gid) {
            return Ok(group.clone());
        }
        let group = match nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))? {
            Some(g) => UserGroup::Name(g.name),
            None => UserGroup::Id(gid),
        };
        self.groups.write().insert(gid, group.clone());
        Ok(group)
    }
}

pub struct LocalVfs {
    fs_cache: Arc<UidGidCache>,
}

impl LocalVfs {
    pub fn new() -> Self {
        Self {
            fs_cache: Arc::new(UidGidCache::default()),
        }
    }
}

impl Default for LocalVfs {
    fn default() -> Self {
        Self::new()
    }
}

/// `st_*` metadata carried on `File` for local entries: allocated
/// bytes (non-directories only), device id, inode, and hardlink count.
/// All `None` on platforms without them (Windows).
fn stat_extras(
    metadata: &std::fs::Metadata,
    is_dir: bool,
) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (
            (!is_dir).then(|| metadata.blocks() * 512),
            Some(metadata.dev()),
            Some(metadata.ino()),
            Some(metadata.nlink()),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, is_dir);
        (None, None, None, None)
    }
}

/// Salvage a failed `remove_file`. Reached only once the plain delete has
/// already failed, so the ordinary path pays nothing for it.
///
/// On Windows a directory symlink or junction *is* a directory to the
/// Win32 API: `DeleteFileW` refuses it and only `RemoveDirectoryW` will
/// do, which removes the link and never what it points at. Anything else
/// keeps the original error.
///
/// Unix needs no such split — `unlink` takes any symlink — so the error
/// stands there.
#[cfg(windows)]
fn remove_file_fallback(path: &StdPath, err: std::io::Error) -> Result<(), Error> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Err(err.into());
    };
    if meta.file_type().is_symlink() && meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return std::fs::remove_dir(path).map_err(Error::from);
    }
    Err(err.into())
}

#[cfg(not(windows))]
fn remove_file_fallback(_path: &StdPath, err: std::io::Error) -> Result<(), Error> {
    Err(err.into())
}

/// Opaque filesystem identity: `(volume, file)`, equal exactly when two
/// paths name the same file. `st_dev`/`st_ino` on Unix, volume serial +
/// 128-bit file id on Windows — the pair `cp` itself uses to refuse a
/// self-copy.
type FileIdentity = (u64, u128);

/// Identity of `path`, or `None` if it doesn't exist. Symlinks are
/// identified as themselves rather than as their target, matching
/// `file_info`'s `symlink_metadata`.
#[cfg(unix)]
pub(super) fn file_identity(path: &StdPath) -> Result<Option<FileIdentity>, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), u128::from(meta.ino())))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(windows)]
pub(super) fn file_identity(path: &StdPath) -> Result<Option<FileIdentity>, Error> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    let file = match std::fs::OpenOptions::new()
        // Attributes only, sharing everything: identifying a file must not
        // fail because something else has it open.
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        // BACKUP_SEMANTICS to open directories at all; OPEN_REPARSE_POINT
        // so a symlink identifies as itself, as on Unix.
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;

    let mut id_info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: live handle, and the buffer matches the requested class.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&raw mut id_info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok != 0 {
        return Ok(Some((
            id_info.VolumeSerialNumber,
            u128::from_le_bytes(id_info.FileId.Identifier),
        )));
    }

    // Network redirectors and older drivers reject FileIdInfo; the legacy
    // call's 64-bit index is enough there.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: live handle, caller-owned out param.
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(Some((
        u64::from(info.dwVolumeSerialNumber),
        u128::from(index),
    )))
}

#[async_trait::async_trait]
impl Vfs for LocalVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &LOCAL_VFS_DESCRIPTOR
    }

    /// A `LocalVfs` always serves the filesystem of whatever process it
    /// runs in — the host for a local session / client-local hairpin, the
    /// agent for a remote session. So it stamps this binary's host style
    /// and the roots enumerated here, carried to whichever side renders.
    fn mount_meta(&self) -> Vec<u8> {
        super::encode_mount_meta(PathStyle::host(), &local_roots())
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::VfsFileList, Error> {
        let path = path.to_native();
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let files: Vec<File> = tokio::task::spawn_blocking({
            let cache = self.fs_cache.clone();
            move || -> Result<Vec<File>, Error> {
                const BATCH_SIZE: usize = LIST_FILES_BATCH_SIZE;

                let mut ret = Vec::new();
                let mut batch = Vec::new();

                if cancel.is_cancelled() {
                    return Ok(ret);
                }

                if let Some(parent) = path.parent() {
                    // Always emit `..` so up-navigation works even if the
                    // parent can't be stat'd (degrade to null metadata).
                    let file = match parent.symlink_metadata() {
                        Ok(metadata) => {
                            let (mode_field, user_field, group_field) = cache.owner_bits(&metadata);
                            File {
                                name: "..".to_string(),
                                size: None,
                                allocated_size: None,
                                device_id: None,
                                inode: None,
                                hard_links: None,
                                is_dir: true,
                                is_symlink: metadata.is_symlink(),
                                symlink_target: None,
                                is_hidden: false,
                                user: user_field,
                                group: group_field,
                                mode: mode_field,
                                attributes: file_attributes(&metadata),
                                modified: metadata.modified().map(|t| t.to_unix()).ok(),
                                accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                                created: metadata.created().map(|t| t.to_unix()).ok(),
                                key: None,
                                source: None,
                            }
                        }
                        Err(_) => File::parent_dir(),
                    };
                    batch.push(file.clone());
                    ret.push(file);
                }

                for maybe_entry in std::fs::read_dir(&path)? {
                    if cancel.is_cancelled() {
                        return Ok(ret);
                    }
                    // A dirent we can't even read — skip it rather than
                    // aborting the whole listing.
                    let Ok(entry) = maybe_entry else {
                        continue;
                    };

                    // Best-effort UTF-8 conversion: a non-UTF-8 filename gets
                    // U+FFFD replacement chars. The entry shows up in the UI
                    // but file ops on it (rename / delete / touch / etc.) will
                    // fail with NotFound — when the frontend echoes the name
                    // back, `path.join(&name)` builds a path with the
                    // replacements that doesn't exist on disk. Acceptable
                    // trade-off vs. panicking the entire listing.
                    let name = entry.file_name().to_string_lossy().into_owned();

                    let file = match entry.metadata() {
                        Ok(metadata) => {
                            let file_type = metadata.file_type();
                            let mut is_dir = file_type.is_dir();

                            let symlink_target = if file_type.is_symlink() {
                                let target_metadata = std::fs::metadata(entry.path());
                                if let Ok(target_metadata) = target_metadata {
                                    is_dir = target_metadata.is_dir();
                                }
                                std::fs::read_link(entry.path())
                                    .ok()
                                    .map(|t| t.to_string_lossy().into_owned())
                            } else {
                                None
                            };

                            let (mode_field, user_field, group_field) = cache.owner_bits(&metadata);
                            let (allocated_size, device_id, inode, hard_links) =
                                stat_extras(&metadata, is_dir);
                            File {
                                name: name.clone(),
                                size: (!is_dir).then_some(metadata.len()),
                                allocated_size,
                                device_id,
                                inode,
                                hard_links,
                                is_dir,
                                is_symlink: file_type.is_symlink(),
                                symlink_target,
                                is_hidden: is_hidden(&name, &metadata),
                                user: user_field,
                                group: group_field,
                                mode: mode_field,
                                attributes: file_attributes(&metadata),
                                modified: metadata.modified().map(|t| t.to_unix()).ok(),
                                accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                                created: metadata.created().map(|t| t.to_unix()).ok(),
                                key: None,
                                source: None,
                            }
                        }
                        // `stat()` was denied. Real-world trigger: Windows
                        // system files (pagefile.sys, hiberfil.sys,
                        // swapfile.sys, DumpStack.log.tmp) seen through WSL's
                        // `/mnt` DrvFs return `-?????????` from `ls`. Degrade
                        // to the bare dirent (d_type, if any) with null
                        // metadata instead of failing the whole directory.
                        Err(_) => {
                            let file_type = entry.file_type().ok();
                            File {
                                name: name.clone(),
                                size: None,
                                allocated_size: None,
                                device_id: None,
                                inode: None,
                                hard_links: None,
                                is_dir: file_type.map(|t| t.is_dir()).unwrap_or(false),
                                is_symlink: file_type.map(|t| t.is_symlink()).unwrap_or(false),
                                symlink_target: None,
                                is_hidden: name.starts_with('.'),
                                user: None,
                                group: None,
                                mode: None,
                                attributes: None,
                                modified: None,
                                accessed: None,
                                created: None,
                                key: None,
                                source: None,
                            }
                        }
                    };

                    batch.push(file.clone());
                    ret.push(file);

                    if batch.len() >= BATCH_SIZE {
                        if let Some(ref tx) = batch_tx {
                            if tx.blocking_send(std::mem::take(&mut batch)).is_err() {
                                // Receiver dropped — cancelled
                                return Ok(ret);
                            }
                        } else {
                            batch.clear();
                        }
                    }
                }

                // Send any remaining entries as a final batch
                if let Some(ref tx) = batch_tx
                    && !batch.is_empty()
                {
                    let _ = tx.blocking_send(batch);
                }

                Ok(ret)
            }
        })
        .await??;
        Ok(files.into())
    }

    async fn fs_stats(&self, path: &Path) -> Result<Option<FsStats>, Error> {
        let path = path.to_native();
        Ok(tokio::task::spawn_blocking(move || platform_fs_stats(&path)).await?)
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_native();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Arc::new(Mutex::new(Some(tx)));

        let mut watcher = {
            let path = path.clone();
            RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    match res {
                        Ok(event) => {
                            let should_notify = match event.kind {
                                EventKind::Remove(RemoveKind::Folder) => event
                                    .paths
                                    .iter()
                                    .any(|p| path.starts_with(p) || p.starts_with(&path)),
                                EventKind::Access(_) => false,
                                _ => event.paths.iter().any(|p| p.starts_with(&path)),
                            };

                            if should_notify && let Some(s) = tx.lock().take() {
                                debug!("{:?} (while watching {})", event, path.display());
                                let _ = s.send(());
                            }
                        }
                        Err(e) => warn!("watch error: {:?}", e),
                    };
                },
                Config::default().with_follow_symlinks(false),
            )?
        };

        let mut watch_path = path;
        loop {
            watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;
            if !watch_path.pop() {
                break;
            }
        }

        let _ = rx.await;
        Ok(())
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let path = path.to_native();
        let cache = self.fs_cache.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;

            let symlink_meta = std::fs::symlink_metadata(&path)?;
            let is_symlink = symlink_meta.is_symlink();
            let symlink_target = if is_symlink {
                std::fs::read_link(&path)
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned())
            } else {
                None
            };

            let meta = if is_symlink {
                std::fs::metadata(&path).unwrap_or(symlink_meta)
            } else {
                symlink_meta
            };

            let is_dir = meta.is_dir();
            let size = meta.len();
            let (mode_field, user_field, group_field) = cache.owner_bits(&meta);

            // Try extension first, then content sniffing.
            let mime_type = if is_dir {
                None
            } else {
                let from_extension = crate::vfs::file::guess_mime_type(&path);
                if from_extension.is_some() {
                    from_extension
                } else {
                    let file = std::fs::File::open(&path)?;
                    let mut buf = vec![0u8; MIME_SNIFF_BUFFER_SIZE.min(size as usize)];
                    let mut reader = std::io::BufReader::new(file);
                    let n = reader.read(&mut buf)?;
                    let header = &buf[..n];

                    let detected = mimetype_detector::detect(header);
                    if detected.is("application/octet-stream") {
                        // No specific match — fall back to null-byte heuristic
                        if !header.contains(&0) {
                            Some("text/plain".to_string())
                        } else {
                            Some("application/octet-stream".to_string())
                        }
                    } else {
                        Some(detected.mime().to_string())
                    }
                }
            };

            Ok(FileDetails {
                size,
                mime_type,
                is_dir,
                is_symlink,
                symlink_target,
                user: user_field,
                group: group_field,
                mode: mode_field,
                modified: meta.modified().map(|t| t.to_unix()).ok(),
                accessed: meta.accessed().map(|t| t.to_unix()).ok(),
                created: meta.created().map(|t| t.to_unix()).ok(),
            })
        })
        .await?
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&path)?;
            let total_size = file.metadata()?.len();
            file.seek(SeekFrom::Start(offset))?;
            // Don't cap at total_size — pseudo-files (procfs, sysfs) and
            // block devices report size 0 but have readable content.
            let to_read = length as usize;
            let mut data = vec![0u8; to_read];
            let mut total_read = 0;
            while total_read < to_read {
                let n = file.read(&mut data[total_read..])?;
                if n == 0 {
                    break;
                }
                total_read += n;
            }
            data.truncate(total_read);
            Ok(FileChunk {
                data,
                offset,
                total_size,
            })
        })
        .await?
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>, Error> {
        let file = tokio::fs::File::open(path.to_native()).await?;
        Ok(Box::new(file))
    }

    async fn open_read_at(&self, path: &Path) -> Result<Box<dyn VfsRandomReader>, Error> {
        let path = path.to_native();
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::open(&path).map_err(Error::from))
                .await??;
        Ok(Box::new(LocalRandomReader {
            file: Arc::new(file),
        }))
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let path = path.to_native();
        let cache = self.fs_cache.clone();
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            let is_symlink = meta.is_symlink();
            let symlink_target = if is_symlink {
                std::fs::read_link(&path)
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned())
            } else {
                None
            };
            let mut is_dir = meta.is_dir();
            if is_symlink && let Ok(target_meta) = std::fs::metadata(&path) {
                is_dir = target_meta.is_dir();
            }
            let (mode_field, user_field, group_field) = cache.owner_bits(&meta);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let (allocated_size, device_id, inode, hard_links) = stat_extras(&meta, is_dir);
            Ok(File {
                is_hidden: is_hidden(&name, &meta),
                name,
                size: (!is_dir).then_some(meta.len()),
                allocated_size,
                device_id,
                inode,
                hard_links,
                is_dir,
                is_symlink,
                symlink_target,
                user: user_field,
                group: group_field,
                mode: mode_field,
                attributes: file_attributes(&meta),
                modified: meta.modified().map(|t| t.to_unix()).ok(),
                accessed: meta.accessed().map(|t| t.to_unix()).ok(),
                created: meta.created().map(|t| t.to_unix()).ok(),
                key: None,
                source: None,
            })
        })
        .await?
    }

    async fn resolve_link(&self, path: &Path) -> Result<PathBuf, Error> {
        Ok(PathBuf::from_native(
            &tokio::fs::canonicalize(path.to_native()).await?,
        ))
    }

    async fn read_attribute(
        &self,
        path: &Path,
        kind: super::attributes::AttributeKind,
    ) -> Result<Option<super::attributes::Attribute>, Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || super::local_attributes::read(&path, kind)).await?
    }

    async fn write_attribute(
        &self,
        path: &Path,
        property: &super::attributes::Attribute,
    ) -> Result<(), Error> {
        let path = path.to_native();
        let property = property.clone();
        tokio::task::spawn_blocking(move || super::local_attributes::write(&path, &property))
            .await?
    }

    async fn stream_path(&self, path: &Path, name: &str) -> Result<PathBuf, Error> {
        super::local_attributes::stream_path(path, name)
    }

    async fn overwrite_async(
        &self,
        path: &Path,
        options: &super::attributes::WriteOptions,
    ) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        if options.object_metadata.is_some()
            || options.object_storage_class.is_some()
            || options.object_canned_acl.is_some()
        {
            return Err(Error::not_supported());
        }
        let native = path.to_native();
        let file = if options.create_new {
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&native)
                .await
            {
                Ok(file) => file,
                Err(e) => return Err(create_new_refusal(&native, e).await),
            }
        } else {
            tokio::fs::File::create(&native).await?
        };
        #[cfg(windows)]
        if options.sparse {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE};
            let mut returned = 0;
            if unsafe {
                DeviceIoControl(
                    file.as_raw_handle() as _,
                    FSCTL_SET_SPARSE,
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(Box::new(LocalAsyncWriter {
            file,
            sparse: options.sparse,
            length: 0,
            holes: Vec::new(),
            exclusive: Exclusive(options.create_new.then_some(native)),
        }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&path).map_err(Error::from))
            .await?
    }

    async fn create_symlink(&self, link: &Path, target: &str) -> Result<(), Error> {
        let link = link.to_native();
        let target = target.to_string();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &link)?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = (link, target);
                Err(Error::not_supported())
            }
        })
        .await?
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) => remove_file_fallback(&path, e),
        })
        .await?
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || {
            std::fs::remove_dir(&path)?;
            Ok(())
        })
        .await?
    }

    async fn trash_item(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || {
            trash::delete(&path).map_err(|e| Error::custom(format!("trash: {e}")))
        })
        .await?
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            let (mut permissions, uid, gid) = unix_meta_ids(&meta);
            if meta.is_symlink() {
                permissions = None;
            }
            Ok(VfsMetadata {
                permissions,
                uid,
                gid,
                atime: meta.accessed().ok(),
                mtime: meta.modified().ok(),
            })
        })
        .await?
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let path = path.to_native();
        let meta = meta.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                if let Some(permissions) = meta.permissions {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
                }

                let uid = meta.uid.map(nix::unistd::Uid::from_raw);
                let gid = meta.gid.map(nix::unistd::Gid::from_raw);
                if uid.is_some() || gid.is_some() {
                    use std::os::unix::ffi::OsStrExt;
                    let native = std::ffi::CString::new(path.as_os_str().as_bytes())
                        .map_err(|e| Error::custom(e.to_string()))?;
                    if unsafe {
                        libc::lchown(
                            native.as_ptr(),
                            uid.map_or(!0, |id| id.as_raw()),
                            gid.map_or(!0, |id| id.as_raw()),
                        )
                    } != 0
                    {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }

                if meta.atime.is_some() || meta.mtime.is_some() {
                    let current_meta = std::fs::symlink_metadata(&path)?;
                    let atime = meta.atime.map_or_else(
                        || filetime::FileTime::from_last_access_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    let mtime = meta.mtime.map_or_else(
                        || filetime::FileTime::from_last_modification_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    filetime::set_symlink_file_times(&path, atime, mtime)?;
                }
            }
            #[cfg(windows)]
            {
                // Local Windows builds don't surface POSIX mode/uid/gid bits
                // (`get_metadata` returns them as `None`), so we only honor
                // atime/mtime — everything else is a no-op.
                if meta.atime.is_some() || meta.mtime.is_some() {
                    let current_meta = std::fs::symlink_metadata(&path)?;
                    let atime = meta.atime.map_or_else(
                        || filetime::FileTime::from_last_access_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    let mtime = meta.mtime.map_or_else(
                        || filetime::FileTime::from_last_modification_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    filetime::set_file_times(&path, atime, mtime)?;
                }
            }
            Ok(())
        })
        .await?
    }

    async fn same_file(&self, a: &Path, b: &Path) -> Result<bool, Error> {
        let a = a.to_native();
        let b = b.to_native();
        tokio::task::spawn_blocking(move || {
            // Two absent paths are not "the same file" — hence the match
            // rather than comparing the Options.
            Ok(matches!(
                (file_identity(&a)?, file_identity(&b)?),
                (Some(x), Some(y)) if x == y
            ))
        })
        .await?
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_native();
        let to = to.to_native();
        tokio::task::spawn_blocking(move || std::fs::rename(&from, &to).map_err(Error::from))
            .await?
    }

    async fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_native();
        let to = to.to_native();
        tokio::task::spawn_blocking(move || rename_no_replace(&from, &to)).await?
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn write_range(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), Error> {
        let path = path.to_native();
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new().write(true).open(&path)?;
            let len = file.metadata()?.len();
            if offset.saturating_add(data.len() as u64) > len {
                return Err(Error::custom("write_range past the end of the file"));
            }
            let mut done = 0;
            while done < data.len() {
                let n = pwrite(&file, &data[done..], offset + done as u64)?;
                if n == 0 {
                    return Err(Error::custom("write_range wrote nothing"));
                }
                done += n;
            }
            Ok(())
        })
        .await?
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let path = path.to_native();
        tokio::task::spawn_blocking(move || platform_space_info(&path)).await?
    }

    async fn copy_within(
        &self,
        from: &Path,
        to: &Path,
        options: &super::attributes::WriteOptions,
    ) -> Result<(), Error> {
        // Kernel copies don't take write options; the streaming fallback
        // honours them.
        let create_new = options.create_new;
        let options = super::attributes::WriteOptions {
            create_new: false,
            ..options.clone()
        };
        if !options.is_default() {
            return Err(Error::not_supported());
        }
        let from = from.to_native();
        let to = to.to_native();
        tokio::task::spawn_blocking(move || {
            if !create_new {
                return kernel_copy(&from, &to);
            }
            // A reservation would make the clone fail, so a clone goes in
            // under its own exclusive steps.
            #[cfg(target_os = "macos")]
            match clone_no_replace(&from, &to) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind == crate::ErrorKind::AlreadyExists => return Err(e),
                Err(e) => debug!("copy_within: clonefile unavailable: {}", e),
            }
            let reserved = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&to)
                .map_err(|e| create_new_refusal_blocking(&to, e))?;
            let copied = copy_into_reserved(&from, &to, reserved);
            // A failed exclusive copy leaves nothing: the file is the
            // reservation.
            if copied.is_err() {
                let _ = std::fs::remove_file(&to);
            }
            copied
        })
        .await?
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let link = link.to_native();
        let target = target.to_native();
        tokio::task::spawn_blocking(move || std::fs::hard_link(&target, &link).map_err(Error::from))
            .await?
    }
}

// ---------------------------------------------------------------------------
// Exclusive creation
// ---------------------------------------------------------------------------

/// A `create_new` open's error, with a refusal as `AlreadyExists`. Windows
/// answers a directory in the way with access denied: the open checks
/// "is a directory" before the name collision.
async fn create_new_refusal(path: &StdPath, e: std::io::Error) -> Error {
    #[cfg(windows)]
    if e.kind() == std::io::ErrorKind::PermissionDenied
        && tokio::fs::symlink_metadata(path).await.is_ok()
    {
        return Error {
            kind: crate::ErrorKind::AlreadyExists,
            message: e.to_string(),
        };
    }
    let _ = path;
    e.into()
}

/// [`create_new_refusal`] for blocking callers.
fn create_new_refusal_blocking(path: &StdPath, e: std::io::Error) -> Error {
    #[cfg(windows)]
    if e.kind() == std::io::ErrorKind::PermissionDenied && std::fs::symlink_metadata(path).is_ok() {
        return Error {
            kind: crate::ErrorKind::AlreadyExists,
            message: e.to_string(),
        };
    }
    let _ = path;
    e.into()
}

/// Copy `from` over `to` in the kernel where the platform can.
fn kernel_copy(from: &StdPath, to: &StdPath) -> Result<(), Error> {
    // Try FICLONE (instant COW clone) first on Linux.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let src = std::fs::File::open(from)?;
        let dst = std::fs::File::create(to)?;
        let ret = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
        if ret == 0 {
            return Ok(());
        }
        // FICLONE failed (unsupported FS); fs::copy truncates what is
        // there.
    }

    // Fall back to the platform's kernel-assisted path
    // (copy_file_range/sendfile, fcopyfile, or CopyFileEx). This also
    // preserves sparse-file behavior where the platform supports it.
    std::fs::copy(from, to)?;
    Ok(())
}

/// Copy `from` into `dst`, the file just created at `to` by an exclusive
/// open, through that descriptor: reopening `to` by name would follow
/// whatever someone swapped in since.
#[cfg(target_os = "macos")]
fn copy_into_reserved(from: &StdPath, _to: &StdPath, dst: std::fs::File) -> Result<(), Error> {
    use std::os::unix::io::AsRawFd;
    let src = std::fs::File::open(from)?;
    // What std's copy does once it has opened the destination.
    let flags = libc::COPYFILE_METADATA | libc::COPYFILE_DATA;
    if unsafe {
        libc::fcopyfile(
            src.as_raw_fd(),
            dst.as_raw_fd(),
            std::ptr::null_mut(),
            flags,
        )
    } != 0
    {
        return Err(nix::Error::last().into());
    }
    Ok(())
}

/// Copy `from` into `dst`, the file just created at `to` by an exclusive
/// open, through that descriptor: reopening `to` by name would follow
/// whatever someone swapped in since.
#[cfg(all(unix, not(target_os = "macos")))]
fn copy_into_reserved(from: &StdPath, _to: &StdPath, mut dst: std::fs::File) -> Result<(), Error> {
    let mut src = std::fs::File::open(from)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
        if ret == 0 {
            return Ok(());
        }
    }
    // copy_file_range/sendfile between the two descriptors, and the
    // source's mode, as std's copy gives.
    std::io::copy(&mut src, &mut dst)?;
    dst.set_permissions(src.metadata()?.permissions())?;
    Ok(())
}

/// CopyFileExW takes names only, so the copy goes over the reservation by
/// name; a symlink swapped in meanwhile needs the privilege to create one.
#[cfg(windows)]
fn copy_into_reserved(from: &StdPath, to: &StdPath, dst: std::fs::File) -> Result<(), Error> {
    drop(dst);
    std::fs::copy(from, to)?;
    Ok(())
}

/// Clone `from` to `to` unless something is at `to`. clonefile follows a
/// dangling symlink at its destination and creates the target, so the
/// clone lands under an unguessable name beside `to` (retried on a
/// collision) and is renamed into place with RENAME_EXCL, which refuses
/// anything there. A clone that does not go in is removed.
#[cfg(target_os = "macos")]
fn clone_no_replace(from: &StdPath, to: &StdPath) -> Result<(), Error> {
    let dir = to
        .parent()
        .ok_or_else(|| Error::custom("no directory to clone into"))?;
    let staged = tempfile::Builder::new()
        .prefix(".newt-clone-")
        .make_in(dir, |path| clonefile(from, path))?
        .into_temp_path();
    rename_no_replace(&staged, to)?;
    let _ = staged.keep();
    Ok(())
}

#[cfg(target_os = "macos")]
fn clonefile(from: &StdPath, to: &StdPath) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let nul = |_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains a NUL byte");
    let from = std::ffi::CString::new(from.as_os_str().as_bytes()).map_err(nul)?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes()).map_err(nul)?;
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_no_replace(from: &StdPath, to: &StdPath) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| Error::custom("path contains a NUL byte"))?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| Error::custom("path contains a NUL byte"))?;
    // The raw syscall: the musl the agents link has no renameat2 wrapper.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if ret == 0 {
        return Ok(());
    }
    match nix::Error::last() {
        // A filesystem without RENAME_NOREPLACE, or a kernel before 3.15.
        nix::Error::EINVAL | nix::Error::ENOSYS => Err(Error::not_supported()),
        e => Err(e.into()),
    }
}

#[cfg(target_os = "macos")]
fn rename_no_replace(from: &StdPath, to: &StdPath) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| Error::custom("path contains a NUL byte"))?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| Error::custom("path contains a NUL byte"))?;
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(nix::Error::last().into())
    }
}

#[cfg(windows)]
fn rename_no_replace(from: &StdPath, to: &StdPath) -> Result<(), Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &StdPath| {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (from_w, to_w) = (wide(from), wide(to));
    // Without MOVEFILE_REPLACE_EXISTING an existing target is refused.
    if unsafe { MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), 0) } == 0 {
        return Err(create_new_refusal_blocking(
            to,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rename_no_replace(_from: &StdPath, _to: &StdPath) -> Result<(), Error> {
    Err(Error::not_supported())
}

// ---------------------------------------------------------------------------
// LocalAsyncWriter
// ---------------------------------------------------------------------------

struct LocalAsyncWriter {
    file: tokio::fs::File,
    sparse: bool,
    length: u64,
    holes: Vec<(u64, u64)>,
    /// After `file`, so the file is closed first where the drop can see
    /// it.
    exclusive: Exclusive,
}

/// A file this writer created exclusively, until the write finishes.
/// Dropped before that (a failed, abandoned or cancelled write) the file
/// is removed: nothing else can have been there, and a failed exclusive
/// write leaving nothing lets a retry stay exclusive.
struct Exclusive(Option<std::path::PathBuf>);

impl Drop for Exclusive {
    fn drop(&mut self) {
        let Some(path) = self.0.take() else {
            return;
        };
        let remove = move || {
            let _ = std::fs::remove_file(path);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => drop(handle.spawn_blocking(remove)),
            Err(_) => remove(),
        }
    }
}

#[async_trait::async_trait]
impl VfsAsyncWriter for LocalAsyncWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        use tokio::io::AsyncWriteExt;
        if self.sparse && buf.iter().all(|byte| *byte == 0) {
            match self.holes.last_mut() {
                Some((offset, len)) if *offset + *len == self.length => *len += buf.len() as u64,
                _ => self.holes.push((self.length, buf.len() as u64)),
            }
            use tokio::io::AsyncSeekExt;
            self.file
                .seek(std::io::SeekFrom::Current(buf.len() as i64))
                .await?;
        } else {
            self.file.write_all(buf).await?;
        }
        self.length += buf.len() as u64;
        Ok(buf.len())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), Error> {
        use tokio::io::AsyncWriteExt;
        self.file.flush().await?;
        if self.sparse {
            self.file.set_len(self.length).await?;
            #[cfg(unix)]
            {
                let file = self.file.into_std().await;
                let holes = self.holes;
                tokio::task::spawn_blocking(move || {
                    super::local_attributes::punch_holes(&file, &holes)
                })
                .await??;
            }
        }
        self.exclusive.0 = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LocalRandomReader — pread on a held-open fd
// ---------------------------------------------------------------------------

struct LocalRandomReader {
    file: Arc<std::fs::File>,
}

#[async_trait::async_trait]
impl VfsRandomReader for LocalRandomReader {
    async fn read_at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Error> {
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || {
            let mut data = vec![0u8; len as usize];
            let mut total = 0usize;
            while total < data.len() {
                let n = pread(&file, &mut data[total..], offset + total as u64)?;
                if n == 0 {
                    break;
                }
                total += n;
            }
            data.truncate(total);
            Ok(data)
        })
        .await?
    }
}

#[cfg(unix)]
fn pread(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn pread(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(unix)]
fn pwrite(file: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, buf, offset)
}

#[cfg(windows)]
fn pwrite(file: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(file, buf, offset)
}

// ---------------------------------------------------------------------------
// Platform-specific helpers
// ---------------------------------------------------------------------------

/// Whether a directory entry should be treated as hidden.
///
/// * Unix: the leading-dot convention.
/// * Windows: the filesystem `HIDDEN`/`SYSTEM` attributes (the dot
///   convention is meaningless there; Explorer / Salamander hide both).
#[cfg(unix)]
fn is_hidden(name: &str, _meta: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Raw `FILE_ATTRIBUTE_*` bits for the Attr column (Windows only).
#[cfg(windows)]
fn file_attributes(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::windows::fs::MetadataExt;
    Some(meta.file_attributes())
}

#[cfg(unix)]
fn file_attributes(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(windows)]
fn is_hidden(_name: &str, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM};
    meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(unix)]
fn unix_meta_ids(meta: &std::fs::Metadata) -> (Option<u32>, Option<u32>, Option<u32>) {
    (Some(meta.mode()), Some(meta.uid()), Some(meta.gid()))
}

#[cfg(windows)]
fn unix_meta_ids(_meta: &std::fs::Metadata) -> (Option<u32>, Option<u32>, Option<u32>) {
    (None, None, None)
}

#[cfg(unix)]
fn platform_fs_stats(path: &StdPath) -> Option<FsStats> {
    nix::sys::statvfs::statvfs(path)
        .ok()
        .map(|s| FsStats::from(s).with_volume(super::volume::probe_native(path)))
}

#[cfg(windows)]
fn platform_fs_stats(path: &StdPath) -> Option<FsStats> {
    win_disk_space(path).map(|(total, free, available)| {
        FsStats::new(
            /* free_bytes */ free, /* available_bytes */ available,
            /* total_bytes */ total,
        )
        .with_volume(super::volume::probe_native(path))
    })
}

#[cfg(unix)]
fn platform_space_info(path: &StdPath) -> Result<VfsSpaceInfo, Error> {
    let stats = nix::sys::statvfs::statvfs(path)?;
    let frag = stats.fragment_size() as u64;
    Ok(VfsSpaceInfo {
        total_bytes: Some(stats.blocks() as u64 * frag),
        used_bytes: Some((stats.blocks() as u64).saturating_sub(stats.blocks_free() as u64) * frag),
        available_bytes: Some(stats.blocks_available() as u64 * frag),
    })
}

#[cfg(windows)]
fn platform_space_info(path: &StdPath) -> Result<VfsSpaceInfo, Error> {
    match win_disk_space(path) {
        Some((total, free, available)) => Ok(VfsSpaceInfo {
            total_bytes: Some(total),
            used_bytes: Some(total.saturating_sub(free)),
            available_bytes: Some(available),
        }),
        None => Ok(VfsSpaceInfo {
            total_bytes: None,
            used_bytes: None,
            available_bytes: None,
        }),
    }
}

/// Returns `(total_bytes, free_bytes, available_to_caller_bytes)` for the
/// volume containing `path`. Returns `None` if the Win32 call fails (path
/// doesn't exist, network share unavailable, etc.).
#[cfg(windows)]
fn win_disk_space(path: &StdPath) -> Option<(u64, u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    // GetDiskFreeSpaceExW accepts any path on the volume; widen + NUL-terminate.
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut free_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    // SAFETY: All three out-params are valid u64 pointers; `wide` is NUL-terminated.
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_caller, &mut total, &mut total_free)
    };
    if ok == 0 {
        return None;
    }
    Some((total, total_free, free_caller))
}

// ---------------------------------------------------------------------------
// LocalVfs::same_file — real filesystem
// ---------------------------------------------------------------------------

#[cfg(test)]
mod write_range_tests {
    use std::sync::Arc;

    use crate::ErrorKind;
    use crate::vfs::Vfs;
    use crate::vfs::local::LocalVfs;
    use crate::vfs::path::PathBuf;

    #[tokio::test]
    async fn overwrites_in_place_and_refuses_to_extend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let native = dir.path().join("f.bin");
        std::fs::write(&native, b"0123456789").expect("write");
        let vfs = Arc::new(LocalVfs::new());
        let path = PathBuf::from_native(&native);

        vfs.write_range(&path, 3, b"abc").await.unwrap();
        assert_eq!(std::fs::read(&native).unwrap(), b"012abc6789");
        vfs.write_range(&path, 8, b"xy").await.unwrap();
        assert_eq!(std::fs::read(&native).unwrap(), b"012abc67xy");

        assert!(vfs.write_range(&path, 9, b"zz").await.is_err());
        assert_eq!(std::fs::read(&native).unwrap(), b"012abc67xy");
        assert_eq!(
            vfs.write_range(&PathBuf::from_native(&dir.path().join("nope")), 0, b"z")
                .await
                .unwrap_err()
                .kind,
            ErrorKind::NotFound
        );
    }
}

#[cfg(test)]
mod same_file_tests {
    use std::sync::Arc;

    use crate::vfs::Vfs;
    use crate::vfs::local::LocalVfs;
    use crate::vfs::path::PathBuf;

    struct Fixture {
        _dir: tempfile::TempDir,
        vfs: Arc<LocalVfs>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                _dir: tempfile::tempdir().expect("tempdir"),
                vfs: Arc::new(LocalVfs::new()),
            }
        }

        /// VFS path of `name` inside the fixture directory.
        fn path(&self, name: &str) -> PathBuf {
            PathBuf::from_native(&self._dir.path().join(name))
        }

        fn write(&self, name: &str) -> PathBuf {
            std::fs::write(self._dir.path().join(name), b"x").expect("write");
            self.path(name)
        }

        /// Whether the volume under test folds case — a stock macOS or
        /// Windows volume does, ext4 doesn't, and APFS can be formatted
        /// either way. Decided by asking the filesystem, so the tests
        /// below assert what's true *here* rather than what's true on the
        /// author's laptop.
        fn volume_folds_case(&self) -> bool {
            std::fs::write(self._dir.path().join("CaseProbe"), b"x").expect("write");
            let folds = self._dir.path().join("caseprobe").exists();
            std::fs::remove_file(self._dir.path().join("CaseProbe")).expect("remove");
            folds
        }
    }

    #[tokio::test]
    async fn a_file_is_itself() {
        let fx = Fixture::new();
        let a = fx.write("a.txt");
        assert!(fx.vfs.same_file(&a, &a).await.unwrap());
    }

    #[tokio::test]
    async fn distinct_files_are_not_the_same() {
        let fx = Fixture::new();
        let (a, b) = (fx.write("a.txt"), fx.write("b.txt"));
        assert!(!fx.vfs.same_file(&a, &b).await.unwrap());
    }

    #[tokio::test]
    async fn an_absent_path_is_nothing_s_twin() {
        let fx = Fixture::new();
        let a = fx.write("a.txt");
        let missing = fx.path("missing.txt");
        let also_missing = fx.path("also-missing.txt");

        assert!(!fx.vfs.same_file(&a, &missing).await.unwrap());
        assert!(!fx.vfs.same_file(&missing, &a).await.unwrap());
        // Two absent paths must not compare equal just because both
        // resolve to "no identity".
        assert!(!fx.vfs.same_file(&missing, &also_missing).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hardlinks_are_the_same_file() {
        let fx = Fixture::new();
        let a = fx.write("a.txt");
        std::fs::hard_link(
            fx._dir.path().join("a.txt"),
            fx._dir.path().join("link.txt"),
        )
        .expect("hard_link");
        let link = fx.path("link.txt");

        // Same inode, different name — what `cp` refuses to copy onto itself.
        assert!(fx.vfs.same_file(&a, &link).await.unwrap());
    }

    #[tokio::test]
    async fn case_variants_follow_the_volume() {
        let fx = Fixture::new();
        let folds = fx.volume_folds_case();
        let upper = fx.write("Foo.txt");
        let lower = fx.path("foo.txt");

        assert_eq!(fx.vfs.same_file(&upper, &lower).await.unwrap(), folds);
    }

    /// The whole point of the exercise: on a case-insensitive volume the
    /// rename must actually go through rather than trip over its own
    /// destination.
    #[tokio::test]
    async fn case_only_rename_succeeds_on_a_folding_volume() {
        let fx = Fixture::new();
        if !fx.volume_folds_case() {
            return;
        }
        let upper = fx.write("Foo.txt");
        let lower = fx.path("foo.txt");

        fx.vfs.rename(&upper, &lower).await.expect("rename");

        let names: Vec<String> = std::fs::read_dir(fx._dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["foo.txt".to_string()]);
    }
}

#[cfg(test)]
mod exclusive_write_tests {
    use crate::ErrorKind;
    use crate::vfs::Vfs;
    use crate::vfs::attributes::WriteOptions;
    use crate::vfs::local::LocalVfs;
    use crate::vfs::path::PathBuf;

    fn create_new() -> WriteOptions {
        WriteOptions {
            create_new: true,
            ..Default::default()
        }
    }

    /// Things a `create_new` must not write over, by name.
    fn obstacles(dir: &std::path::Path) -> Vec<&'static str> {
        std::fs::write(dir.join("file"), b"old").unwrap();
        std::fs::create_dir(dir.join("dir")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("nowhere", dir.join("dangling")).unwrap();
            vec!["file", "dir", "dangling"]
        }
        #[cfg(not(unix))]
        vec!["file", "dir"]
    }

    /// The names `obstacles` makes, with `more`, sorted.
    fn obstacles_and(more: &[&str]) -> Vec<String> {
        let mut names = vec!["dir", "file"];
        #[cfg(unix)]
        names.push("dangling");
        names.extend(more);
        let mut names = names.into_iter().map(String::from).collect::<Vec<_>>();
        names.sort();
        names
    }

    #[tokio::test]
    async fn create_new_refuses_whatever_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = LocalVfs::new();
        for name in obstacles(dir.path()) {
            let path = PathBuf::from_native(&dir.path().join(name));
            let refused = vfs.overwrite_async(&path, &create_new()).await.err();
            assert_eq!(
                refused.map(|e| e.kind),
                Some(ErrorKind::AlreadyExists),
                "{name}"
            );
        }
        assert_eq!(std::fs::read(dir.path().join("file")).unwrap(), b"old");
        assert!(!dir.path().join("nowhere").exists());
    }

    /// Dropped off the runtime, the writer removes its file inline rather
    /// than on the blocking pool, so the result is there to assert.
    fn drop_off_runtime(writer: Box<dyn crate::vfs::VfsAsyncWriter>) {
        std::thread::spawn(move || drop(writer)).join().unwrap();
    }

    #[tokio::test]
    async fn an_unfinished_create_new_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = LocalVfs::new();
        let native = dir.path().join("new");
        let path = PathBuf::from_native(&native);

        let writer = vfs.overwrite_async(&path, &create_new()).await.unwrap();
        assert!(native.exists());
        drop_off_runtime(writer);
        assert!(!native.exists());

        let mut writer = vfs.overwrite_async(&path, &create_new()).await.unwrap();
        writer.write(b"partial").await.unwrap();
        drop_off_runtime(writer);
        assert!(!native.exists());
    }

    #[tokio::test]
    async fn an_unfinished_plain_write_leaves_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = LocalVfs::new();
        let native = dir.path().join("old");
        std::fs::write(&native, b"old").unwrap();

        let mut writer = vfs
            .overwrite_async(&PathBuf::from_native(&native), &WriteOptions::default())
            .await
            .unwrap();
        writer.write(b"partial").await.unwrap();
        drop_off_runtime(writer);
        assert!(native.exists());
    }

    #[tokio::test]
    async fn create_new_finishes_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = LocalVfs::new();
        let native = dir.path().join("empty");
        let writer = vfs
            .overwrite_async(&PathBuf::from_native(&native), &create_new())
            .await
            .unwrap();
        writer.finish().await.unwrap();
        assert_eq!(std::fs::read(&native).unwrap(), b"");
    }

    #[tokio::test]
    async fn copy_within_under_create_new_refuses_whatever_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = LocalVfs::new();
        std::fs::write(dir.path().join("src"), b"new").unwrap();
        let src = PathBuf::from_native(&dir.path().join("src"));
        for name in obstacles(dir.path()) {
            let path = PathBuf::from_native(&dir.path().join(name));
            let refused = vfs.copy_within(&src, &path, &create_new()).await.err();
            assert_eq!(
                refused.map(|e| e.kind),
                Some(ErrorKind::AlreadyExists),
                "{name}"
            );
        }
        assert_eq!(std::fs::read(dir.path().join("file")).unwrap(), b"old");
        assert!(!dir.path().join("nowhere").exists());
        let mut names = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, obstacles_and(&["src"]));

        let fresh = dir.path().join("fresh");
        vfs.copy_within(&src, &PathBuf::from_native(&fresh), &create_new())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn a_reserved_copy_carries_the_data_and_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (src, to) = (dir.path().join("src"), dir.path().join("to"));
        std::fs::write(&src, b"payload").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o640)).unwrap();
        let reserved = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)
            .unwrap();

        super::copy_into_reserved(&src, &to, reserved).unwrap();

        assert_eq!(std::fs::read(&to).unwrap(), b"payload");
        let mode = std::fs::metadata(&to).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn a_reserved_copy_is_not_redirected_by_a_link_swapped_in() {
        let dir = tempfile::tempdir().unwrap();
        let (src, to) = (dir.path().join("src"), dir.path().join("to"));
        std::fs::write(&src, b"payload").unwrap();
        let reserved = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)
            .unwrap();
        std::fs::remove_file(&to).unwrap();
        std::os::unix::fs::symlink("elsewhere", &to).unwrap();

        super::copy_into_reserved(&src, &to, reserved).unwrap();

        assert!(!dir.path().join("elsewhere").exists());
    }

    #[tokio::test]
    async fn rename_no_replace_refuses_whatever_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = LocalVfs::new();
        std::fs::write(dir.path().join("src"), b"new").unwrap();
        let src = PathBuf::from_native(&dir.path().join("src"));
        for name in obstacles(dir.path()) {
            let path = PathBuf::from_native(&dir.path().join(name));
            match vfs.rename_no_replace(&src, &path).await {
                // A filesystem without RENAME_NOREPLACE: nothing to test here.
                Err(e) if e.kind == ErrorKind::NotSupported => return,
                result => assert_eq!(
                    result.err().map(|e| e.kind),
                    Some(ErrorKind::AlreadyExists),
                    "{name}"
                ),
            }
        }
        assert_eq!(std::fs::read(dir.path().join("src")).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.path().join("file")).unwrap(), b"old");

        let fresh = dir.path().join("fresh");
        vfs.rename_no_replace(&src, &PathBuf::from_native(&fresh))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"new");
        assert!(!dir.path().join("src").exists());
    }
}
