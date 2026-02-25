use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::prelude::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use log::{debug, warn};
use notify::event::RemoveKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileInfo, FileReader};
use crate::filesystem::{File, FileList, Filesystem, FsStats, ListFilesOptions, Mode};
use crate::rpc::Communicator;
use crate::{Error, ToUnix};

// ---------------------------------------------------------------------------
// VfsId
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VfsId(pub u32);

impl VfsId {
    pub const ROOT: VfsId = VfsId(0);
}

impl std::fmt::Display for VfsId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// VfsPath
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VfsPath {
    pub vfs_id: VfsId,
    pub path: PathBuf,
}

impl VfsPath {
    pub fn root(path: impl Into<PathBuf>) -> Self {
        Self {
            vfs_id: VfsId::ROOT,
            path: path.into(),
        }
    }

    pub fn new(vfs_id: VfsId, path: impl Into<PathBuf>) -> Self {
        Self {
            vfs_id,
            path: path.into(),
        }
    }

    pub fn join(&self, name: impl AsRef<Path>) -> Self {
        Self {
            vfs_id: self.vfs_id,
            path: self.path.join(name),
        }
    }

    pub fn parent(&self) -> Option<Self> {
        self.path.parent().map(|p| Self {
            vfs_id: self.vfs_id,
            path: p.to_path_buf(),
        })
    }

    pub fn file_name(&self) -> Option<&std::ffi::OsStr> {
        self.path.file_name()
    }
}

impl std::fmt::Display for VfsPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.vfs_id == VfsId::ROOT {
            write!(f, "{}", self.path.display())
        } else {
            write!(f, "vfs://{}:{}", self.vfs_id, self.path.display())
        }
    }
}

// ---------------------------------------------------------------------------
// VfsDescriptor — type-level metadata for a VFS implementation
// ---------------------------------------------------------------------------

pub trait VfsDescriptor: Send + Sync + std::fmt::Debug {
    fn type_name(&self) -> &'static str;

    // --- Browse ---
    fn can_watch(&self) -> bool;

    // --- Read ---
    fn can_read_sync(&self) -> bool;
    fn can_read_async(&self) -> bool;

    // --- Write ---
    fn can_overwrite_sync(&self) -> bool;
    fn can_overwrite_async(&self) -> bool;
    fn can_create_directory(&self) -> bool;
    fn can_create_symlink(&self) -> bool;
    fn can_touch(&self) -> bool;
    fn can_truncate(&self) -> bool;
    fn can_set_metadata(&self) -> bool;

    // --- Delete ---
    fn can_remove(&self) -> bool;
    fn can_remove_tree(&self) -> bool;

    // --- Capabilities ---
    fn has_symlinks(&self) -> bool;

    // --- Same-VFS fast paths ---
    fn can_rename(&self) -> bool;
    fn can_copy_within(&self) -> bool;
    fn can_hard_link(&self) -> bool;
}

// Auto-registration via inventory
pub struct RegisteredDescriptor(pub &'static dyn VfsDescriptor);
inventory::collect!(RegisteredDescriptor);

pub fn lookup_descriptor(type_name: &str) -> Option<&'static dyn VfsDescriptor> {
    inventory::iter::<RegisteredDescriptor>()
        .find(|r| r.0.type_name() == type_name)
        .map(|r| r.0)
}

// ---------------------------------------------------------------------------
// VfsMetadata — for metadata preservation in copy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VfsMetadata {
    pub permissions: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<SystemTime>,
    pub mtime: Option<SystemTime>,
}

// ---------------------------------------------------------------------------
// VfsEntryMetadata — lightweight, for operation planning tree walks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsEntryMetadata {
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
}

// ---------------------------------------------------------------------------
// VfsSpaceInfo
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsSpaceInfo {
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// VfsAsyncWriter
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait VfsAsyncWriter: Send {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error>;
    async fn finish(self: Box<Self>) -> Result<(), Error>;
}

// ---------------------------------------------------------------------------
// Vfs trait
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait Vfs: Send + Sync {
    // --- Descriptor ---
    fn descriptor(&self) -> &'static dyn VfsDescriptor;
    fn origin(&self) -> Option<&VfsPath> {
        None
    }
    fn mount_meta(&self) -> Vec<u8> {
        Vec::new()
    }

    // --- Browse ---
    async fn list_files(
        &self,
        path: &Path,
        opts: ListFilesOptions,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<FileList, Error>;
    async fn poll_changes(&self, path: &Path) -> Result<(), Error>;

    // --- Read ---
    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let _ = (path, offset, length);
        Err(Error::NotSupported)
    }

    async fn file_info(&self, path: &Path) -> Result<FileInfo, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn read_link(&self, path: &Path) -> Result<PathBuf, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn symlink_metadata(&self, path: &Path) -> Result<VfsEntryMetadata, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    // --- Write ---
    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let _ = (link, target);
        Err(Error::NotSupported)
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    // --- Delete ---
    async fn remove(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    // --- Metadata ---
    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let _ = (path, meta);
        Err(Error::NotSupported)
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    // --- Same-VFS fast paths ---
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::NotSupported)
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::NotSupported)
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let _ = (link, target);
        Err(Error::NotSupported)
    }
}

