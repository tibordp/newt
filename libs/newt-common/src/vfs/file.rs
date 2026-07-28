//! The directory-entry data model: what a listing row *is*. Produced by
//! every VFS backend, consumed by the session `Filesystem` facade, the
//! operations framework, and enrichers, and serialized to the frontend.

use super::VfsId;
use super::path::VfsPath;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum UserGroup {
    Name(String),
    Id(u32),
}

impl PartialEq for UserGroup {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Name(a), Self::Name(b)) => a == b,
            (Self::Id(a), Self::Id(b)) => a == b,
            _ => false,
        }
    }
}

impl PartialOrd for UserGroup {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Name(a), Self::Name(b)) => a.partial_cmp(b),
            (Self::Id(a), Self::Id(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    Hash,
    specta::Type,
)]
pub struct Mode(pub u32);

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct File {
    pub name: String,
    pub size: Option<u64>,
    /// Bytes actually allocated on disk (`st_blocks`-based), when the
    /// source filesystem reports it — sparse files (VM disk images,
    /// Docker.raw, …) allocate far less than their apparent `size`.
    /// The du enricher sums this when present so computed directory
    /// sizes match `du`, not the apparent-size sum.
    pub allocated_size: Option<u64>,
    /// Filesystem identity (`st_dev`), when the source reports it.
    /// Lets consumers detect mount boundaries — the du walker stops at
    /// them (`du -x`), and a future `--one-file-system` delete guard
    /// needs the same signal.
    pub device_id: Option<u64>,
    /// Inode number (`st_ino`); with `device_id`, identifies a file
    /// across hardlinks.
    pub inode: Option<u64>,
    /// Hardlink count (`st_nlink`) — consumers only need the
    /// `(device_id, inode)` dedup for entries with more than one link.
    pub hard_links: Option<u64>,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
    /// Raw link target as reported by the source FS. A string, not a path
    /// type: it may be relative (`../x`) or otherwise un-normalizable, and
    /// it crosses the agent↔host RPC boundary — no `std::path` there, its
    /// meaning belongs to the source OS, not the receiver's.
    pub symlink_target: Option<String>,
    pub user: Option<UserGroup>,
    pub group: Option<UserGroup>,
    pub mode: Option<Mode>,
    /// Raw Windows `FILE_ATTRIBUTE_*` bits, populated only by
    /// Windows-shaped local listings (the Attr column renders them).
    pub attributes: Option<u32>,
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
    /// Directory-scoped identifier. When `None`, `name` is used as the
    /// identifier — the common case. Set explicitly by synthetic VFSes
    /// (e.g. flat search results, where `name` is the basename for display
    /// but multiple entries can share it). See `File::key()`.
    pub key: Option<String>,
    /// Underlying source path for entries that are virtual references to a
    /// real file in another VFS — e.g. a search result. Frontend uses this
    /// for the "where from" secondary display; backend treats it as
    /// informational (the operative redirect is in `VfsRegistry`, see
    /// `Vfs::redirect_target`).
    pub source: Option<VfsPath>,
}

impl File {
    /// Synthetic directory entry carrying no metadata — implicit archive
    /// ancestors, and (via [`File::parent_dir`]) the `..` row. Deliberately
    /// spells out every field so the compiler forces a decision here when
    /// the model grows one; backends listing real entries keep their own
    /// exhaustive literals for the same reason.
    pub fn bare_dir(name: impl Into<String>) -> Self {
        File {
            name: name.into(),
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
            attributes: None,
            modified: None,
            accessed: None,
            created: None,
            key: None,
            source: None,
        }
    }

    /// The `..` up-navigation entry every listing starts with (except at
    /// a mount root with nowhere to go up to).
    pub fn parent_dir() -> Self {
        Self::bare_dir("..")
    }

