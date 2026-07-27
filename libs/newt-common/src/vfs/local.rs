use std::path::Path as StdPath;
use std::sync::Arc;

use crate::vfs::path::{Path, PathBuf};

use log::{debug, warn};
use notify::event::RemoveKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;

#[cfg(unix)]
use std::os::unix::prelude::MetadataExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::vfs::ToUnix;
use crate::vfs::{File, FsStats, Mode, UserGroup};
use crate::vfs::{FileChunk, FileDetails};

#[cfg(windows)]
use super::native::local_path_from_native;
use super::native::to_native;
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
                path: local_path_from_native(native),
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

/// Memoized uid/gid → name resolution over the local user/group database.
struct UidGidCache {
    local_users: parking_lot::RwLock<std::collections::HashMap<u32, UserGroup>>,
    local_groups: parking_lot::RwLock<std::collections::HashMap<u32, UserGroup>>,
}

impl Default for UidGidCache {
    fn default() -> Self {
        Self {
            local_users: parking_lot::RwLock::new(std::collections::HashMap::new()),
            local_groups: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }
}

impl UidGidCache {
    fn new() -> Self {
        Self::default()
    }

    fn group_name(&self, gid: u32) -> Result<UserGroup, Error> {
        {
            let groups = self.local_groups.read();
            if let Some(group) = groups.get(&gid) {
                return Ok(group.clone());
            }
        }

        let group = lookup_group(gid)?;

        let mut groups = self.local_groups.write();
        groups.insert(gid, group.clone());

        Ok(group)
    }

    fn user_name(&self, uid: u32) -> Result<UserGroup, Error> {
        {
            let users = self.local_users.read();
            if let Some(user) = users.get(&uid) {
                return Ok(user.clone());
            }
        }

        let user = lookup_user(uid)?;

        let mut users = self.local_users.write();
        users.insert(uid, user.clone());

        Ok(user)
    }
}

#[cfg(unix)]
fn lookup_group(gid: u32) -> Result<UserGroup, Error> {
    let group = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))?;
    Ok(match group {
        Some(g) => UserGroup::Name(g.name),
        None => UserGroup::Id(gid),
    })
}

#[cfg(unix)]
fn lookup_user(uid: u32) -> Result<UserGroup, Error> {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))?;
    Ok(match user {
        Some(u) => UserGroup::Name(u.name),
        None => UserGroup::Id(uid),
    })
}

#[cfg(windows)]
fn lookup_group(gid: u32) -> Result<UserGroup, Error> {
    // Windows has no POSIX gid space; the local FS never produces real gids
    // (`VfsMetadata.gid` is None), so this should be unreachable in practice.
    Ok(UserGroup::Id(gid))
}

#[cfg(windows)]
fn lookup_user(uid: u32) -> Result<UserGroup, Error> {
    Ok(UserGroup::Id(uid))
}

pub struct LocalVfs {
    fs_cache: Arc<UidGidCache>,
}