// ---------------------------------------------------------------------------
// LocalVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LocalVfsDescriptor;

impl VfsDescriptor for LocalVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "local"
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read_sync(&self) -> bool {
        true
    }
    fn can_read_async(&self) -> bool {
        false
    }
    fn can_overwrite_sync(&self) -> bool {
        true
    }
    fn can_overwrite_async(&self) -> bool {
        false
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
        true
    }
    fn has_symlinks(&self) -> bool {
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
}

pub static LOCAL_VFS_DESCRIPTOR: LocalVfsDescriptor = LocalVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&LOCAL_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// LocalVfs — wraps existing filesystem::Local + file_reader::Local logic
// ---------------------------------------------------------------------------

pub struct LocalVfs {
    fs_cache: Arc<crate::filesystem::UidGidCache>,
}

impl LocalVfs {
    pub fn new() -> Self {
        Self {
            fs_cache: Arc::new(crate::filesystem::UidGidCache::new()),
        }
    }
}

impl Default for LocalVfs {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Vfs for LocalVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &LOCAL_VFS_DESCRIPTOR
    }

    async fn list_files(
        &self,
        path: &Path,
        opts: ListFilesOptions,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<FileList, Error> {
        assert!(path.is_absolute());
        let mut path = path.to_path_buf();
        loop {
            match tokio::task::spawn_blocking({
                let path = path.clone();
                let cache = self.fs_cache.clone();
                let batch_tx = batch_tx.as_ref().cloned();
                move || -> Result<Vec<File>, Error> {
                    const BATCH_SIZE: usize = 500;

                    let mut ret = Vec::new();
                    let mut batch = Vec::new();

                    if let Some(parent) = path.parent() {
                        let metadata = parent.symlink_metadata()?;
                        let mode = metadata.mode();
                        let file = File {
                            name: "..".to_string(),
                            size: None,
                            is_dir: true,
                            is_symlink: metadata.is_symlink(),
                            is_hidden: false,
                            user: cache.user_name(metadata.uid()).ok(),
                            group: cache.group_name(metadata.gid()).ok(),
                            mode: Mode(mode),
                            modified: metadata.modified().map(|t| t.to_unix()).ok(),
                            accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                            created: metadata.created().map(|t| t.to_unix()).ok(),
                        };
                        batch.push(file.clone());
                        ret.push(file);
                    }

                    for maybe_entry in std::fs::read_dir(&path)? {
                        let entry = maybe_entry?;
                        let metadata = entry.metadata()?;
                        let file_type = metadata.file_type();

                        let name = entry.file_name().into_string().unwrap();
                        let mut is_dir = file_type.is_dir();

                        if file_type.is_symlink() {
                            let target_metadata = std::fs::metadata(entry.path());
                            if let Ok(target_metadata) = target_metadata {
                                is_dir = target_metadata.is_dir();
                            }
                        }

                        let mode = metadata.mode();
                        let file = File {
                            name: name.clone(),
                            size: (!is_dir).then_some(metadata.len()),
                            is_dir,
                            is_symlink: file_type.is_symlink(),
                            is_hidden: name.starts_with('.'),
                            user: cache.user_name(metadata.uid()).ok(),
                            group: cache.group_name(metadata.gid()).ok(),
                            mode: Mode(mode),
                            modified: metadata.modified().map(|t| t.to_unix()).ok(),
                            accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                            created: metadata.created().map(|t| t.to_unix()).ok(),
                        };
                        batch.push(file.clone());
                        ret.push(file);

                        if batch.len() >= BATCH_SIZE {
                            if let Some(ref tx) = batch_tx {
                                if tx.send(std::mem::take(&mut batch)).is_err() {
                                    // Receiver dropped — cancelled
                                    return Ok(ret);
                                }
                            } else {
                                batch.clear();
                            }
                        }
                    }

                    // Send any remaining entries as a final batch
                    if let Some(ref tx) = batch_tx {
                        if !batch.is_empty() {
                            let _ = tx.send(batch);
                        }
                    }

                    Ok(ret)
                }
            })
            .await?
            {
                Ok(files) => {
                    let stats = nix::sys::statvfs::statvfs(&path).ok().map(FsStats::from);
                    return Ok(FileList::new(VfsPath::root(&path), files, stats));
                }
                Err(Error::Io(e)) => match (e.kind(), opts.strict) {
                    (std::io::ErrorKind::NotFound, false)
                    | (std::io::ErrorKind::NotADirectory, _) => {
                        if !path.pop() {
                            return Err(e.into());
                        }
                    }
                    _ => return Err(e.into()),
                },
                Err(e) => return Err(e),
            }
        }
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Arc::new(Mutex::new(Some(tx)));

        let mut watcher = {
            let path = path.clone();
            RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    match res {
                        Ok(event) => {
                            debug!("{:?} (while watching {})", event, path.display());
                            let should_notify = match event.kind {
                                EventKind::Remove(RemoveKind::Folder) => event
                                    .paths
                                    .iter()
                                    .any(|p| path.starts_with(p) || p.starts_with(&path)),
                                _ => event.paths.iter().any(|p| p.starts_with(&path)),
                            };

                            if should_notify {
                                if let Some(s) = tx.lock().take() {
                                    let _ = s.send(());
                                }
                            }
                        }
                        Err(e) => warn!("watch error: {:?}", e),
                    };
                },
                Config::default(),
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

    async fn file_info(&self, path: &Path) -> Result<FileInfo, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let file = std::fs::File::open(&path)?;
            let metadata = file.metadata()?;
            let size = metadata.len();
            let mut buf = vec![0u8; 8192.min(size as usize)];
            let mut reader = std::io::BufReader::new(file);
            let n = reader.read(&mut buf)?;
            let is_binary = buf[..n].contains(&0);
            Ok(FileInfo { size, is_binary })
        })
        .await?
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&path)?;
            let metadata = file.metadata()?;
            let total_size = metadata.len();
            file.seek(SeekFrom::Start(offset))?;
            let to_read = length.min(total_size.saturating_sub(offset)) as usize;
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

    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let path = path.to_path_buf();
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::open(&path).map_err(Error::Io))
                .await??;
        Ok(Box::new(file))
    }

    async fn read_link(&self, path: &Path) -> Result<PathBuf, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::read_link(&path).map_err(Error::Io)).await?
    }

    async fn symlink_metadata(&self, path: &Path) -> Result<VfsEntryMetadata, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            Ok(VfsEntryMetadata {
                is_file: meta.is_file(),
                is_dir: meta.is_dir(),
                is_symlink: meta.is_symlink(),
                size: meta.len(),
            })
        })
        .await?
    }

    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
        let path = path.to_path_buf();
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::create(&path).map_err(Error::Io))
                .await??;
        Ok(Box::new(file))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&path).map_err(Error::Io))
            .await?
    }

    async fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let link = link.to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &link)?;
            #[cfg(not(unix))]
            return Err(Error::NotSupported);
            Ok(())
        })
        .await?
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn remove(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            if path.is_dir() {
                std::fs::remove_dir(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            Ok(())
        })
        .await?
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            Ok(())
        })
        .await?
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::symlink_metadata(&path)?;
            Ok(VfsMetadata {
                permissions: Some(meta.mode()),
                uid: Some(meta.uid()),
                gid: Some(meta.gid()),
                atime: meta.accessed().ok(),
                mtime: meta.modified().ok(),
            })
        })
        .await?
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let path = path.to_path_buf();
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
            Ok(())
        })
        .await?
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::rename(&from, &to).map_err(Error::Io)).await?
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
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
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let stats = nix::sys::statvfs::statvfs(&path)?;
            let frag = stats.fragment_size() as u64;
            Ok(VfsSpaceInfo {
                total_bytes: Some(stats.blocks() as u64 * frag),
                used_bytes: Some(
                    (stats.blocks() as u64).saturating_sub(stats.blocks_free() as u64) * frag,
                ),
                available_bytes: Some(stats.blocks_available() as u64 * frag),
            })
        })
        .await?
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        tokio::task::spawn_blocking(move || {
            // Try FICLONE (instant COW clone) first on Linux
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::io::AsRawFd;
                let src = std::fs::File::open(&from)?;
                let dst = std::fs::File::create(&to)?;
                let ret =
                    unsafe { libc::ioctl(dst.as_raw_fd(), 0x40049409u64 as _, src.as_raw_fd()) };
                if ret == 0 {
                    return Ok(());
                }
                // FICLONE failed (unsupported FS), clean up and fall through to fs::copy
                drop(dst);
                let _ = std::fs::remove_file(&to);
            }

            // Fall back to kernel-level copy
            std::fs::copy(&from, &to)?;
            Ok(())
        })
        .await?
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let link = link.to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::hard_link(&target, &link).map_err(Error::Io))
            .await?
    }
}

