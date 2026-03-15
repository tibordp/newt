use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::File;
use crate::rpc::Communicator;

use super::{
    Breadcrumb, LOCAL_VFS_DESCRIPTOR, RegisteredDescriptor, Vfs, VfsDescriptor, VfsMetadata,
    VfsSpaceInfo,
};

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
        false
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
        LOCAL_VFS_DESCRIPTOR.format_path(path, mount_meta)
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        LOCAL_VFS_DESCRIPTOR.breadcrumbs(path, mount_meta)
    }

    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<PathBuf> {
        None
    }
}

pub static REMOTE_VFS_DESCRIPTOR: RemoteVfsDescriptor = RemoteVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&REMOTE_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// RemoteVfs — proxies Vfs calls back to the host over RPC
// ---------------------------------------------------------------------------

pub struct RemoteVfs {
    communicator: Communicator,
}

impl RemoteVfs {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

use crate::api::{
    API_HOST_VFS_AVAILABLE_SPACE, API_HOST_VFS_COPY_WITHIN, API_HOST_VFS_CREATE_DIRECTORY,
    API_HOST_VFS_CREATE_SYMLINK, API_HOST_VFS_FILE_DETAILS, API_HOST_VFS_FILE_INFO,
    API_HOST_VFS_FS_STATS, API_HOST_VFS_GET_METADATA, API_HOST_VFS_HARD_LINK,
    API_HOST_VFS_LIST_FILES, API_HOST_VFS_OPEN_READ_SYNC, API_HOST_VFS_OVERWRITE_SYNC,
    API_HOST_VFS_POLL_CHANGES, API_HOST_VFS_READ_RANGE, API_HOST_VFS_REMOVE_DIR,
    API_HOST_VFS_REMOVE_FILE, API_HOST_VFS_REMOVE_TREE, API_HOST_VFS_RENAME,
    API_HOST_VFS_SET_METADATA, API_HOST_VFS_TOUCH, API_HOST_VFS_TRUNCATE,
};

#[async_trait::async_trait]
impl Vfs for RemoteVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &REMOTE_VFS_DESCRIPTOR
    }

    async fn list_files(
        &self,
        path: &Path,
        _batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
        let ret: Result<Vec<File>, Error> = self
            .communicator
            .invoke(API_HOST_VFS_LIST_FILES, &path.to_path_buf())
            .await?;
        ret
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_POLL_CHANGES, &path.to_path_buf())
            .await?;
        ret
    }

    async fn fs_stats(&self, path: &Path) -> Result<Option<crate::filesystem::FsStats>, Error> {
        let ret: Result<Option<crate::filesystem::FsStats>, Error> = self
            .communicator
            .invoke(API_HOST_VFS_FS_STATS, &path.to_path_buf())
            .await?;
        ret
    }

    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let ret: Result<Vec<u8>, Error> = self
            .communicator
            .invoke(API_HOST_VFS_OPEN_READ_SYNC, &path.to_path_buf())
            .await?;
        Ok(Box::new(std::io::Cursor::new(ret?)))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let ret: Result<FileChunk, Error> = self
            .communicator
            .invoke(
                API_HOST_VFS_READ_RANGE,
                &(path.to_path_buf(), offset, length),
            )
            .await?;
        ret
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let ret: Result<FileDetails, Error> = self
            .communicator
            .invoke(API_HOST_VFS_FILE_DETAILS, &path.to_path_buf())
            .await?;
        ret
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let ret: Result<File, Error> = self
            .communicator
            .invoke(API_HOST_VFS_FILE_INFO, &path.to_path_buf())
            .await?;
        ret
    }

    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
        Ok(Box::new(ProxyWriter {
            path: path.to_path_buf(),
            communicator: self.communicator.clone(),
            buffer: Vec::new(),
        }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_CREATE_DIRECTORY, &path.to_path_buf())
            .await?;
        ret
    }

    async fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                API_HOST_VFS_CREATE_SYMLINK,
                &(link.to_path_buf(), target.to_path_buf()),
            )
            .await?;
        ret
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_TOUCH, &path.to_path_buf())
            .await?;
        ret
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_TRUNCATE, &path.to_path_buf())
            .await?;
        ret
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_REMOVE_FILE, &path.to_path_buf())
            .await?;
        ret
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_REMOVE_DIR, &path.to_path_buf())
            .await?;
        ret
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_REMOVE_TREE, &path.to_path_buf())
            .await?;
        ret
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let ret: Result<VfsMetadata, Error> = self
            .communicator
            .invoke(API_HOST_VFS_GET_METADATA, &path.to_path_buf())
            .await?;
        ret
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                API_HOST_VFS_SET_METADATA,
                &(path.to_path_buf(), meta.clone()),
            )
            .await?;
        ret
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let ret: Result<VfsSpaceInfo, Error> = self
            .communicator
            .invoke(API_HOST_VFS_AVAILABLE_SPACE, &path.to_path_buf())
            .await?;
        ret
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_RENAME, &(from.to_path_buf(), to.to_path_buf()))
            .await?;
        ret
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                API_HOST_VFS_COPY_WITHIN,
                &(from.to_path_buf(), to.to_path_buf()),
            )
            .await?;
        ret
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                API_HOST_VFS_HARD_LINK,
                &(link.to_path_buf(), target.to_path_buf()),
            )
            .await?;
        ret
    }
}

// ---------------------------------------------------------------------------
// ProxyWriter — buffers writes, sends all data on drop via blocking RPC call
// ---------------------------------------------------------------------------

struct ProxyWriter {
    path: PathBuf,
    communicator: Communicator,
    buffer: Vec<u8>,
}

impl Write for ProxyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for ProxyWriter {
    fn drop(&mut self) {
        let data = std::mem::take(&mut self.buffer);
        if data.is_empty() {
            return;
        }
        let communicator = self.communicator.clone();
        let path = self.path.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let _: Result<Result<(), Error>, _> = communicator
                    .invoke(API_HOST_VFS_OVERWRITE_SYNC, &(path, data))
                    .await;
            });
        });
    }
}
