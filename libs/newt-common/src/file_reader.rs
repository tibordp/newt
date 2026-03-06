use std::path::PathBuf;

use crate::filesystem::{Mode, UserGroup};
use crate::rpc::Communicator;
use crate::vfs::VfsPath;
use crate::Error;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileDetails {
    pub size: u64,
    pub mime_type: Option<String>,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub symlink_target: Option<PathBuf>,
    pub user: Option<UserGroup>,
    pub group: Option<UserGroup>,
    pub mode: Option<Mode>,
    pub modified: Option<i128>,
    pub accessed: Option<i128>,
    pub created: Option<i128>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileChunk {
    pub data: Vec<u8>,
    pub offset: u64,
    pub total_size: u64,
}

#[async_trait::async_trait]
pub trait FileReader: Send + Sync {
    async fn file_details(&self, path: VfsPath) -> Result<FileDetails, Error>;
    async fn read_range(&self, path: VfsPath, offset: u64, length: u64)
        -> Result<FileChunk, Error>;
    async fn read_file(&self, path: VfsPath, max_size: u64) -> Result<Vec<u8>, Error>;
    async fn write_file(&self, path: VfsPath, data: Vec<u8>) -> Result<(), Error>;
}

pub struct Remote {
    communicator: Communicator,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl FileReader for Remote {
    async fn file_details(&self, path: VfsPath) -> Result<FileDetails, Error> {
        let ret: Result<FileDetails, Error> = self
            .communicator
            .invoke(crate::api::API_FILE_DETAILS, &path)
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

    async fn read_file(&self, path: VfsPath, max_size: u64) -> Result<Vec<u8>, Error> {
        let ret: Result<Vec<u8>, Error> = self
            .communicator
            .invoke(crate::api::API_READ_FILE, &(path, max_size))
            .await?;

        Ok(ret?)
    }

    async fn write_file(&self, path: VfsPath, data: Vec<u8>) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_WRITE_FILE, &(path, data))
            .await?;

        Ok(ret?)
    }
}