// ---------------------------------------------------------------------------
// VfsRegistry
// ---------------------------------------------------------------------------

pub struct VfsRegistry {
    vfs_map: RwLock<HashMap<VfsId, Arc<dyn Vfs>>>,
    next_id: AtomicU32,
}

impl VfsRegistry {
    pub fn with_root(root: Arc<dyn Vfs>) -> Self {
        let mut map = HashMap::new();
        map.insert(VfsId::ROOT, root);
        Self {
            vfs_map: RwLock::new(map),
            next_id: AtomicU32::new(1),
        }
    }

    pub fn get(&self, id: VfsId) -> Option<Arc<dyn Vfs>> {
        self.vfs_map.read().get(&id).cloned()
    }

    pub fn resolve(&self, vfs_path: &VfsPath) -> Result<(Arc<dyn Vfs>, PathBuf), Error> {
        let vfs = self
            .get(vfs_path.vfs_id)
            .ok_or_else(|| Error::Custom(format!("VFS {} not found", vfs_path.vfs_id)))?;
        Ok((vfs, vfs_path.path.clone()))
    }

    pub fn mount(&self, vfs: Arc<dyn Vfs>) -> VfsId {
        let id = VfsId(self.next_id.fetch_add(1, Ordering::SeqCst));
        self.vfs_map.write().insert(id, vfs);
        id
    }

