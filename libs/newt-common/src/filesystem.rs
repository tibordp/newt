use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use bytes::Bytes;
use futures::Stream;
use tokio::sync::mpsc;

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails, SearchMatch, SearchPattern};
use crate::rpc::Communicator;
use crate::vfs::properties::PropertySheet;
use crate::vfs::{VfsId, VfsPath, VfsRandomReader};

/// Channel capacity for streaming file-list batches back to the UI.
pub const LIST_BATCH_CHANNEL_CAPACITY: usize = 16;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct StreamId(pub u64);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ListFilesOptions {
    pub strict: bool,
}

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
    volume: Option<crate::vfs::VolumeInfo>,
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

    pub fn with_volume(mut self, volume: Option<crate::vfs::VolumeInfo>) -> Self {
        self.volume = volume;
        self
    }

    pub fn available_bytes(&self) -> u64 {
        self.available_bytes
    }

    pub fn volume(&self) -> Option<&crate::vfs::VolumeInfo> {
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

/// The app shell's filesystem surface: the `Vfs` trait adapted for direct
/// use by the shell (panes, viewer/editor, dialogs, file server, `newt`
/// CLI). `VfsPath`-addressed — registry resolution and cross-VFS redirects
/// happen behind it — remoted as one dispatcher, and hairpin-diverted per
/// method in remote sessions. Membership test for new verbs: needs
/// session-side data locality, one round trip per user gesture. The
/// operations framework does not go through this; it sits on the registry
/// side and speaks `Vfs` directly.
#[async_trait::async_trait]
pub trait Filesystem: Send + Sync {
    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error>;
    async fn list_files(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
        batch_tx: Option<mpsc::Sender<FileList>>,
    ) -> Result<FileList, Error>;
    async fn touch(&self, path: VfsPath) -> Result<(), Error>;
    async fn create_directory(&self, path: VfsPath) -> Result<(), Error>;

    /// Volume stats (free/total bytes + classification) for the volume
    /// containing `path`. `Ok(None)` where the VFS has no volume notion.
    async fn fs_stats(&self, path: VfsPath) -> Result<Option<FsStats>, Error>;

    /// Revalidate the VFS identified by `vfs_id`. The navigation layer
    /// calls this when a pane is about to land on a path inside `vfs_id`
    /// after having been outside of it, giving the VFS a chance to detect
    /// drift and rebuild internal state without losing mount identity.
    ///
    /// Callers should consult `VfsDescriptor::can_revalidate` first and
    /// skip the call entirely for VFSes that don't need it (e.g. local
    /// FS) — this avoids an RPC round-trip in remote sessions for the
    /// common case.
    async fn revalidate(
        &self,
        vfs_id: crate::vfs::VfsId,
    ) -> Result<crate::vfs::RevalidationOutcome, Error>;

    // --- File content ---

    async fn file_details(&self, path: VfsPath) -> Result<FileDetails, Error>;

    /// Per-VFS extras beyond `FileDetails` (S3 ACLs, user metadata) for
    /// the Properties dialog. Tolerated exception to the data-plane
    /// membership test — it lives here because it needs the same
    /// session-side resolution and there is no better surface for a
    /// single verb.
    async fn get_property_sheet(&self, path: VfsPath) -> Result<PropertySheet, Error>;

    async fn read_range(&self, path: VfsPath, offset: u64, length: u64)
    -> Result<FileChunk, Error>;

    /// Positioned-read handle for multi-chunk loops (see
    /// `Vfs::open_read_at`). Callers that know they will issue a single
    /// chunk should stay on `read_range` — one round trip, no session to
    /// reap.
    async fn open_read_at(&self, path: VfsPath) -> Result<Box<dyn VfsRandomReader>, Error>;

    async fn read_file(&self, path: VfsPath, max_size: u64) -> Result<Vec<u8>, Error>;
    async fn write_file(&self, path: VfsPath, data: Vec<u8>) -> Result<(), Error>;
    async fn find_in_file(
        &self,
        path: VfsPath,
        offset: u64,
        pattern: SearchPattern,
        max_length: u64,
    ) -> Result<Option<SearchMatch>, Error>;
}

pub type PendingStreams = Arc<parking_lot::Mutex<HashMap<StreamId, mpsc::Sender<FileList>>>>;

pub struct Remote {
    communicator: Communicator,
    pending_streams: PendingStreams,
    next_stream_id: AtomicU64,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self {
            communicator,
            pending_streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            next_stream_id: AtomicU64::new(1),
        }
    }

    pub fn new_with_streams(communicator: Communicator, pending_streams: PendingStreams) -> Self {
        Self {
            communicator,
            pending_streams,
            next_stream_id: AtomicU64::new(1),
        }
    }

    pub fn pending_streams(&self) -> &PendingStreams {
        &self.pending_streams
    }
}

