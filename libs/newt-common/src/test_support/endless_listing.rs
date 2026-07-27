use crate::Error;
use crate::filesystem::{FileList, Filesystem, ListFilesOptions};
use crate::vfs::{VfsId, VfsPath};

/// `Filesystem` stub whose streaming listing never terminates: it pumps
/// empty batches from a blocking task until the receiver is dropped.
/// Exercises cancellation paths — `started`/`stopped` let the test
/// synchronize on the producer's lifecycle.
pub struct EndlessListing {
    pub started: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub stopped: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[async_trait::async_trait]
impl Filesystem for EndlessListing {
    async fn poll_changes(&self, _path: VfsPath) -> Result<(), Error> {
        Ok(())
    }

    async fn list_files(
        &self,
        path: VfsPath,
        _options: ListFilesOptions,
        batch_tx: Option<tokio::sync::mpsc::Sender<FileList>>,
    ) -> Result<FileList, Error> {
        let tx = batch_tx.expect("streaming listing must provide a sender");
        let stopped = self.stopped.lock().take();
        tokio::task::spawn_blocking(move || {
            loop {
                if tx
                    .blocking_send(FileList::new(path.clone(), Vec::new(), None))
                    .is_err()
                {
                    if let Some(stopped) = stopped {
                        let _ = stopped.send(());
                    }
                    return;
                }
            }
        });
        if let Some(started) = self.started.lock().take() {
            let _ = started.send(());
        }
        std::future::pending().await
    }

    async fn touch(&self, _path: VfsPath) -> Result<(), Error> {
        Ok(())
    }

    async fn create_directory(&self, _path: VfsPath) -> Result<(), Error> {
        Ok(())
    }

    async fn fs_stats(&self, _path: VfsPath) -> Result<Option<crate::filesystem::FsStats>, Error> {
        Ok(None)
    }

    async fn revalidate(&self, _vfs_id: VfsId) -> Result<crate::vfs::RevalidationOutcome, Error> {
        Ok(crate::vfs::RevalidationOutcome::Fresh)
    }

    async fn file_details(&self, _path: VfsPath) -> Result<crate::file_reader::FileDetails, Error> {
        Err(Error::not_supported())
    }

    async fn get_property_sheet(
        &self,
        _path: VfsPath,
    ) -> Result<crate::vfs::properties::PropertySheet, Error> {
        Err(Error::not_supported())
    }

    async fn read_range(
        &self,
        _path: VfsPath,
        _offset: u64,
        _length: u64,
    ) -> Result<crate::file_reader::FileChunk, Error> {
        Err(Error::not_supported())
    }

    async fn open_read_at(
        &self,
        _path: VfsPath,
    ) -> Result<Box<dyn crate::vfs::VfsRandomReader>, Error> {
        Err(Error::not_supported())
    }

    async fn read_file(&self, _path: VfsPath, _max_size: u64) -> Result<Vec<u8>, Error> {
        Err(Error::not_supported())
    }

    async fn write_file(&self, _path: VfsPath, _data: Vec<u8>) -> Result<(), Error> {
        Err(Error::not_supported())
    }

    async fn find_in_file(
        &self,
        _path: VfsPath,
        _offset: u64,
        _pattern: crate::file_reader::SearchPattern,
        _max_length: u64,
    ) -> Result<Option<crate::file_reader::SearchMatch>, Error> {
        Err(Error::not_supported())
    }
}
