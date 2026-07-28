pub mod agent;
pub mod archive;
pub mod background_job;
pub mod change_notifier;
pub mod disc;
pub mod file;
pub mod local;
pub mod mount;
pub mod native;
pub mod origin;
pub mod path;
pub mod path_style;
mod pipelined_read;
pub mod progress;
pub mod properties;
pub mod registry;
pub mod remote;
pub mod s3;
pub mod search;
pub mod sftp;
pub mod volume;

pub use agent::{AGENT_VFS_DESCRIPTOR, AgentVfsDescriptor};
pub use archive::{TarArchiveVfs, ZipArchiveVfs, is_archive_name, is_zip_name};
pub use background_job::{BackgroundJob, ConsumerGuard, JobHandle, JobStatus, RestartPolicy};
pub use change_notifier::VfsChangeNotifier;
pub use disc::{DiscVfs, is_disc_image_name};
pub use file::{File, FileChunk, FileDetails, FileList, FsStats, Mode, ToUnix, UserGroup};
pub use local::{LOCAL_VFS_DESCRIPTOR, LocalVfs, LocalVfsDescriptor};
pub use mount::{
    MountContext, MountRequest, MountResponse, MountedVfsInfo, SftpAskpass, VfsManager,
    VfsManagerRemote, VfsRegistryManager, enterable_mount_request,
};
pub use path_style::{
    PathStyle, encode_mount_meta, encode_mount_meta_labeled, mount_meta_kind, mount_meta_label,
    mount_root_infos, mount_roots, unix_breadcrumbs, unix_display_path,
};
pub use progress::{
    NoopProgressSink, ProgressReporter, RemoteProgressSink, ScopedReporter, VfsProgress,
    VfsProgressSink,
};
pub use properties::{
    PropertyField, PropertyFieldValue, PropertyGrant, PropertyGrantee, PropertyGroup,
    PropertyPatch, PropertyPatchOp, PropertySheet, PropertyValuePatch, fold_sheets,
};
pub use registry::{
    RegisteredDescriptor, VfsRegistry, VfsRegistryFs, all_descriptors, lookup_descriptor,
};
pub use remote::{REMOTE_VFS_DESCRIPTOR, RemoteVfs, RemoteVfsDescriptor};
pub use s3::{S3Credentials, S3Vfs, S3VfsDescriptor};
pub use search::{SEARCH_VFS_DESCRIPTOR, SearchParams, SearchVfs, SearchVfsDescriptor};
pub use sftp::SftpVfs;
pub use volume::{RootInfo, VolumeInfo, VolumeKind};

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::Error;

/// Default chunk size for VFS read/copy buffers and streaming channels.
///
/// Used by file copy loops, the RPC dispatcher chunking host→agent reads,
/// and archive/SFTP streaming readers. 64 KiB is large enough to amortise
/// syscall/RPC overhead without holding much memory per in-flight chunk.
pub const VFS_READ_CHUNK_SIZE: usize = 64 * 1024;

/// Maximum in-VFS symlink hops before declaring a loop, for backends that
/// resolve symlinks against their own index (archives, disc images).
/// Matches Linux MAXSYMLINKS.
pub(crate) const MAX_SYMLINK_HOPS: usize = 40;

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

/// Which optional per-entry metadata families a VFS actually populates
/// on its `File`s — drives which file-list columns a pane offers (see
/// `VfsDescriptor::metadata_traits`). A column whose family is absent
/// would only ever render empty cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, specta::Type)]
pub struct MetadataTraits {
    /// `File::user`/`group`/`mode` are real: Unix-shaped local/remote
    /// FSes, SFTP, tar archives, Rock Ridge disc images.
    pub unix_owner: bool,
    /// `File::attributes` carries Windows `FILE_ATTRIBUTE_*` bits
    /// (Windows-shaped local/remote FSes only).
    pub windows_attributes: bool,
}

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

/// How a VFS type relates to its `Vfs::origin` — the position in another
/// VFS it was mounted from. Governs what escaping `..` above the root
/// means (and terminal-cwd resolution through synthetic mounts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginKind {
    /// Standalone — no origin to escape to; `..` clamps at the root
    /// (local, S3, SFTP, Remote).
    None,
    /// The origin names the *entry* the mount was made of (an archive
    /// file). Escaping `..` pops it, landing beside the entry with it
    /// focused; terminal cwd is its enclosing directory.
    Entry,
    /// The origin is the *directory* the mount was derived from (a
    /// search root). Escaping `..` (and terminal cwd) lands in the
    /// origin itself.
    Directory,
}

