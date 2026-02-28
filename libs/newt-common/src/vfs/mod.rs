pub mod local;
pub mod s3;

pub use local::{LocalVfs, LocalVfsDescriptor, LOCAL_VFS_DESCRIPTOR};
pub use s3::{S3Vfs, S3VfsDescriptor};

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use log::{debug, info};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileDetails, FileReader};
use crate::filesystem::{File, FileList, Filesystem, FsStats, ListFilesOptions};
use crate::rpc::Communicator;
use crate::Error;

// ---------------------------------------------------------------------------
// VfsId
// ---------------------------------------------------------------------------

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
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
// Breadcrumb — a segment in a display path
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub label: String,
    pub nav_path: String,
}

// ---------------------------------------------------------------------------
// VfsDescriptor — type-level metadata for a VFS implementation
// ---------------------------------------------------------------------------

pub trait VfsDescriptor: Send + Sync + std::fmt::Debug {
    fn type_name(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn auto_mount_request(&self) -> Option<MountRequest>;

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
    fn can_fs_stats(&self) -> bool;

    // --- Same-VFS fast paths ---
    fn can_rename(&self) -> bool;
    fn can_copy_within(&self) -> bool;
    fn can_hard_link(&self) -> bool;

    // --- Display ---
    fn format_path(&self, path: &Path) -> String;
    fn breadcrumbs(&self, path: &Path) -> Vec<Breadcrumb>;

    /// Try to parse a user-entered display path. Returns the VFS-internal path
    /// if this VFS recognizes the input (e.g., S3 recognizes "s3://...").
    /// Returns None if this VFS doesn't claim the input.
    fn try_parse_display_path(&self, input: &str) -> Option<PathBuf>;
}

// Auto-registration via inventory
pub struct RegisteredDescriptor(pub &'static dyn VfsDescriptor);
inventory::collect!(RegisteredDescriptor);

pub fn lookup_descriptor(type_name: &str) -> Option<&'static dyn VfsDescriptor> {
    inventory::iter::<RegisteredDescriptor>()
        .find(|r| r.0.type_name() == type_name)
        .map(|r| r.0)
}

pub fn all_descriptors() -> impl Iterator<Item = &'static dyn VfsDescriptor> {
    inventory::iter::<RegisteredDescriptor>().map(|r| r.0)
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
// VfsSpaceInfo
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsSpaceInfo {
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// VfsChangeNotifier — reusable self-notification for VFS implementations
// ---------------------------------------------------------------------------

/// Allows VFS implementations to signal their own panes when they mutate
/// objects.  Call [`watch`] from `poll_changes` and [`notify`] after any
/// mutation.  Watchers whose prefix matches the modified path are signalled.
#[derive(Clone)]
pub struct VfsChangeNotifier {
    watchers: Arc<Mutex<Vec<(u64, PathBuf, tokio::sync::oneshot::Sender<()>)>>>,
    next_id: Arc<AtomicU64>,
}

impl VfsChangeNotifier {
    pub fn new() -> Self {
        Self {
            watchers: Arc::new(Mutex::new(Vec::new())),
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Register a watcher for `path` and wait until a matching mutation is
    /// notified.  The watcher is automatically removed if the future is
    /// dropped (e.g. the pane navigates away).
    pub async fn watch(&self, path: &Path) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.watchers.lock().push((id, path.to_path_buf(), tx));
        let _guard = WatcherGuard {
            id,
            watchers: self.watchers.clone(),
        };
        let _ = rx.await;
    }

    /// Signal all watchers whose watched prefix is a parent of
    /// `modified_path`.
    pub fn notify(&self, modified_path: &Path) {
        let mut guard = self.watchers.lock();
        let old = std::mem::take(&mut *guard);
        for (id, prefix, sender) in old {
            if modified_path.starts_with(&prefix) {
                let _ = sender.send(());
            } else {
                guard.push((id, prefix, sender));
            }
        }
    }
}

impl Default for VfsChangeNotifier {
    fn default() -> Self {
        Self::new()
    }
}

struct WatcherGuard {
    id: u64,
    watchers: Arc<Mutex<Vec<(u64, PathBuf, tokio::sync::oneshot::Sender<()>)>>>,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.watchers.lock().retain(|(id, _, _)| *id != self.id);
    }
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
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<Vec<File>, Error>;
    async fn poll_changes(&self, path: &Path) -> Result<(), Error>;
    async fn fs_stats(&self, path: &Path) -> Result<Option<FsStats>, Error>;

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

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
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
    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::NotSupported)
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
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
        info!("vfs: mount id={} type={}", id, vfs.descriptor().type_name());
        self.vfs_map.write().insert(id, vfs);
        id
    }

