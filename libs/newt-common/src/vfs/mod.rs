pub mod agent;
pub mod archive;
pub mod background_job;
pub mod k8s;
pub mod local;
pub mod path;
pub mod path_style;
pub mod progress;
pub mod remote;
pub mod s3;
pub mod search;
pub mod sftp;

#[cfg(test)]
#[path = "../vfs_tests.rs"]
mod tests;

pub use agent::{AGENT_VFS_DESCRIPTOR, AgentVfsDescriptor};
pub use archive::{TarArchiveVfs, ZipArchiveVfs, is_archive_name, is_zip_name};
pub use background_job::{BackgroundJob, ConsumerGuard, JobHandle, JobStatus, RestartPolicy};
pub use k8s::K8sVfs;
pub use local::{LOCAL_VFS_DESCRIPTOR, LocalVfs, LocalVfsDescriptor};
pub use path_style::{
    PathStyle, encode_mount_meta, encode_mount_meta_labeled, mount_meta_kind, mount_meta_label,
    mount_roots,
};
pub use progress::{
    NoopProgressSink, ProgressReporter, RemoteProgressSink, ScopedReporter, VfsProgress,
    VfsProgressSink,
};
pub use remote::{REMOTE_VFS_DESCRIPTOR, RemoteVfs, RemoteVfsDescriptor};
pub use s3::{S3Vfs, S3VfsDescriptor};
pub use search::{SEARCH_VFS_DESCRIPTOR, SearchParams, SearchVfs, SearchVfsDescriptor};
pub use sftp::SftpVfs;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::SystemTime;

use log::{debug, info};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails, FileReader, SearchMatch, SearchPattern};
use crate::filesystem::{File, FileList, Filesystem, FsStats, ListFilesOptions};
use crate::rpc::Communicator;

/// Default chunk size for VFS read/copy buffers and streaming channels.
///
/// Used by file copy loops, async-read bridge tasks, the RPC dispatcher
/// chunking host→agent reads, and archive/SFTP streaming readers. 64 KiB
/// is large enough to amortise syscall/RPC overhead without holding much
/// memory per in-flight chunk.
pub const VFS_READ_CHUNK_SIZE: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// VfsId
// ---------------------------------------------------------------------------

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    specta::Type,
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

/// `/`-rooted display string for a segment list. Used by the descriptors
/// of every Unix-path-speaking VFS (SFTP, S3, archives, …) in their
/// `format_path` impls.
pub fn unix_display_path(path: &Path) -> String {
    path.as_wire_str().to_string()
}

/// Breadcrumb list for a Unix-style path. Each breadcrumb's `nav_path`
/// is the corresponding prefix as a `/`-rooted string.
pub fn unix_breadcrumbs(path: &Path) -> Vec<Breadcrumb> {
    let comps: Vec<&str> = path.components().collect();
    let mut crumbs = Vec::with_capacity(comps.len() + 1);
    crumbs.push(Breadcrumb {
        label: "/".to_string(),
        nav_path: "/".to_string(),
    });
    let mut accumulated = String::new();
    for (i, seg) in comps.iter().enumerate() {
        accumulated.push('/');
        accumulated.push_str(seg);
        let is_last = i == comps.len() - 1;
        crumbs.push(Breadcrumb {
            label: if is_last {
                (*seg).to_string()
            } else {
                format!("{}/", seg)
            },
            nav_path: accumulated.clone(),
        });
    }
    crumbs
}

// ---------------------------------------------------------------------------
// VfsPath
// ---------------------------------------------------------------------------
//
// The type itself lives in `vfs::path`; re-exported here so the public path
// `newt_common::vfs::VfsPath` stays stable.

pub use path::VfsPath;
// Within this module (and the Vfs trait surface) `Path`/`PathBuf` mean the
// platform-independent VFS path types, never `std::path`. The few places
// that still need the std types refer to them as `std::path::PathBuf`.
use path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Breadcrumb — a segment in a display path
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Breadcrumb {
    pub label: String,
    pub nav_path: String,
}

