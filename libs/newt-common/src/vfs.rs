use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use bitflags::bitflags;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::file_reader::{FileChunk, FileInfo, FileReader};
use crate::filesystem::{FileList, Filesystem, ListFilesOptions};
use crate::Error;

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
// VfsCapabilities
// ---------------------------------------------------------------------------

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct VfsCapabilities: u32 {
        const READ         = 0b0000_0001;
        const WRITE        = 0b0000_0010;
        const DELETE       = 0b0000_0100;
        const RENAME       = 0b0000_1000;
        const WATCH        = 0b0001_0000;
        const SHELL_EXPAND = 0b0010_0000;
        const FAST_COPY    = 0b0100_0000;
    }
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
// VfsDirEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsDirEntry {
    pub name: OsString,
    pub metadata: VfsEntryMetadata,
}

// ---------------------------------------------------------------------------
// Vfs trait
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait Vfs: Send + Sync {
    // --- Required (browsing) ---
    fn name(&self) -> &str;
    fn capabilities(&self) -> VfsCapabilities;
    async fn list_files(&self, path: &Path, opts: ListFilesOptions) -> Result<FileList, Error>;
    async fn poll_changes(&self, path: &Path) -> Result<(), Error>;

    // --- Reading (gated by READ) ---
    async fn file_info(&self, path: &Path) -> Result<FileInfo, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let _ = (path, offset, length);
        Err(Error::NotSupported)
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
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

    async fn read_dir(&self, path: &Path) -> Result<Vec<VfsDirEntry>, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    // --- Writing (gated by WRITE) ---
    async fn open_write(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
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

    // --- Deletion (gated by DELETE) ---
    async fn remove(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    // --- Metadata mutation ---
    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let _ = (path, meta);
        Err(Error::NotSupported)
    }

    // --- Fast-path same-VFS operations ---
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::NotSupported)
    }

    async fn copy_file(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::NotSupported)
    }

    async fn clone_file(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::NotSupported)
    }

    // --- Shell ---
    async fn shell_expand(&self, input: String) -> Result<PathBuf, Error> {
        let _ = input;
        Err(Error::NotSupported)
    }

    // --- VFS origin (for sub-VFS like archives) ---
    fn origin(&self) -> Option<&VfsPath> {
        None
    }
}

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
    fn name(&self) -> &str {
        "local"
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities::READ
            | VfsCapabilities::WRITE
            | VfsCapabilities::DELETE
            | VfsCapabilities::RENAME
            | VfsCapabilities::WATCH
            | VfsCapabilities::SHELL_EXPAND
            | VfsCapabilities::FAST_COPY
    }

    async fn list_files(&self, path: &Path, opts: ListFilesOptions) -> Result<FileList, Error> {
        // Delegate to existing filesystem::Local implementation
        let local = crate::filesystem::Local::with_cache(self.fs_cache.clone());
        local.list_files_impl(path, opts).await
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let local = crate::filesystem::Local::with_cache(self.fs_cache.clone());
        local.poll_changes_impl(path).await
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

    async fn open_read(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
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

    async fn read_dir(&self, path: &Path) -> Result<Vec<VfsDirEntry>, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                let meta = std::fs::symlink_metadata(&entry.path())?;
                entries.push(VfsDirEntry {
                    name: entry.file_name(),
                    metadata: VfsEntryMetadata {
                        is_file: meta.is_file(),
                        is_dir: meta.is_dir(),
                        is_symlink: meta.is_symlink(),
                        size: meta.len(),
                    },
                });
            }
            Ok(entries)
        })
        .await?
    }

    async fn open_write(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
        let path = path.to_path_buf();
        let file = tokio::task::spawn_blocking(move || {
            std::fs::File::create(&path).map_err(Error::Io)
        })
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
        tokio::task::spawn_blocking(move || std::fs::rename(&from, &to).map_err(Error::Io))
            .await?
    }

    async fn copy_file(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::copy(&from, &to)?;
            Ok(())
        })
        .await?
    }

    async fn clone_file(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        tokio::task::spawn_blocking(move || {
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::io::AsRawFd;
                let src = std::fs::File::open(&from)?;
                let dst = std::fs::File::create(&to)?;
                // FICLONE ioctl
                let ret =
                    unsafe { libc::ioctl(dst.as_raw_fd(), 0x40049409u64 as _, src.as_raw_fd()) };
                if ret < 0 {
                    // Clean up the created file on failure
                    drop(dst);
                    let _ = std::fs::remove_file(&to);
                    return Err(Error::Io(std::io::Error::last_os_error()));
                }
                Ok(())
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (&from, &to);
                Err(Error::NotSupported)
            }
        })
        .await?
    }

    async fn shell_expand(&self, input: String) -> Result<PathBuf, Error> {
        tokio::task::spawn_blocking(move || {
            expanduser::expanduser(input).map_err(Error::Io)
        })
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
    async fn shell_expand(&self, path: String) -> Result<VfsPath, Error> {
        // Shell expand only makes sense on the root (local) VFS
        let (vfs, _) = self.registry.resolve(&VfsPath::root(""))?;
        let expanded = vfs.shell_expand(path).await?;
        Ok(VfsPath::root(expanded))
    }

    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.poll_changes(&local_path).await
    }

    async fn list_files(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
    ) -> Result<FileList, Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.list_files(&local_path, options).await
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
    Archive { host_path: VfsPath },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResponse {
    pub vfs_id: VfsId,
    pub capabilities: VfsCapabilities,
}