    pub fn unmount(&self, id: VfsId) -> Option<Arc<dyn Vfs>> {
        if id == VfsId::ROOT {
            return None; // refuse to unmount ROOT
        }
        info!("vfs: unmount id={}", id);
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
        batch_tx: Option<mpsc::UnboundedSender<FileList>>,
    ) -> Result<FileList, Error> {
        let vfs_id = path.vfs_id;
        let (vfs, mut local_path) = self.registry.resolve(&path)?;
        loop {
            let fs_stats = if vfs.descriptor().can_fs_stats() {
                vfs.fs_stats(&local_path).await.unwrap_or(None)
            } else {
                None
            };

            let (inner_tx, forwarder) = if let Some(ref outer_tx) = batch_tx {
                let (tx, mut rx) = mpsc::unbounded_channel::<Vec<File>>();
                let outer_tx = outer_tx.clone();
                let vfs_path = VfsPath::new(vfs_id, local_path.clone());
                let fs_stats = fs_stats.clone();
                let handle = tokio::spawn(async move {
                    while let Some(files) = rx.recv().await {
                        let batch = FileList::new(vfs_path.clone(), files, fs_stats.clone());
                        if outer_tx.send(batch).is_err() {
                            break;
                        }
                    }
                });
                (Some(tx), Some(handle))
            } else {
                (None, None)
            };

            match vfs.list_files(&local_path, inner_tx).await {
                Ok(files) => {
                    if let Some(h) = forwarder {
                        let _ = h.await;
                    }
                    return Ok(FileList::new(
                        VfsPath::new(vfs_id, local_path),
                        files,
                        fs_stats,
                    ));
                }
                Err(Error::Io(e))
                    if matches!(
                        (e.kind(), options.strict),
                        (std::io::ErrorKind::NotFound, false)
                            | (std::io::ErrorKind::NotADirectory, _)
                    ) =>
                {
                    if let Some(h) = forwarder {
                        let _ = h.await;
                    }
                    if !local_path.pop() {
                        return Err(e.into());
                    }
                }
                Err(e) => {
                    if let Some(h) = forwarder {
                        let _ = h.await;
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn rename(&self, old_path: VfsPath, new_path: VfsPath) -> Result<(), Error> {
        debug!("vfs_registry_fs: rename {} -> {}", old_path, new_path);
        if old_path.vfs_id != new_path.vfs_id {
            return Err(Error::Custom("cannot rename across VFS boundaries".into()));
        }
        let (vfs, old_local) = self.registry.resolve(&old_path)?;
        if vfs.descriptor().can_rename() {
            vfs.rename(&old_local, &new_path.path).await
        } else {
            Err(Error::NotSupported)
        }
    }

    async fn touch(&self, path: VfsPath) -> Result<(), Error> {
        debug!("vfs_registry_fs: touch {}", path);
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.touch(&local_path).await
    }

    async fn create_directory(&self, path: VfsPath) -> Result<(), Error> {
        debug!("vfs_registry_fs: create_directory {}", path);
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.create_directory(&local_path).await
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
    async fn file_details(&self, path: VfsPath) -> Result<FileDetails, Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.file_details(&local_path).await
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