// ---------------------------------------------------------------------------
// VfsDescriptor — type-level metadata for a VFS implementation
// ---------------------------------------------------------------------------

/// Result of `try_parse_display_path`. Lower priority values are preferred.
/// Within the same priority, mount order (first mounted wins) is used as
/// a tiebreaker via stable sort.
pub struct DisplayPathMatch {
    pub path: PathBuf,
    pub priority: DisplayPathPriority,
}

/// Priority for display path resolution. Variants are ordered from
/// highest priority (most specific) to lowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DisplayPathPriority {
    /// Exact scoped match (e.g., S3 mount for a specific bucket).
    Exact = 0,
    /// Generic prefix match (e.g., unscoped S3 mount matching any s3:// path).
    Generic = 1,
}

impl DisplayPathMatch {
    pub fn exact(path: PathBuf) -> Self {
        Self {
            path,
            priority: DisplayPathPriority::Exact,
        }
    }

    pub fn generic(path: PathBuf) -> Self {
        Self {
            path,
            priority: DisplayPathPriority::Generic,
        }
    }
}

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
    fn can_stat_directories(&self) -> bool;
    fn can_fs_stats(&self) -> bool;

    // --- Same-VFS fast paths ---
    fn can_rename(&self) -> bool;
    fn can_copy_within(&self) -> bool;
    fn can_hard_link(&self) -> bool;

    // --- Origin ---
    /// Whether this VFS type is grafted onto another VFS (e.g. archive mounts).
    /// When true, navigating `..` from the root should exit to the origin VFS.
    fn has_origin(&self) -> bool {
        false
    }

    /// Whether this VFS is "ephemeral" — short-lived, scoped to a single
    /// user action, and not something the user would want to navigate
    /// back to from a fresh selector (e.g. an archive mount tied to a
    /// specific origin file, or a search VFS whose params are baked in
    /// at mount time). Two consequences:
    ///
    /// 1. Auto-cleanup: the main window unmounts ephemeral VFSes that
    ///    no pane references — directly *or* via back/forward history.
    /// 2. UI: the VFS selector hides ephemeral mounts (they're reachable
    ///    via history; surfacing them as switchable destinations would
    ///    just be noise).
    ///
    /// Defaults to `false`. Override to `true` for synthetic / origin-
    /// derived VFSes.
    fn is_ephemeral(&self) -> bool {
        false
    }

    /// Whether panes on this VFS should auto-refresh on window focus.
    /// Defaults to true (suitable for local/remote filesystems). Override
    /// to false for VFSes where listing is expensive (S3, SFTP, archives).
    fn auto_refresh(&self) -> bool {
        true
    }

    /// Whether this VFS implements `Vfs::revalidate`. The navigation layer
    /// uses this to skip the call (and its RPC round-trip in remote
    /// sessions) for VFSes that hold no cached external state — e.g. the
    /// local filesystem. VFSes that override `revalidate` should also
    /// return `true` here.
    fn can_revalidate(&self) -> bool {
        false
    }

    /// Whether the recursive-search dialog (cmd+f) makes sense on a pane
    /// mounted on this VFS. Defaults to `true` — override only for VFSes
    /// where stacking a fresh search on top is incoherent. The motivating
    /// case is the search VFS itself: its entries are aliases to files in
    /// the underlying source, and a nested search produces duplicate keys
    /// and breaks operation routing. When this returns `false`, the host
    /// transparently falls back to the in-pane quick filter.
    fn can_search(&self) -> bool {
        true
    }

    // --- Display ---
    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String;
    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb>;

    /// Logical parent of `path` within this VFS, or `None` if the path is
    /// at a root the user can't navigate above. Consulted by the pane's
    /// `..`-handler and any code that needs to walk upward. Different
    /// from `path.parent()` because some VFSes have non-trivial "root"
    /// boundaries — e.g. the local VFS on Windows refuses to navigate
    /// above a drive or share root.
    ///
    /// Default: pop one component; `None` only when already at the root.
    fn navigable_parent(&self, path: &Path, _mount_meta: &[u8]) -> Option<PathBuf> {
        path.parent().map(Path::to_owned)
    }

    /// The filesystem's root paths. One `/` for a unified-root FS (every
    /// network/archive VFS, and Unix local); one per drive/share for a
    /// split-root FS (Windows local, incl. a Windows client's FS exposed
    /// into a remote session). Recorded in `mount_meta` at mount time.
    fn roots(&self, _mount_meta: &[u8]) -> Vec<PathBuf> {
        vec![PathBuf::root()]
    }

    /// Whether the FS has a single `/` root. When false the VFS selector
    /// surfaces each [`roots`](Self::roots) entry as its own drive.
    fn has_unified_root(&self, mount_meta: &[u8]) -> bool {
        self.roots(mount_meta).len() == 1
    }

    /// VFS-internal path to land on when this VFS is freshly selected or
    /// mounted (VFS selector, post-mount, unmount redirect). The abstract
    /// root `/` is correct for a unified-root FS but is the unlistable
    /// `\\?\` position on Windows — so a split-root FS lands on its first
    /// drive instead.
    fn initial_path(&self, mount_meta: &[u8]) -> PathBuf {
        if self.has_unified_root(mount_meta) {
            PathBuf::root()
        } else {
            self.roots(mount_meta)
                .into_iter()
                .next()
                .unwrap_or_else(PathBuf::root)
        }
    }

    /// Try to parse a user-entered display path. Returns the VFS-internal path
    /// if this VFS recognizes the input (e.g., S3 recognizes "s3://...").
    /// Returns None if this VFS doesn't claim the input.
    /// Returns `Exact` for scoped matches (e.g., S3 mount scoped to a specific
    /// bucket), `Generic` for prefix matches (e.g., unscoped S3 mount).
    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch>;

    /// Human-readable label for a mounted instance, derived from mount_meta.
    /// E.g. for SFTP this returns the hostname. Shown in the VFS selector
    /// next to the VFS display name.
    fn mount_label(&self, _mount_meta: &[u8]) -> Option<String> {
        None
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
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

type WatcherList = Vec<(u64, PathBuf, tokio::sync::oneshot::Sender<()>)>;

/// Allows VFS implementations to signal their own panes when they mutate
/// objects.  Call [`watch`] from `poll_changes` and [`notify`] after any
/// mutation.  Watchers whose prefix matches the modified path are signalled.
#[derive(Clone)]
pub struct VfsChangeNotifier {
    watchers: Arc<Mutex<WatcherList>>,
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
        self.watchers.lock().push((id, path.to_owned(), tx));
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
    watchers: Arc<Mutex<WatcherList>>,
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

/// Outcome of a `Vfs::revalidate` pass. Conveyed back to the navigation
/// layer so it can decide whether to treat any local caches as stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevalidationOutcome {
    /// The VFS's cached state is current; nothing was rebuilt. Navigation
    /// can rely on previously-observed structure.
    Fresh,
    /// The VFS detected drift and rebuilt internal state in place. The
    /// VFS identity (`VfsId`, `mount_meta`, `origin`) is preserved, but
    /// any cached file listings / annotations / sizes the host or enrichers
    /// kept across the previous and current visit must be considered
    /// stale.
    Refreshed,
}

// ---------------------------------------------------------------------------
// Vfs trait
// ---------------------------------------------------------------------------

/// Return value of `Vfs::list_files`. Carries the entries plus a
/// `partial` bit that the VFS sets when the listing it served is
/// intrinsically incomplete — e.g. a SearchVfs whose walker was
/// cancelled before reaching `Done`. The flag persists across
/// navigations to the same VFS, so a re-visit to a Cancelled search
/// still shows the partial state correctly. `VfsRegistryFs::list_files`
/// hoists the bit onto the registry-level `FileList` for consumer-side
/// rendering.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct VfsFileList {
    pub files: Vec<File>,
    pub partial: bool,
}

