use std::path::PathBuf;

use crate::rpc::Communicator;
use crate::Error;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileInfo {
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
    async fn file_info(&self, path: PathBuf) -> Result<FileInfo, Error>;
    async fn read_range(&self, path: PathBuf, offset: u64, length: u64)
        -> Result<FileChunk, Error>;
}

pub struct Local;

impl Local {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Local {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FileReader for Local {
    async fn file_info(&self, path: PathBuf) -> Result<FileInfo, Error> {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;

            let file = std::fs::File::open(&path)?;
            let metadata = file.metadata()?;
            let size = metadata.len();

            // Detect binary by checking for null bytes in first 8KB
            let mut buf = vec![0u8; 8192.min(size as usize)];
            let mut reader = std::io::BufReader::new(file);
            let n = reader.read(&mut buf)?;
            let is_binary = buf[..n].contains(&0);

            Ok(FileInfo { size, is_binary })
        })
        .await?
    }

    async fn read_range(
        &self,
        path: PathBuf,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, Error> {
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
    async fn file_info(&self, path: PathBuf) -> Result<FileInfo, Error> {
        let ret: Result<FileInfo, Error> = self
            .communicator
            .invoke(crate::api::API_FILE_INFO, &path)
            .await?;

        Ok(ret?)
    }

    async fn read_range(
        &self,
        path: PathBuf,
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