    pub fn unmount(&self, id: VfsId) -> Option<Arc<dyn Vfs>> {
        if id == VfsId::ROOT {
            return None; // refuse to unmount ROOT
        }
        self.vfs_map.write().remove(&id)
    }
}

// ---------------------------------------------------------------------------
// VfsRegistryFs — implements Filesystem by dispatching through VfsRegistry
// ---------------------------------------------------------------------------

pub struct VfsRegistryFs {
    registry: Arc<VfsRegistry>,
}

impl VfsRegistryFs {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl Filesystem for VfsRegistryFs {
    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.poll_changes(&local_path).await
    }

    async fn list_files(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<FileList, Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.list_files(&local_path, options, batch_tx).await
    }

    async fn rename(&self, old_path: VfsPath, new_path: VfsPath) -> Result<(), Error> {
        if old_path.vfs_id != new_path.vfs_id {
            return Err(Error::Custom("cannot rename across VFS boundaries".into()));
        }
        let (vfs, old_local) = self.registry.resolve(&old_path)?;
        vfs.rename(&old_local, &new_path.path).await
    }

    async fn touch(&self, path: VfsPath) -> Result<(), Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.touch(&local_path).await
    }

    async fn create_directory(&self, path: VfsPath) -> Result<(), Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.create_directory(&local_path).await
    }

    async fn delete_all(&self, paths: Vec<VfsPath>) -> Result<(), Error> {
        for path in paths {
            let (vfs, local_path) = self.registry.resolve(&path)?;
            vfs.remove_tree(&local_path).await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// VfsRegistryFileReader — implements FileReader by dispatching through VfsRegistry
// ---------------------------------------------------------------------------

pub struct VfsRegistryFileReader {
    registry: Arc<VfsRegistry>,
}

impl VfsRegistryFileReader {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl FileReader for VfsRegistryFileReader {
    async fn file_info(&self, path: VfsPath) -> Result<FileInfo, Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.file_info(&local_path).await
    }

    async fn read_range(
        &self,
        path: VfsPath,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.read_range(&local_path, offset, length).await
    }
}

// ---------------------------------------------------------------------------
// Mount/unmount RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MountRequest {
    S3 { region: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResponse {
    pub vfs_id: VfsId,
    pub type_name: String,
    pub mount_meta: Vec<u8>,
}

// ---------------------------------------------------------------------------
// MountedVfsInfo — client-side descriptor + metadata for a mounted VFS
// ---------------------------------------------------------------------------

pub struct MountedVfsInfo {
    pub vfs_id: VfsId,
    pub descriptor: &'static dyn VfsDescriptor,
    pub mount_meta: Vec<u8>,
}

// ---------------------------------------------------------------------------
// VfsManager — trait for mount/unmount operations
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait VfsManager: Send + Sync {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error>;
    async fn unmount(&self, vfs_id: VfsId) -> Result<(), Error>;
}

pub struct VfsManagerRemote {
    communicator: Communicator,
}

impl VfsManagerRemote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl VfsManager for VfsManagerRemote {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error> {
        let ret: Result<MountResponse, Error> = self
            .communicator
            .invoke(crate::api::API_MOUNT_VFS, &request)
            .await?;
        Ok(ret?)
    }

    async fn unmount(&self, vfs_id: VfsId) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_UNMOUNT_VFS, &vfs_id)
            .await?;
        Ok(ret?)
    }
}