impl LocalVfs {
    pub fn new() -> Self {
        Self {
            fs_cache: Arc::new(UidGidCache::new()),
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
fn file_identity(path: &StdPath) -> Result<Option<FileIdentity>, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), u128::from(meta.ino())))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(windows)]
fn file_identity(path: &StdPath) -> Result<Option<FileIdentity>, Error> {
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
        let path = to_native(path);
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
                            let (mode_field, user_field, group_field) =
                                unix_owner_bits(&metadata, &cache);
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
                        Err(_) => File {
                            name: "..".to_string(),
                            size: None,
                            allocated_size: None,
                            device_id: None,
                            inode: None,
                            hard_links: None,
                            is_dir: true,
                            is_symlink: false,
                            symlink_target: None,
                            is_hidden: false,
                            user: None,
                            group: None,
                            mode: None,
                            attributes: None,
                            modified: None,
                            accessed: None,
                            created: None,
                            key: None,
                            source: None,
                        },
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

                            let (mode_field, user_field, group_field) =
                                unix_owner_bits(&metadata, &cache);
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
        let path = to_native(path);
        Ok(tokio::task::spawn_blocking(move || platform_fs_stats(&path)).await?)
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
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
        let path = to_native(path);
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
            let (mode_field, user_field, group_field) = unix_owner_bits(&meta, &cache);

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
        let path = to_native(path);
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
        let file = tokio::fs::File::open(to_native(path)).await?;
        Ok(Box::new(file))
    }

    async fn open_read_at(&self, path: &Path) -> Result<Box<dyn VfsRandomReader>, Error> {
        let path = to_native(path);
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::open(&path).map_err(Error::from))
                .await??;
        Ok(Box::new(LocalRandomReader {
            file: Arc::new(file),
        }))
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let path = to_native(path);
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
            let (mode_field, user_field, group_field) = unix_owner_bits(&meta, &cache);
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

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let file = tokio::fs::File::create(to_native(path)).await?;
        Ok(Box::new(LocalAsyncWriter { file }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&path).map_err(Error::from))
            .await?
    }

    async fn create_symlink(&self, link: &Path, target: &str) -> Result<(), Error> {
        let link = to_native(link);
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
        let path = to_native(path);
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
        let path = to_native(path);
        tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) => remove_file_fallback(&path, e),
        })
        .await?
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            std::fs::remove_dir(&path)?;
            Ok(())
        })
        .await?
    }

    async fn trash_item(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            trash::delete(&path).map_err(|e| Error::custom(format!("trash: {e}")))
        })
        .await?
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            let (permissions, uid, gid) = unix_meta_ids(&meta);
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
        let path = to_native(path);
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
                    nix::unistd::chown(&path, uid, gid)?;
                }

                if meta.atime.is_some() || meta.mtime.is_some() {
                    let current_meta = std::fs::metadata(&path)?;
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
            #[cfg(windows)]
            {
                // Local Windows builds don't surface POSIX mode/uid/gid bits
                // (`get_metadata` returns them as `None`), so we only honor
                // atime/mtime — everything else is a no-op.
                if meta.atime.is_some() || meta.mtime.is_some() {
                    let current_meta = std::fs::metadata(&path)?;
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
        let a = to_native(a);
        let b = to_native(b);
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
        let from = to_native(from);
        let to = to_native(to);
        tokio::task::spawn_blocking(move || std::fs::rename(&from, &to).map_err(Error::from))
            .await?
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || platform_space_info(&path)).await?
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = to_native(from);
        let to = to_native(to);
        tokio::task::spawn_blocking(move || {
            // Try FICLONE (instant COW clone) first on Linux.
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::io::AsRawFd;
                let src = std::fs::File::open(&from)?;
                let dst = std::fs::File::create(&to)?;
                let ret = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
                if ret == 0 {
                    return Ok(());
                }
                // FICLONE failed (unsupported FS), clean up and fall through to fs::copy
                drop(dst);
                let _ = std::fs::remove_file(&to);
            }

            // Fall back to the platform's kernel-assisted path
            // (copy_file_range/sendfile, fcopyfile, or CopyFileEx). This also
            // preserves sparse-file behavior where the platform supports it.
            std::fs::copy(&from, &to)?;
            Ok(())
        })
        .await?
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let link = to_native(link);
        let target = to_native(target);
        tokio::task::spawn_blocking(move || std::fs::hard_link(&target, &link).map_err(Error::from))
            .await?
    }
}

// ---------------------------------------------------------------------------
// LocalAsyncWriter
// ---------------------------------------------------------------------------

struct LocalAsyncWriter {
    file: tokio::fs::File,
}

#[async_trait::async_trait]
impl VfsAsyncWriter for LocalAsyncWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        use tokio::io::AsyncWriteExt;
        self.file.write_all(buf).await?;
        Ok(buf.len())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), Error> {
        use tokio::io::AsyncWriteExt;
        self.file.flush().await?;
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

// ---------------------------------------------------------------------------
// Platform-specific helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn unix_owner_bits(
    meta: &std::fs::Metadata,
    cache: &Arc<UidGidCache>,
) -> (Option<Mode>, Option<UserGroup>, Option<UserGroup>) {
    (
        Some(Mode(meta.mode())),
        cache.user_name(meta.uid()).ok(),
        cache.group_name(meta.gid()).ok(),
    )
}

#[cfg(windows)]
fn unix_owner_bits(
    _meta: &std::fs::Metadata,
    _cache: &Arc<UidGidCache>,
) -> (Option<Mode>, Option<UserGroup>, Option<UserGroup>) {
    (None, None, None)
}

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
