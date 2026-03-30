use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Poll};

use tokio::sync::mpsc;

use crate::Error;
use crate::api::PendingVfsReadStreams;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, StreamId};
use crate::rpc::Communicator;

use super::{
    Breadcrumb, DisplayPathMatch, LOCAL_VFS_DESCRIPTOR, RegisteredDescriptor, Vfs, VfsAsyncWriter,
    VfsDescriptor, VfsMetadata, VfsSpaceInfo,
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
        false
    }
    fn can_read_async(&self) -> bool {
        true
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
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

    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        None
    }
}

pub static REMOTE_VFS_DESCRIPTOR: RemoteVfsDescriptor = RemoteVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&REMOTE_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// RemoteVfs — proxies Vfs calls back to the host over RPC
// ---------------------------------------------------------------------------

/// Channel capacity for read-chunk streams between the notification dispatcher
/// and the AsyncRead consumer.
const READ_STREAM_CHANNEL_CAPACITY: usize = 4;

pub struct RemoteVfs {
    communicator: Communicator,
    pending_read_streams: PendingVfsReadStreams,
    next_stream_id: AtomicU64,
}

impl RemoteVfs {
    pub fn new(communicator: Communicator, pending_read_streams: PendingVfsReadStreams) -> Self {
        Self {
            communicator,
            pending_read_streams,
            next_stream_id: AtomicU64::new(1),
        }
    }

    fn next_stream_id(&self) -> StreamId {
        StreamId(
            self.next_stream_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        )
    }
}

use crate::api::{
    API_HOST_VFS_AVAILABLE_SPACE, API_HOST_VFS_COPY_WITHIN, API_HOST_VFS_CREATE_DIRECTORY,
    API_HOST_VFS_CREATE_SYMLINK, API_HOST_VFS_FILE_DETAILS, API_HOST_VFS_FILE_INFO,
    API_HOST_VFS_FS_STATS, API_HOST_VFS_GET_METADATA, API_HOST_VFS_HARD_LINK,
    API_HOST_VFS_LIST_FILES, API_HOST_VFS_OPEN_READ_ASYNC, API_HOST_VFS_OVERWRITE_ASYNC_BEGIN,
    API_HOST_VFS_OVERWRITE_ASYNC_FINISH, API_HOST_VFS_POLL_CHANGES, API_HOST_VFS_READ_RANGE,
    API_HOST_VFS_REMOVE_DIR, API_HOST_VFS_REMOVE_FILE, API_HOST_VFS_REMOVE_TREE,
    API_HOST_VFS_RENAME, API_HOST_VFS_SET_METADATA, API_HOST_VFS_TOUCH, API_HOST_VFS_TRUNCATE,
    API_HOST_VFS_WRITE_CHUNK,
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

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>, Error> {
        let stream_id = self.next_stream_id();
        let (chunk_tx, chunk_rx) = mpsc::channel::<Vec<u8>>(READ_STREAM_CHANNEL_CAPACITY);

        // Register so the VfsReadChunkDispatcher can route notifications to us.
        self.pending_read_streams.lock().insert(
            stream_id,
            crate::api::ReadStream {
                tx: chunk_tx,
                expected_seq: 0,
            },
        );

        // RAII guard to clean up the stream on cancellation/error.
        let guard = StreamGuard {
            stream_id,
            pending: self.pending_read_streams.clone(),
        };

        // The invoke blocks until the host has sent all chunks (including the
        // empty sentinel that signals EOF), then returns Ok(()) or an error.
        // The sentinel is delivered through the data channel, so the reader sees
        // EOF independently of when the invoke completes.
        let communicator = self.communicator.clone();
        let path = path.to_path_buf();
        let invoke_handle = tokio::spawn(async move {
            let ret: Result<Result<(), Error>, _> = communicator
                .invoke(API_HOST_VFS_OPEN_READ_ASYNC, &(path, stream_id))
                .await;
            ret
        });

        Ok(Box::new(ChannelAsyncRead {
            rx: chunk_rx,
            current_chunk: Vec::new(),
            chunk_offset: 0,
            _invoke_handle: invoke_handle,
            _guard: guard,
        }))
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

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let stream_id: Result<StreamId, Error> = self
            .communicator
            .invoke(API_HOST_VFS_OVERWRITE_ASYNC_BEGIN, &path.to_path_buf())
            .await?;
        let stream_id = stream_id?;

        Ok(Box::new(RemoteVfsWriter {
            stream_id,
            communicator: self.communicator.clone(),
            next_seq: 0,
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
// ChannelAsyncRead — turns a stream of sequenced Vec<u8> chunks into AsyncRead.
//
// Chunks carry (stream_id, seq, data). An empty `data` is the EOF sentinel.
// Sequence numbers are validated — an out-of-order chunk is a protocol error.
// ---------------------------------------------------------------------------

struct ChannelAsyncRead {
    rx: mpsc::Receiver<Vec<u8>>,
    current_chunk: Vec<u8>,
    chunk_offset: usize,
    /// Keeps the invoke task alive; its result is unused — EOF is signaled
    /// in-band via the empty sentinel chunk.
    _invoke_handle: tokio::task::JoinHandle<Result<Result<(), Error>, Error>>,
    /// Removes the stream from the pending map on drop (cancellation safety).
    _guard: StreamGuard,
}

struct StreamGuard {
    stream_id: StreamId,
    pending: PendingVfsReadStreams,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.pending.lock().remove(&self.stream_id);
    }
}

impl tokio::io::AsyncRead for ChannelAsyncRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Serve from the current buffered chunk first.
        if self.chunk_offset < self.current_chunk.len() {
            let remaining = &self.current_chunk[self.chunk_offset..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.chunk_offset += n;
            return Poll::Ready(Ok(()));
        }

        // Try to receive the next chunk.
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => {
                if chunk.is_empty() {
                    // Empty sentinel — EOF.
                    Poll::Ready(Ok(()))
                } else {
                    let n = chunk.len().min(buf.remaining());
                    buf.put_slice(&chunk[..n]);
                    if n < chunk.len() {
                        self.current_chunk = chunk;
                        self.chunk_offset = n;
                    } else {
                        self.current_chunk = Vec::new();
                        self.chunk_offset = 0;
                    }
                    Poll::Ready(Ok(()))
                }
            }
            Poll::Ready(None) => {
                // Channel closed unexpectedly (e.g. connection dropped).
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// RemoteVfsWriter — streams sequenced write chunks as notifications,
// sends sentinel + finish invoke to complete.
// ---------------------------------------------------------------------------

struct RemoteVfsWriter {
    stream_id: StreamId,
    communicator: Communicator,
    next_seq: u64,
}

#[async_trait::async_trait]
impl VfsAsyncWriter for RemoteVfsWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let data = buf.to_vec();
        self.communicator
            .notify(API_HOST_VFS_WRITE_CHUNK, &(self.stream_id, seq, data))
            .await?;
        Ok(buf.len())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), Error> {
        // Send empty sentinel to signal end-of-stream. This goes through the
        // same notify path (low priority) as data chunks, so it is ordered
        // after all preceding chunks regardless of outbox scheduling.
        let seq = self.next_seq;
        self.communicator
            .notify(
                API_HOST_VFS_WRITE_CHUNK,
                &(self.stream_id, seq, Vec::<u8>::new()),
            )
            .await?;

        // The finish invoke waits for the host-side writer task to complete
        // and returns any write errors. It does not participate in stream
        // shutdown — the sentinel does that.
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_HOST_VFS_OVERWRITE_ASYNC_FINISH, &self.stream_id)
            .await?;
        ret
    }
}