    /// Directory-scoped identifier. Falls back to `name` when `key` is unset,
    /// which is the case for every "real" filesystem entry. Synthetic VFSes
    /// (e.g. search results) set `key` explicitly so identity (selection,
    /// focus, joining into a `VfsPath`) is independent of the displayed
    /// `name`, which need not be unique.
    pub fn key(&self) -> &str {
        self.key.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FsStats {
    free_bytes: u64,
    available_bytes: u64,
    total_bytes: u64,
    /// Classification of the volume containing the listed directory,
    /// probed on the FS-owning side. `None` where probing failed or the
    /// VFS has no volume notion.
    volume: Option<super::VolumeInfo>,
}

impl FsStats {
    pub fn new(free_bytes: u64, available_bytes: u64, total_bytes: u64) -> Self {
        Self {
            free_bytes,
            available_bytes,
            total_bytes,
            volume: None,
        }
    }

    pub fn with_volume(mut self, volume: Option<super::VolumeInfo>) -> Self {
        self.volume = volume;
        self
    }

    pub fn available_bytes(&self) -> u64 {
        self.available_bytes
    }

    pub fn volume(&self) -> Option<&super::VolumeInfo> {
        self.volume.as_ref()
    }
}

#[cfg(unix)]
impl From<nix::sys::statvfs::Statvfs> for FsStats {
    #[allow(clippy::unnecessary_cast)]
    fn from(stats: nix::sys::statvfs::Statvfs) -> Self {
        Self {
            free_bytes: ((stats.blocks_available() as u64) * (stats.fragment_size() as u64)),
            available_bytes: ((stats.blocks_available() as u64) * (stats.fragment_size() as u64)),
            total_bytes: ((stats.blocks() as u64) * (stats.fragment_size() as u64)),
            volume: None,
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FileList {
    path: VfsPath,
    fs_stats: Option<FsStats>,
    files: Vec<File>,
    /// Set when the underlying VFS reports that the listing is
    /// intrinsically incomplete (e.g. a SearchVfs whose walker was
    /// cancelled). Surfaces in the pane status bar as `(partial)` and
    /// is sticky across navigations into the same VFS.
    partial: bool,
}

impl FileList {
    pub fn new(path: VfsPath, files: Vec<File>, fs_stats: Option<FsStats>) -> Self {
        Self {
            path,
            files,
            fs_stats,
            partial: false,
        }
    }

    pub fn with_partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    pub fn path(&self) -> &VfsPath {
        &self.path
    }

    pub fn files(&self) -> &[File] {
        &self.files
    }

    pub fn fs_stats(&self) -> Option<&FsStats> {
        self.fs_stats.as_ref()
    }

    pub fn is_partial(&self) -> bool {
        self.partial
    }

    /// Replace the VFS ID in this file list's path.
    pub fn rewrite_vfs_id(&mut self, vfs_id: VfsId) {
        self.path.vfs_id = vfs_id;
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FileDetails {
    pub size: u64,
    pub mime_type: Option<String>,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Raw link target as reported by the source FS (see `File::symlink_target`).
    pub symlink_target: Option<String>,
    pub user: Option<UserGroup>,
    pub group: Option<UserGroup>,
    pub mode: Option<Mode>,
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FileChunk {
    // serde_bytes: bincode's serde path walks Vec<u8> per byte; this hits
    // its bytes fast path with an identical wire format.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    pub offset: u64,
    pub total_size: u64,
}

/// Guess MIME type from a file path's extension.
/// Returns `None` if the extension is not recognized.
pub fn guess_mime_type(path: &std::path::Path) -> Option<String> {
    mime_guess::from_path(path)
        .first()
        .map(|m| m.essence_str().to_string())
}

/// `SystemTime` → the millisecond epoch timestamps carried on `File` /
/// `FileDetails`.
pub trait ToUnix {
    fn to_unix(&self) -> i64;
}

impl ToUnix for std::time::SystemTime {
    fn to_unix(&self) -> i64 {
        use std::time::SystemTime;
        let ms = self
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|t| t.as_millis() as i128)
            .unwrap_or_else(|e| -(e.duration().as_millis() as i128));
        ms.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }
}