pub trait VfsDescriptor: Send + Sync + std::fmt::Debug {
    fn type_name(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn auto_mount_request(&self) -> Option<MountRequest>;

    // --- Browse ---
    fn can_watch(&self) -> bool;

    // --- Read ---
    fn can_read(&self) -> bool;

    // --- Write ---
    fn can_overwrite(&self) -> bool;
    fn can_create_directory(&self) -> bool;
    fn can_create_symlink(&self) -> bool;
    fn can_touch(&self) -> bool;
    fn can_truncate(&self) -> bool;
    fn can_set_metadata(&self) -> bool;

    // --- Delete ---
    fn can_remove(&self) -> bool;
    fn can_remove_tree(&self) -> bool;
    /// Whether `Vfs::trash_item` is available (OS / freedesktop trash).
    /// Only real local filesystems — on whichever machine owns them —
    /// qualify. Impls overriding `trash_item` must override this too.
    fn can_trash(&self) -> bool {
        false
    }

    // --- Capabilities ---
    fn has_symlinks(&self) -> bool;
    fn can_stat_directories(&self) -> bool;
    fn can_fs_stats(&self) -> bool;

    /// Whether this VFS serves a property sheet (`Vfs::get_property_sheet`
    /// / `apply_properties`) — per-VFS extras beyond `VfsMetadata`, e.g.
    /// S3 ACLs and user metadata. Gates the sheet section in the
    /// Properties dialog (and the fetch, so VFSes without sheets pay
    /// nothing).
    fn has_extended_properties(&self) -> bool {
        false
    }

    // --- Same-VFS fast paths ---
    fn can_rename(&self) -> bool;
    fn can_copy_within(&self) -> bool;
    fn can_hard_link(&self) -> bool;

    // --- Origin ---
    /// How this VFS type relates to its `Vfs::origin`, if it has one.
    /// Must agree with `Vfs::origin()`: return [`OriginKind::None`] iff
    /// `origin()` is `None`.
    fn origin_kind(&self) -> OriginKind {
        OriginKind::None
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
    /// and breaks operation routing. When this returns `false`, cmd+f
    /// refines via [`search_params`](Self::search_params) when available,
    /// else transparently falls back to the in-pane quick filter.
    fn can_search(&self) -> bool {
        true
    }

    /// When this VFS is itself a search-results view, the params it was
    /// mounted with — cmd+f on it reopens the search dialog pre-filled
    /// with these (rooted at `origin`) instead of starting a nested
    /// search. `None` for everything else.
    fn search_params(&self, _mount_meta: &[u8]) -> Option<search::SearchParams> {
        None
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

    /// The root *containing* `path`: `/` on a unified-root FS, and the
    /// drive or share root on a split-root one — `/?/C:` for
    /// `/?/C:/Users/x`, `/?/UNC/server/share` for a path on that share.
    ///
    /// Answers what an absolute path fragment means when it carries no
    /// drive of its own: on Windows a leading `\` is drive-*relative*, so
    /// `\` and `\Users` resolve against the drive you are already on
    /// rather than against the filesystem's abstract root (which is the
    /// unlistable `\?\` position anyway).
    ///
    /// Derived from [`navigable_parent`](Self::navigable_parent) rather
    /// than from [`roots`](Self::roots) so it lands exactly where holding
    /// `..` down does, by construction. The walk is bounded by the path's
    /// own depth — a `navigable_parent` step never lengthens a path, so
    /// the bound is only ever reached at the root.
    fn root_of(&self, path: &Path, mount_meta: &[u8]) -> PathBuf {
        let mut current = path.to_owned();
        for _ in 0..path.components().count() {
            match self.navigable_parent(&current, mount_meta) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        current
    }

    /// The filesystem's root paths, each with its volume classification
    /// where known. One `/` for a unified-root FS (every network/archive
    /// VFS, and Unix local); one per drive/share for a split-root FS
    /// (Windows local, incl. a Windows client's FS exposed into a remote
    /// session). Recorded in `mount_meta` at mount time.
    fn roots(&self, _mount_meta: &[u8]) -> Vec<volume::RootInfo> {
        vec![volume::RootInfo::root()]
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
                .map(|r| r.path)
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

    /// Which optional metadata families this VFS populates (see
    /// [`MetadataTraits`]). Default: none — right for S3,
    /// zip archives, and search results.
    fn metadata_traits(&self, _mount_meta: &[u8]) -> MetadataTraits {
        MetadataTraits::default()
    }
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

impl VfsMetadata {
    /// Derive from a listing entry — the natural source for VFSes whose
    /// attributes live in the index (archives, disc images, S3) rather than
    /// behind a stat call. Named users/groups don't map to numeric ids and
    /// are dropped.
    pub fn from_listing(file: &File) -> Self {
        fn id(ug: Option<&crate::vfs::UserGroup>) -> Option<u32> {
            match ug {
                Some(crate::vfs::UserGroup::Id(id)) => Some(*id),
                _ => None,
            }
        }
        fn time(ms: i64) -> Option<SystemTime> {
            let d = std::time::Duration::from_millis(ms.unsigned_abs());
            if ms >= 0 {
                SystemTime::UNIX_EPOCH.checked_add(d)
            } else {
                SystemTime::UNIX_EPOCH.checked_sub(d)
            }
        }
        Self {
            permissions: file.mode.as_ref().map(|m| m.0),
            uid: id(file.user.as_ref()),
            gid: id(file.group.as_ref()),
            atime: file.accessed.and_then(time),
            mtime: file.modified.and_then(time),
        }
    }
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

/// Positioned reads over one file, behind a handle held open for the
/// caller's whole read session. This is the primitive the archive/disc
/// engines loop on; `Vfs::read_range` stays the one-shot convenience for
/// sporadic access (a viewer chunk, a mime sniff). The handle pins the
/// file's identity where the backend can (an open fd, a pinned ETag), so
/// a file replaced mid-session doesn't get its bytes mixed across calls.
///
/// `read_at` fills `len` fully unless the file ends short; empty means
/// nothing at or past `offset`. Backends where probing past the end is an
/// error rather than a short read (S3 range GETs) rely on callers clamping
/// to a known file size — same contract as `read_range`.
#[async_trait::async_trait]
pub trait VfsRandomReader: Send {
    async fn read_at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Error>;
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

    async fn open_read_at(&self, path: &Path) -> Result<Box<dyn VfsRandomReader>, Error> {
        let _ = path;
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

    /// Move a file or an entire directory tree to the OS trash.
    async fn trash_item(&self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    // --- Metadata ---
    /// The default derives from `file_info`, so any VFS that surfaces
    /// mode/owner/timestamps in listings automatically supports metadata
    /// preservation as a copy source. Override where a dedicated stat call
    /// is cheaper or more precise (local, SFTP), or to answer for the
    /// remote side (`RemoteVfs`).
    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        Ok(VfsMetadata::from_listing(&self.file_info(path).await?))
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let _ = (path, meta);
        Err(Error::not_supported())
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    // --- Extended properties ---
    // VFSes overriding these must also override
    // `VfsDescriptor::has_extended_properties`.
    async fn get_property_sheet(&self, path: &Path) -> Result<PropertySheet, Error> {
        let _ = path;
        Err(Error::not_supported())
    }

    async fn apply_properties(&self, path: &Path, patch: &PropertyPatch) -> Result<(), Error> {
        let _ = (path, patch);
        Err(Error::not_supported())
    }

    // --- Identity ---
    /// Whether two paths in this VFS denote the same file — what the
    /// *filesystem* thinks, not what string comparison thinks. Case
    /// folding, Unicode normalization (HFS+ NFD), Windows short names and
    /// trailing-dot stripping, hardlinks and bind mounts all let distinct
    /// strings name one file, and no amount of folding on our side gets
    /// that right: NTFS's uppercase table is frozen per-volume, APFS has
    /// its own, and ext4 casefold is per-*directory*. So ask the FS.
    ///
    /// Callers use this to tell a genuine conflict from a re-spelling:
    /// copying a file onto itself is refused outright, while renaming
    /// `Foo` to `foo` on a case-insensitive volume is a legitimate rename
    /// rather than an `AlreadyExists`.
    ///
    /// `false` when either path is absent — a destination that doesn't
    /// exist is nothing's twin.
    ///
    /// The default answers by exact path equality, which is *correct*
    /// (not merely conservative) for byte-keyed namespaces: S3, archives,
    /// search results. Implementors backed by a real filesystem must
    /// override it, `RemoteVfs` included — inheriting the default there
    /// would quietly answer for the wrong machine.
    async fn same_file(&self, a: &Path, b: &Path) -> Result<bool, Error> {
        Ok(a.as_wire_str() == b.as_wire_str())
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