impl From<Vec<File>> for VfsFileList {
    fn from(files: Vec<File>) -> Self {
        Self {
            files,
            partial: false,
        }
    }
}

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

    // (helpers defined below for impls to use)

    // --- Browse ---
    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<VfsFileList, Error>;
    async fn poll_changes(&self, path: &Path) -> Result<(), Error>;
    async fn fs_stats(&self, path: &Path) -> Result<Option<FsStats>, Error>;

    /// Optional redirect: a synthetic VFS (e.g. flat search results) maps
    /// its in-vfs paths to real `VfsPath`s in another VFS. The registry
    /// consults this in `dereference` and rewrites every leaf op (read,
    /// write, rename, delete, metadata, ...) to hit the underlying file.
    /// `list_files` is the deliberate exception — listing must still hit
    /// the synthetic VFS itself to return the result set.
    ///
    /// Default returns `None` for "no redirect".
    async fn redirect_target(&self, path: &Path) -> Option<VfsPath> {
        let _ = path;
        None
    }

    /// Revalidate this VFS's cached state against its underlying source.
    /// Called by the host's navigation layer when a pane is about to land
    /// on a path inside this VFS that wasn't its previous location — so a
    /// VFS that caches external state (an archive's central directory, an
    /// SFTP connection, etc.) can detect drift and rebuild that state
    /// without losing the mount's identity (`VfsId`, `mount_meta`,
    /// `origin`).
    ///
    /// VFSes that have something to do here must also override
    /// `VfsDescriptor::can_revalidate` so the navigation layer knows to
    /// dispatch the call (and pay the RPC round-trip in remote sessions).
    /// The default implementation returns `not_supported`, which the
    /// navigation layer treats as a programming error if it ever fires:
    /// reaching it means a descriptor advertised the capability while the
    /// `Vfs` impl didn't follow through.
    ///
    /// Returning `Refreshed` is an instruction to navigation-layer caches
    /// (file listings, enricher results) to treat any prior data for this
    /// VFS as stale; the next `list_files` will reflect the rebuilt
    /// state. Returning `Err` aborts the navigation; the pane is left at
    /// its previous path.
    async fn revalidate(&self) -> Result<RevalidationOutcome, Error> {
        Err(Error::not_supported())
    }

    // --- Read ---
    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let _ = (path, offset, length);
        Err(Error::not_supported())
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    // --- Write ---
    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    /// Create a symlink at `link` whose raw contents are `target`.
    /// `target` is opaque link text (may be relative, may contain `..`)
    /// — *not* a navigable VFS path — so it's a `&str`, not a `Path`.
    async fn create_symlink(&self, link: &Path, target: &str) -> Result<(), Error> {
        let _ = (link, target);
        Err(Error::not_supported())
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    // --- Delete ---
    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    // --- Metadata ---
    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let _ = (path, meta);
        Err(Error::not_supported())
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    // --- Same-VFS fast paths ---
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::not_supported())
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::not_supported())
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let _ = (link, target);
        Err(Error::not_supported())
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
            .ok_or_else(|| Error::custom(format!("VFS {} not found", vfs_path.vfs_id)))?;
        Ok((vfs, vfs_path.path.clone()))
    }

    /// Follow `Vfs::redirect_target` once: if the VFS at `vfs_path.vfs_id`
    /// reports a redirect for `vfs_path`, return the source path; else
    /// return the input unchanged. Used by `VfsRegistryFs` and
    /// `VfsRegistryFileReader` to make leaf operations transparent across
    /// synthetic VFSes (flat search results, etc.).
    pub async fn dereference(&self, vfs_path: &VfsPath) -> VfsPath {
        let Some(vfs) = self.get(vfs_path.vfs_id) else {
            return vfs_path.clone();
        };
        match vfs.redirect_target(&vfs_path.path).await {
            Some(target) => target,
            None => vfs_path.clone(),
        }
    }

    /// Reserve a fresh `VfsId` without inserting anything. Used by the
    /// manager when a VFS needs to know its id at construction time —
    /// allocate first, hand the id to the VFS (via a scoped progress
    /// reporter etc.), then `insert`.
    pub fn allocate_id(&self) -> VfsId {
        VfsId(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Insert a freshly-constructed VFS under a previously-allocated id.
    /// Panics if the id is already taken (programmer error — allocate
    /// always returns a fresh id).
    pub fn insert(&self, id: VfsId, vfs: Arc<dyn Vfs>) {
        info!("vfs: mount id={} type={}", id, vfs.descriptor().type_name());
        let prev = self.vfs_map.write().insert(id, vfs);
        assert!(prev.is_none(), "vfs_id {} already taken", id);
    }

    /// Convenience: allocate + insert in one shot. Use when the VFS
    /// doesn't need to know its id at construction.
    pub fn mount(&self, vfs: Arc<dyn Vfs>) -> VfsId {
        let id = self.allocate_id();
        self.insert(id, vfs);
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
        batch_tx: Option<mpsc::Sender<FileList>>,
    ) -> Result<FileList, Error> {
        let vfs = self
            .registry
            .get(path.vfs_id)
            .ok_or_else(|| Error::custom(format!("VFS {} not found", path.vfs_id)))?;
        let mut current = path;
        loop {
            let fs_stats = if vfs.descriptor().can_fs_stats() {
                vfs.fs_stats(&current.path).await.unwrap_or(None)
            } else {
                None
            };

            let (inner_tx, forwarder) = if let Some(ref outer_tx) = batch_tx {
                let (tx, mut rx) =
                    mpsc::channel::<Vec<File>>(crate::filesystem::LIST_BATCH_CHANNEL_CAPACITY);
                let outer_tx = outer_tx.clone();
                let vfs_path = current.clone();
                let fs_stats = fs_stats.clone();
                let handle = tokio::spawn(async move {
                    while let Some(files) = rx.recv().await {
                        let batch = FileList::new(vfs_path.clone(), files, fs_stats.clone());
                        if outer_tx.send(batch).await.is_err() {
                            break;
                        }
                    }
                });
                (Some(tx), Some(handle))
            } else {
                (None, None)
            };

            match vfs.list_files(&current.path, inner_tx).await {
                Ok(result) => {
                    if let Some(h) = forwarder {
                        let _ = h.await;
                    }
                    return Ok(
                        FileList::new(current, result.files, fs_stats).with_partial(result.partial)
                    );
                }
                Err(e)
                    if matches!(
                        (e.kind, options.strict),
                        (crate::ErrorKind::NotFound, false) | (crate::ErrorKind::NotADirectory, _)
                    ) =>
                {
                    if let Some(h) = forwarder {
                        let _ = h.await;
                    }
                    if !current.path.pop() {
                        return Err(e);
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
        // Deref the source side. `new_path` is a freshly-constructed target
        // path supplied by the caller; only the *source* can be a redirect.
        let old_path = self.registry.dereference(&old_path).await;
        if old_path.vfs_id != new_path.vfs_id {
            return Err(Error::custom("cannot rename across VFS boundaries"));
        }
        let (vfs, old_local) = self.registry.resolve(&old_path)?;
        if vfs.descriptor().can_rename() {
            vfs.rename(&old_local, &new_path.path).await
        } else {
            Err(Error::not_supported())
        }
    }

    async fn touch(&self, path: VfsPath) -> Result<(), Error> {
        debug!("vfs_registry_fs: touch {}", path);
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.touch(&local_path).await
    }

    async fn create_directory(&self, path: VfsPath) -> Result<(), Error> {
        debug!("vfs_registry_fs: create_directory {}", path);
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.create_directory(&local_path).await
    }

    async fn revalidate(&self, vfs_id: VfsId) -> Result<RevalidationOutcome, Error> {
        let vfs = self
            .registry
            .get(vfs_id)
            .ok_or_else(|| Error::custom(format!("unknown VFS id: {}", vfs_id)))?;
        // Mirror the descriptor capability gate: if the VFS doesn't claim
        // to support revalidation, treat it as a no-op rather than dispatching
        // and getting a `not_supported` back. This is the host-local short-
        // circuit; remote callers gate on the descriptor *before* the RPC.
        if !vfs.descriptor().can_revalidate() {
            return Ok(RevalidationOutcome::Fresh);
        }
        vfs.revalidate().await
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
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.file_details(&local_path).await
    }

    async fn read_range(
        &self,
        path: VfsPath,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.read_range(&local_path, offset, length).await
    }

    async fn read_file(&self, path: VfsPath, max_size: u64) -> Result<Vec<u8>, Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        let details = vfs.file_details(&local_path).await?;
        if details.size > max_size {
            return Err(Error::custom(format!(
                "File is too large to edit ({} bytes, limit is {} bytes)",
                details.size, max_size
            )));
        }
        let descriptor = vfs.descriptor();
        if descriptor.can_read_sync() {
            let mut reader = vfs.open_read_sync(&local_path).await?;
            let mut data = Vec::with_capacity(details.size as usize);
            std::io::Read::read_to_end(&mut reader, &mut data)?;
            Ok(data)
        } else if descriptor.can_read_async() {
            use tokio::io::AsyncReadExt;
            let mut reader = vfs.open_read_async(&local_path).await?;
            let mut data = Vec::with_capacity(details.size as usize);
            reader.read_to_end(&mut data).await?;
            Ok(data)
        } else {
            Err(Error::not_supported())
        }
    }

    async fn write_file(&self, path: VfsPath, data: Vec<u8>) -> Result<(), Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        let descriptor = vfs.descriptor();
        if descriptor.can_overwrite_sync() {
            let mut writer = vfs.overwrite_sync(&local_path).await?;
            std::io::Write::write_all(&mut writer, &data)?;
            Ok(())
        } else if descriptor.can_overwrite_async() {
            let mut writer = vfs.overwrite_async(&local_path).await?;
            writer.write(&data).await?;
            writer.finish().await?;
            Ok(())
        } else {
            Err(Error::not_supported())
        }
    }

    async fn find_in_file(
        &self,
        path: VfsPath,
        offset: u64,
        pattern: SearchPattern,
        max_length: u64,
    ) -> Result<Option<SearchMatch>, Error> {
        let compiled = compile_regex(&pattern)?;
        let overlap = compute_overlap(&pattern);
        let mut carry: Vec<u8> = Vec::new();
        let mut pos = offset;
        let end = offset.saturating_add(max_length);

        while pos < end {
            let chunk_len = std::cmp::min(SEARCH_CHUNK_SIZE as u64, end - pos);
            let chunk = self.read_range(path.clone(), pos, chunk_len).await?;
            if chunk.data.is_empty() {
                break;
            }

            let carry_len = carry.len();
            carry.extend_from_slice(&chunk.data);

            if let Some((match_pos, match_len)) =
                find_in_buffer(&carry, &pattern, compiled.as_ref())
            {
                let abs_offset = pos - carry_len as u64 + match_pos as u64;
                return Ok(Some(SearchMatch {
                    offset: abs_offset,
                    length: match_len as u64,
                }));
            }

            pos += chunk.data.len() as u64;

            // Keep overlap bytes for next iteration
            if carry.len() > overlap {
                let start = carry.len() - overlap;
                carry.drain(..start);
            }

            if chunk.data.len() < chunk_len as usize {
                break; // EOF
            }
        }

        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Search helpers for find_in_file
// ---------------------------------------------------------------------------

const SEARCH_CHUNK_SIZE: usize = 256 * 1024;

/// Maximum bytes carried over between search chunks for regex patterns. The
/// regex engine has no way to bound match length up front, so we have to
/// guess; 64 KiB covers any realistic regex while keeping the per-chunk
/// re-scan small.
const REGEX_OVERLAP_LIMIT: usize = 64 * 1024;

fn compute_overlap(pattern: &SearchPattern) -> usize {
    match pattern {
        SearchPattern::Literal(pat) => pat.len().saturating_sub(1),
        SearchPattern::Regex(_) => std::cmp::min(REGEX_OVERLAP_LIMIT, SEARCH_CHUNK_SIZE / 2),
    }
}

fn find_in_buffer(
    buf: &[u8],
    pattern: &SearchPattern,
    compiled_regex: Option<&regex::bytes::Regex>,
) -> Option<(usize, usize)> {
    match pattern {
        SearchPattern::Literal(pat) => memchr::memmem::find(buf, pat).map(|pos| (pos, pat.len())),
        SearchPattern::Regex(_) => compiled_regex?.find(buf).map(|m| (m.start(), m.len())),
    }
}

fn compile_regex(pattern: &SearchPattern) -> Result<Option<regex::bytes::Regex>, Error> {
    match pattern {
        SearchPattern::Regex(pat) => {
            let re = regex::bytes::Regex::new(pat).map_err(|e| Error::custom(e.to_string()))?;
            Ok(Some(re))
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Mount/unmount RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
pub struct S3Credentials {
    /// AWS access key ID (IAM user or assumed role).
    pub access_key_id: Option<String>,
    /// AWS secret access key.
    pub secret_access_key: Option<String>,
    /// AWS session token (for temporary credentials / AssumeRole).
    pub session_token: Option<String>,
    /// AWS profile name (from ~/.aws/config). Overrides default profile.
    pub profile: Option<String>,
    /// Custom endpoint URL (for S3-compatible services like MinIO, R2, etc.)
    pub endpoint_url: Option<String>,
    /// IAM role ARN to assume. When set, uses STS AssumeRole with the
    /// ambient or explicit credentials, then mounts with the resulting
    /// temporary credentials.
    pub role_arn: Option<String>,
    /// External ID for AssumeRole (optional, for cross-account access).
    pub external_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub enum MountRequest {
    S3 {
        region: Option<String>,
        /// When set, the VFS is scoped to this bucket (root = bucket contents).
        /// When None, root lists all buckets.
        bucket: Option<String>,
        #[serde(default)]
        credentials: S3Credentials,
    },
    Sftp {
        host: String,
    },
    Kubernetes {
        context: String,
    },
    Archive {
        origin: VfsPath,
    },
    Search {
        root: VfsPath,
        params: search::SearchParams,
    },
    Remote,
    /// Spawn an FS-only sub-agent over a transport (SSH, docker, …) and
    /// mount its local filesystem. See `vfs::agent`.
    Agent {
        spec: crate::connect::SpawnSpec,
        /// Transport kind shown as the VFS display name, e.g. `Docker`.
        kind: String,
        /// Mount target shown as the VFS label, e.g. the container name.
        label: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResponse {
    pub vfs_id: VfsId,
    pub type_name: String,
    pub mount_meta: Vec<u8>,
    pub origin: Option<VfsPath>,
}

// ---------------------------------------------------------------------------
// MountedVfsInfo — client-side descriptor + metadata for a mounted VFS
// ---------------------------------------------------------------------------

pub struct MountedVfsInfo {
    pub vfs_id: VfsId,
    pub descriptor: &'static dyn VfsDescriptor,
    pub mount_meta: Vec<u8>,
    pub origin: Option<VfsPath>,
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