#[async_trait::async_trait]
impl Filesystem for Remote {
    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_POLL_CHANGES, &path)
            .await?;

        Ok(ret?)
    }
    async fn list_files(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
        batch_tx: Option<mpsc::Sender<FileList>>,
    ) -> Result<FileList, Error> {
        if let Some(batch_tx) = batch_tx {
            let stream_id = StreamId(
                self.next_stream_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );

            // Register the batch sender so notifications can be routed to it.
            self.pending_streams.lock().insert(stream_id, batch_tx);

            // RAII guard to ensure cleanup even on cancellation/error.
            struct StreamGuard {
                stream_id: StreamId,
                pending_streams: PendingStreams,
            }
            impl Drop for StreamGuard {
                fn drop(&mut self) {
                    self.pending_streams.lock().remove(&self.stream_id);
                }
            }
            let _guard = StreamGuard {
                stream_id,
                pending_streams: self.pending_streams.clone(),
            };

            let ret: Result<FileList, Error> = self
                .communicator
                .invoke(
                    crate::api::API_LIST_FILES_STREAMING,
                    &(path, options, stream_id),
                )
                .await?;

            Ok(ret?)
        } else {
            let ret: Result<FileList, Error> = self
                .communicator
                .invoke(crate::api::API_LIST_FILES, &(path, options))
                .await?;

            Ok(ret?)
        }
    }
    async fn touch(&self, path: VfsPath) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_TOUCH, &path)
            .await?;

        Ok(ret?)
    }

    async fn create_directory(&self, path: VfsPath) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_CREATE_DIRECTORY, &path)
            .await?;

        Ok(ret?)
    }

    async fn fs_stats(&self, path: VfsPath) -> Result<Option<FsStats>, Error> {
        let ret: Result<Option<FsStats>, Error> = self
            .communicator
            .invoke(crate::api::API_FS_STATS, &path)
            .await?;

        Ok(ret?)
    }

    async fn revalidate(
        &self,
        vfs_id: crate::vfs::VfsId,
    ) -> Result<crate::vfs::RevalidationOutcome, Error> {
        let ret: Result<crate::vfs::RevalidationOutcome, Error> = self
            .communicator
            .invoke(crate::api::API_REVALIDATE, &vfs_id)
            .await?;
        ret
    }

    async fn file_details(&self, path: VfsPath) -> Result<FileDetails, Error> {
        let ret: Result<FileDetails, Error> = self
            .communicator
            .invoke(crate::api::API_FILE_DETAILS, &path)
            .await?;

        Ok(ret?)
    }

    async fn get_property_sheet(&self, path: VfsPath) -> Result<PropertySheet, Error> {
        let ret: Result<PropertySheet, Error> = self
            .communicator
            .invoke(crate::api::API_GET_PROPERTY_SHEET, &path)
            .await?;

        Ok(ret?)
    }

    async fn read_range(
        &self,
        path: VfsPath,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, Error> {
        let ret: Result<FileChunk, Error> = self
            .communicator
            .invoke(crate::api::API_READ_RANGE, &(path, offset, length))
            .await?;

        Ok(ret?)
    }

    async fn open_read_at(&self, path: VfsPath) -> Result<Box<dyn VfsRandomReader>, Error> {
        let stream_id: Result<StreamId, Error> = self
            .communicator
            .invoke(crate::api::API_OPEN_READ_AT, &path)
            .await?;
        Ok(Box::new(crate::api::RemoteRandomReader {
            stream_id: stream_id?,
            communicator: self.communicator.clone(),
            read_api: crate::api::API_READ_AT,
            close_api: crate::api::API_READ_AT_CLOSE,
        }))
    }

    async fn read_file(&self, path: VfsPath, max_size: u64) -> Result<Vec<u8>, Error> {
        let ret: Result<serde_bytes::ByteBuf, Error> = self
            .communicator
            .invoke(crate::api::API_READ_FILE, &(path, max_size))
            .await?;

        Ok(ret?.into_vec())
    }

    async fn write_file(&self, path: VfsPath, data: Vec<u8>) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                crate::api::API_WRITE_FILE,
                &(path, serde_bytes::Bytes::new(&data)),
            )
            .await?;

        Ok(ret?)
    }

    async fn find_in_file(
        &self,
        path: VfsPath,
        offset: u64,
        pattern: SearchPattern,
        max_length: u64,
    ) -> Result<Option<SearchMatch>, Error> {
        let ret: Result<Option<SearchMatch>, Error> = self
            .communicator
            .invoke(
                crate::api::API_FIND_IN_FILE,
                &(path, offset, pattern, max_length),
            )
            .await?;
        Ok(ret?)
    }
}

/// Byte stream with stringly-typed errors — the shape that crosses the
/// shell-control HTTP boundary and the `read_file` data plane.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

/// Stream a file through the session `Filesystem` in 1 MiB chunks — the
/// shared `cat` data plane for both host (local session) and agent (remote
/// session).
/// One positioned-read handle spans the whole stream; it opens lazily on
/// the first poll so this stays a plain constructor.
pub fn file_reader_stream(reader: Arc<dyn Filesystem>, path: VfsPath) -> ByteStream {
    const CHUNK: u64 = 1024 * 1024;
    type Handle = Box<dyn VfsRandomReader>;
    Box::pin(futures::stream::try_unfold(
        (None::<Handle>, Some(0u64)),
        move |(handle, state)| {
            let reader = reader.clone();
            let path = path.clone();
            async move {
                let Some(offset) = state else {
                    return Ok(None);
                };
                let mut handle = match handle {
                    Some(handle) => handle,
                    None => reader.open_read_at(path).await.map_err(|e| e.to_string())?,
                };
                let data = handle
                    .read_at(offset, CHUNK)
                    .await
                    .map_err(|e| e.to_string())?;
                if data.is_empty() {
                    return Ok(None);
                }
                // A short chunk is EOF (read_at fills fully otherwise).
                let next = (data.len() as u64 == CHUNK).then(|| offset + CHUNK);
                Ok(Some((Bytes::from(data), (Some(handle), next))))
            }
        },
    ))
}
