use tokio::sync::mpsc;

use crate::Error;
use crate::rpc::{Communicator, StreamId, StreamReceiver, StreamSender};
use crate::vfs::File;
use crate::vfs::FileChunk;
use crate::vfs::path::Path;

use super::{
    Breadcrumb, DisplayPathMatch, PathStyle, RegisteredDescriptor, Vfs, VfsAsyncWriter,
    VfsDescriptor, VfsMetadata, VfsSpaceInfo,
};
use crate::vfs::path::PathBuf;

// ---------------------------------------------------------------------------
// RemoteVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct RemoteVfsDescriptor;

impl VfsDescriptor for RemoteVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "remote"
    }
    fn display_name(&self) -> &'static str {
        "Remote"
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
        // Identical path shape to `LocalVfsDescriptor`; the style is
        // whatever the proxied end stamped into `mount_meta` (Unix for
        // every remote in scope, the client's OS for a client-local FS
        // exposed back into a remote session).
        super::path_style::local_display_path(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        super::path_style::local_breadcrumbs(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn navigable_parent(&self, path: &Path, mount_meta: &[u8]) -> Option<PathBuf> {
        super::path_style::navigable_parent(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn roots(&self, mount_meta: &[u8]) -> Vec<super::RootInfo> {
        // A Windows client's FS exposed into a remote session is
        // split-root; `has_unified_root`/`initial_path` then land on its
        // first drive instead of the unlistable `/`.
        super::path_style::roots_from_meta(mount_meta)
    }
    fn has_unified_root(&self, mount_meta: &[u8]) -> bool {
        // Style-based, not root-count (a single-drive Windows client is
        // still split-root). See `unified_root_from_meta`.
        super::path_style::unified_root_from_meta(mount_meta)
    }

    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        // Windows syntax is distinctive (`C:\…`, `\\server\share`), and a
        // Windows-styled client-local mount is the only place it can mean
        // anything in a Unix-rooted remote session — so claim it here and
        // Ctrl+L / Shift+<drive> land on the right VFS. Unix-style input
        // deliberately stays unclaimed: it belongs to the session root via
        // shell expansion on the agent.
        if PathStyle::from_mount_meta(mount_meta) != PathStyle::Windows {
            return None;
        }
        Some(DisplayPathMatch::exact(
            super::path::PathBuf::from_typed_display(input)?,
        ))
    }

    fn metadata_traits(&self, mount_meta: &[u8]) -> super::MetadataTraits {
        super::path_style::metadata_traits_from_meta(mount_meta)
    }
}

pub static REMOTE_VFS_DESCRIPTOR: RemoteVfsDescriptor = RemoteVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&REMOTE_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// RemoteVfs — proxies Vfs calls back to the host over RPC
// ---------------------------------------------------------------------------

pub struct RemoteVfs {
    communicator: Communicator,
    descriptor: &'static dyn VfsDescriptor,
    /// Owner-supplied: the proxied side describes its own filesystem
    /// (style + roots), so the meta arrives with the mount request and
    /// can be replaced on remount (drive changes) — see `set_mount_meta`.
    mount_meta: parking_lot::RwLock<Vec<u8>>,
    /// Keeps a spawned sub-agent (process, askpass listener) alive for the
    /// lifetime of the mount. `None` for the host-communicator flavor.
    _connection: Option<super::agent::AgentConnectionGuard>,
}

impl RemoteVfs {
    /// Build a `RemoteVfs` from a `MountRequest::Remote`. Pulls the host
    /// communicator (set up at session start by the agent) out of the mount
    /// context; the meta comes with the request — only the host can
    /// describe its own FS. Returns
    /// the concrete type so the mounting manager can keep a typed handle
    /// for later `set_mount_meta` (remount) dispatch.
    pub fn mount(
        ctx: &crate::vfs::mount::MountContext<'_>,
        mount_meta: Vec<u8>,
    ) -> Result<std::sync::Arc<Self>, crate::Error> {
        let communicator = ctx
            .host_communicator
            .get()
            .ok_or_else(|| crate::Error::custom("host communicator not available"))?
            .clone();
        Ok(std::sync::Arc::new(Self::new(communicator, mount_meta)))
    }

    /// Replace the owner-supplied `mount_meta` (logical remount: the host
    /// re-enumerated its drives and pushed the fresh description). Not a
    /// `Vfs` trait operation — every other VFS derives its own meta.
    pub fn set_mount_meta(&self, mount_meta: Vec<u8>) {
        *self.mount_meta.write() = mount_meta;
    }

    pub fn new(communicator: Communicator, mount_meta: Vec<u8>) -> Self {
        Self {
            communicator,
            descriptor: &REMOTE_VFS_DESCRIPTOR,
            mount_meta: parking_lot::RwLock::new(mount_meta),
            _connection: None,
        }
    }

    /// The same proxy pointed at a spawned FS-only sub-agent instead of the
    /// host: an agent mount. Owns the connection so unmount tears it down.
    pub fn for_agent(
        communicator: Communicator,
        mount_meta: Vec<u8>,
        connection: super::agent::AgentConnectionGuard,
    ) -> Self {
        Self {
            communicator,
            descriptor: &super::agent::AGENT_VFS_DESCRIPTOR,
            mount_meta: parking_lot::RwLock::new(mount_meta),
            _connection: Some(connection),
        }
    }
}

use crate::api::{
    API_VFS_AVAILABLE_SPACE, API_VFS_COPY_WITHIN, API_VFS_CREATE_DIRECTORY, API_VFS_CREATE_SYMLINK,
    API_VFS_FILE_INFO, API_VFS_FS_STATS, API_VFS_GET_METADATA, API_VFS_HARD_LINK,
    API_VFS_LIST_FILES, API_VFS_OPEN_READ_ASYNC, API_VFS_OPEN_READ_AT,
    API_VFS_OVERWRITE_ASYNC_BEGIN, API_VFS_POLL_CHANGES, API_VFS_READ_AT, API_VFS_READ_AT_CLOSE,
    API_VFS_READ_RANGE, API_VFS_REMOVE_DIR, API_VFS_REMOVE_FILE, API_VFS_REMOVE_TREE,
    API_VFS_RENAME, API_VFS_SAME_FILE, API_VFS_SET_METADATA, API_VFS_TOUCH, API_VFS_TRASH_ITEM,
    API_VFS_TRUNCATE,
};

#[async_trait::async_trait]
impl Vfs for RemoteVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        self.descriptor
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.mount_meta.read().clone()
    }

    async fn list_files(
        &self,
        path: &Path,
        _batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::VfsFileList, Error> {
        let ret: Result<super::VfsFileList, Error> = self
            .communicator
            .invoke(API_VFS_LIST_FILES, &path.to_owned())
            .await?;
        ret
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_POLL_CHANGES, &path.to_owned())
            .await?;
        ret
    }

    async fn fs_stats(&self, path: &Path) -> Result<Option<crate::vfs::FsStats>, Error> {
        let ret: Result<Option<crate::vfs::FsStats>, Error> = self
            .communicator
            .invoke(API_VFS_FS_STATS, &path.to_owned())
            .await?;
        ret
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>, Error> {
        let (stream_id, data) = self.communicator.open_stream();
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_OPEN_READ_ASYNC, &(path.to_owned(), stream_id))
            .await?;
        ret?;
        Ok(Box::new(data.into_async_read()))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let ret: Result<FileChunk, Error> = self
            .communicator
            .invoke(API_VFS_READ_RANGE, &(path.to_owned(), offset, length))
            .await?;
        ret
    }

    async fn open_read_at(&self, path: &Path) -> Result<Box<dyn super::VfsRandomReader>, Error> {
        let id: Result<crate::api::ReadAtId, Error> = self
            .communicator
            .invoke(API_VFS_OPEN_READ_AT, &path.to_owned())
            .await?;
        Ok(Box::new(crate::api::RemoteRandomReader {
            id: id?,
            communicator: self.communicator.clone(),
            read_api: API_VFS_READ_AT,
            close_api: API_VFS_READ_AT_CLOSE,
        }))
    }

    async fn file_info(&self, path: &Path, follow: bool) -> Result<File, Error> {
        let ret: Result<File, Error> = self
            .communicator
            .invoke(API_VFS_FILE_INFO, &(path.to_owned(), follow))
            .await?;
        ret
    }

    async fn overwrite_async(
        &self,
        path: &Path,
        options: &super::attributes::WriteOptions,
    ) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let (result_id, result) = self.communicator.open_stream();
        let data_id: Result<StreamId, Error> = self
            .communicator
            .invoke(
                API_VFS_OVERWRITE_ASYNC_BEGIN,
                &(path.to_owned(), options, result_id),
            )
            .await?;

        Ok(Box::new(RemoteVfsWriter {
            data: self.communicator.stream_sender(data_id?),
            result,
        }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_CREATE_DIRECTORY, &path.to_owned())
            .await?;
        ret
    }

    async fn create_symlink(&self, link: &Path, target: &str) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                API_VFS_CREATE_SYMLINK,
                &(link.to_owned(), target.to_string()),
            )
            .await?;
        ret
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_TOUCH, &path.to_owned())
            .await?;
        ret
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_TRUNCATE, &path.to_owned())
            .await?;
        ret
    }

    async fn write_range(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), Error> {
        self.communicator
            .invoke(
                crate::api::API_VFS_WRITE_RANGE,
                &(path.to_owned(), offset, data),
            )
            .await?
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_REMOVE_FILE, &path.to_owned())
            .await?;
        ret
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_REMOVE_DIR, &path.to_owned())
            .await?;
        ret
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_REMOVE_TREE, &path.to_owned())
            .await?;
        ret
    }

    async fn trash_item(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_TRASH_ITEM, &path.to_owned())
            .await?;
        ret
    }

    async fn resolve_link(&self, path: &Path) -> Result<PathBuf, Error> {
        self.communicator
            .invoke(crate::api::API_VFS_RESOLVE_LINK, &path.to_owned())
            .await?
    }

    async fn stream_path(&self, path: &Path, name: &str) -> Result<PathBuf, Error> {
        self.communicator
            .invoke(crate::api::API_VFS_STREAM_PATH, &(path.to_owned(), name))
            .await?
    }

    async fn read_order(&self, paths: &[PathBuf]) -> Result<Option<Vec<u64>>, Error> {
        self.communicator
            .invoke(crate::api::API_VFS_READ_ORDER, &paths)
            .await?
    }

    async fn list_recursive(
        &self,
        prefix: &Path,
        tx: mpsc::Sender<Vec<super::FlatEntry>>,
    ) -> Result<(), Error> {
        let entries: Result<Vec<super::FlatEntry>, Error> = self
            .communicator
            .invoke(crate::api::API_VFS_LIST_RECURSIVE, &prefix.to_owned())
            .await?;
        let _ = tx.send(entries?).await;
        Ok(())
    }

    async fn read_attribute(
        &self,
        path: &Path,
        kind: super::attributes::AttributeKind,
    ) -> Result<Option<super::attributes::Attribute>, Error> {
        self.communicator
            .invoke(crate::api::API_VFS_READ_ATTRIBUTE, &(path.to_owned(), kind))
            .await?
    }

    async fn write_attribute(
        &self,
        path: &Path,
        property: &super::attributes::Attribute,
    ) -> Result<(), Error> {
        self.communicator
            .invoke(
                crate::api::API_VFS_WRITE_ATTRIBUTE,
                &(path.to_owned(), property),
            )
            .await?
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let ret: Result<VfsMetadata, Error> = self
            .communicator
            .invoke(API_VFS_GET_METADATA, &path.to_owned())
            .await?;
        ret
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_SET_METADATA, &(path.to_owned(), meta.clone()))
            .await?;
        ret
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let ret: Result<VfsSpaceInfo, Error> = self
            .communicator
            .invoke(API_VFS_AVAILABLE_SPACE, &path.to_owned())
            .await?;
        ret
    }

    async fn same_file(&self, a: &Path, b: &Path) -> Result<bool, Error> {
        let ret: Result<bool, Error> = self
            .communicator
            .invoke(API_VFS_SAME_FILE, &(a.to_owned(), b.to_owned()))
            .await?;
        ret
    }

    async fn rename(
        &self,
        from: &Path,
        to: &Path,
        options: &super::attributes::RenameOptions,
    ) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                API_VFS_RENAME,
                &(from.to_owned(), to.to_owned(), options.clone()),
            )
            .await?;
        ret
    }

    async fn is_package(&self, path: &Path) -> Result<bool, Error> {
        let ret: Result<bool, Error> = self
            .communicator
            .invoke(crate::api::API_VFS_IS_PACKAGE, &path.to_owned())
            .await?;
        ret
    }

    async fn exchange(&self, a: &Path, b: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_VFS_EXCHANGE, &(a.to_owned(), b.to_owned()))
            .await?;
        ret
    }

    async fn copy_within(
        &self,
        from: &Path,
        to: &Path,
        options: &super::attributes::WriteOptions,
    ) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                API_VFS_COPY_WITHIN,
                &(from.to_owned(), to.to_owned(), options),
            )
            .await?;
        ret
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_HARD_LINK, &(link.to_owned(), target.to_owned()))
            .await?;
        ret
    }
}

/// Sends the data down one stream and reads the outcome off another.
/// Dropped before `finish`, it resets the data stream, and the other side
/// drops its writer uncommitted.
struct RemoteVfsWriter {
    data: StreamSender,
    result: StreamReceiver,
}

impl RemoteVfsWriter {
    /// Why the other side stopped taking data: it reports before it lets
    /// go of the stream.
    async fn failure(&mut self, error: Error) -> Error {
        match self.result.recv_item::<Result<(), Error>>().await {
            Some(Ok(Err(e))) => e,
            _ => error,
        }
    }
}

#[async_trait::async_trait]
impl VfsAsyncWriter for RemoteVfsWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        let n = buf.len().min(crate::rpc::CHUNK_SIZE);
        match self
            .data
            .send(bytes::Bytes::copy_from_slice(&buf[..n]))
            .await
        {
            Ok(()) => Ok(n),
            Err(e) => Err(self.failure(e).await),
        }
    }

    async fn finish(self: Box<Self>) -> Result<(), Error> {
        let Self { data, mut result } = *self;
        data.finish().await?;
        result
            .recv_item::<Result<(), Error>>()
            .await
            .expect("a remote write reports its outcome")?
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
