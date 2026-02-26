use crate::rpc::Communicator;
use crate::vfs::VfsPath;
use crate::Error;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileDetails {
    pub size: u64,
    pub is_binary: bool,
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
}
