use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Poll};

use tokio::sync::mpsc;

use crate::Error;
use crate::api::PendingVfsReadStreams;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, StreamId};
use crate::rpc::Communicator;
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
        super::local::local_display_path(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        super::local::local_breadcrumbs(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn navigable_parent(&self, path: &Path, mount_meta: &[u8]) -> Option<PathBuf> {
        super::local::navigable_parent(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn roots(&self, mount_meta: &[u8]) -> Vec<super::RootInfo> {
        // A Windows client's FS exposed into a remote session is
        // split-root; `has_unified_root`/`initial_path` then land on its
        // first drive instead of the unlistable `/`.
        super::local::roots_from_meta(mount_meta)
    }
    fn has_unified_root(&self, mount_meta: &[u8]) -> bool {
        // Style-based, not root-count (a single-drive Windows client is
        // still split-root). See `unified_root_from_meta`.
        super::local::unified_root_from_meta(mount_meta)
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
            super::local::local_path_from_typed_display(input)?,
        ))
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
    descriptor: &'static dyn VfsDescriptor,
    mount_meta: Vec<u8>,
    /// Keeps a spawned sub-agent (process, askpass listener) alive for the
    /// lifetime of the mount. `None` for the host-communicator flavor.
    _connection: Option<super::agent::AgentConnectionGuard>,
}

impl RemoteVfs {
    /// Build a `RemoteVfs` from a `MountRequest::Remote`. Pulls the host
    /// communicator (set up at session start by the agent) and the
    /// shared pending-stream map out of the mount context.
    pub fn mount(
        ctx: &crate::api::MountContext<'_>,
    ) -> Result<std::sync::Arc<dyn super::Vfs>, crate::Error> {
        let communicator = ctx
            .host_communicator
            .get()
            .ok_or_else(|| crate::Error::custom("host communicator not available"))?
            .clone();
        Ok(std::sync::Arc::new(Self::new(
            communicator,
            ctx.pending_read_streams.clone(),
        )))
    }

    pub fn new(communicator: Communicator, pending_read_streams: PendingVfsReadStreams) -> Self {
        Self {
            communicator,
            pending_read_streams,
            next_stream_id: AtomicU64::new(1),
            descriptor: &REMOTE_VFS_DESCRIPTOR,
            mount_meta: Vec::new(),
            _connection: None,
        }
    }

    /// The same proxy pointed at a spawned FS-only sub-agent instead of the
    /// host: an agent mount. Owns the connection so unmount tears it down.
    pub fn for_agent(
        communicator: Communicator,
        pending_read_streams: PendingVfsReadStreams,
        mount_meta: Vec<u8>,
        connection: super::agent::AgentConnectionGuard,
    ) -> Self {
        Self {
            communicator,
            pending_read_streams,
            next_stream_id: AtomicU64::new(1),
            descriptor: &super::agent::AGENT_VFS_DESCRIPTOR,
            mount_meta,
            _connection: Some(connection),
        }
    }

    fn next_stream_id(&self) -> StreamId {
        StreamId(
            self.next_stream_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        )
    }
}

use crate::api::{
    API_VFS_AVAILABLE_SPACE, API_VFS_COPY_WITHIN, API_VFS_CREATE_DIRECTORY, API_VFS_CREATE_SYMLINK,
    API_VFS_FILE_DETAILS, API_VFS_FILE_INFO, API_VFS_FS_STATS, API_VFS_GET_METADATA,
    API_VFS_HARD_LINK, API_VFS_LIST_FILES, API_VFS_OPEN_READ_ASYNC, API_VFS_OVERWRITE_ASYNC_ABORT,
    API_VFS_OVERWRITE_ASYNC_BEGIN, API_VFS_OVERWRITE_ASYNC_FINISH, API_VFS_POLL_CHANGES,
    API_VFS_READ_RANGE, API_VFS_REMOVE_DIR, API_VFS_REMOVE_FILE, API_VFS_REMOVE_TREE,
    API_VFS_RENAME, API_VFS_SET_METADATA, API_VFS_TOUCH, API_VFS_TRASH_ITEM, API_VFS_TRUNCATE,
    API_VFS_WRITE_CHUNK,
};

#[async_trait::async_trait]
impl Vfs for RemoteVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        self.descriptor
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.mount_meta.clone()
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

    async fn fs_stats(&self, path: &Path) -> Result<Option<crate::filesystem::FsStats>, Error> {
        let ret: Result<Option<crate::filesystem::FsStats>, Error> = self
            .communicator
            .invoke(API_VFS_FS_STATS, &path.to_owned())
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
        let path = path.to_owned();
        let invoke_handle = tokio::spawn(async move {
            let ret: Result<Result<(), Error>, _> = communicator
                .invoke(API_VFS_OPEN_READ_ASYNC, &(path, stream_id))
                .await;
            ret
        });

        Ok(Box::new(ChannelAsyncRead {
            rx: chunk_rx,
            current_chunk: Vec::new(),
            chunk_offset: 0,
            invoke_handle: Some(invoke_handle),
            _guard: guard,
        }))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let ret: Result<FileChunk, Error> = self
            .communicator
            .invoke(API_VFS_READ_RANGE, &(path.to_owned(), offset, length))
            .await?;
        ret
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let ret: Result<FileDetails, Error> = self
            .communicator
            .invoke(API_VFS_FILE_DETAILS, &path.to_owned())
            .await?;
        ret
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let ret: Result<File, Error> = self
            .communicator
            .invoke(API_VFS_FILE_INFO, &path.to_owned())
            .await?;
        ret
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let stream_id: Result<StreamId, Error> = self
            .communicator
            .invoke(API_VFS_OVERWRITE_ASYNC_BEGIN, &path.to_owned())
            .await?;
        let stream_id = stream_id?;

        Ok(Box::new(RemoteVfsWriter {
            stream_id,
            communicator: self.communicator.clone(),
            next_seq: 0,
            active: true,
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

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_RENAME, &(from.to_owned(), to.to_owned()))
            .await?;
        ret
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_VFS_COPY_WITHIN, &(from.to_owned(), to.to_owned()))
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
    /// EOF is signaled in-band; aborting this task on drop cancels the remote
    /// producer when the consumer stops before EOF. `None` once the result has
    /// been consumed (a JoinHandle must not be polled after completion).
    invoke_handle: Option<tokio::task::JoinHandle<Result<Result<(), Error>, Error>>>,
    /// Removes the stream from the pending map on drop (cancellation safety).
    _guard: StreamGuard,
}

impl Drop for ChannelAsyncRead {
    fn drop(&mut self) {
        if let Some(handle) = &self.invoke_handle {
            handle.abort();
        }
    }
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

        loop {
            // Try to receive the next chunk.
            let recv = self.rx.poll_recv(cx);
            if let Poll::Ready(Some(chunk)) = recv {
                if chunk.is_empty() {
                    // Empty sentinel — EOF.
                    return Poll::Ready(Ok(()));
                }
                let n = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.current_chunk = chunk;
                    self.chunk_offset = n;
                } else {
                    self.current_chunk = Vec::new();
                    self.chunk_offset = 0;
                }
                return Poll::Ready(Ok(()));
            }

            // No chunk available. Surface a resolved invoke error instead of
            // waiting forever on a stream that will never send its sentinel.
            let Some(handle) = self.invoke_handle.as_mut() else {
                return match recv {
                    // Invoke succeeded and the channel is drained — EOF.
                    Poll::Ready(None) => Poll::Ready(Ok(())),
                    _ => Poll::Pending,
                };
            };
            match Pin::new(handle).poll(cx) {
                Poll::Ready(result) => {
                    self.invoke_handle = None;
                    let ret = match result {
                        Ok(Ok(ret)) => ret,
                        Ok(Err(e)) => Err(e),
                        Err(e) => Err(Error::custom(format!("read stream task failed: {e}"))),
                    };
                    if let Err(e) = ret {
                        return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                    }
                    // The success response is ordered after every chunk, so
                    // the sentinel is already buffered — re-poll the channel.
                }
                Poll::Pending => return Poll::Pending,
            }
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
    active: bool,
}

impl Drop for RemoteVfsWriter {
    fn drop(&mut self) {
        if self.active {
            let _ = self
                .communicator
                .signal(API_VFS_OVERWRITE_ASYNC_ABORT, &self.stream_id);
        }
    }
}

#[async_trait::async_trait]
impl VfsAsyncWriter for RemoteVfsWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.communicator
            .notify(
                API_VFS_WRITE_CHUNK,
                &(self.stream_id, seq, serde_bytes::Bytes::new(buf)),
            )
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
                API_VFS_WRITE_CHUNK,
                &(self.stream_id, seq, serde_bytes::Bytes::new(&[])),
            )
            .await?;

        // The finish invoke waits for the host-side writer task to complete
        // and returns any write errors. It does not participate in stream
        // shutdown — the sentinel does that.
        match self
            .communicator
            .invoke(API_VFS_OVERWRITE_ASYNC_FINISH, &self.stream_id)
            .await
        {
            Ok(ret) => {
                self.active = false;
                ret
            }
            // Keep active armed: dropping self on this return sends ABORT and
            // reaps the host handle if FINISH did not complete.
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AbortCapture(parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<StreamId>>>);

    struct FinishCapture {
        abort: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<StreamId>>>,
        finish_started: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    #[async_trait::async_trait]
    impl crate::rpc::Dispatcher for FinishCapture {
        async fn invoke(
            &self,
            api: crate::rpc::Api,
            _req: bytes::Bytes,
        ) -> Result<Option<bytes::Bytes>, Error> {
            if api == API_VFS_OVERWRITE_ASYNC_FINISH {
                if let Some(tx) = self.finish_started.lock().take() {
                    let _ = tx.send(());
                }
                std::future::pending().await
            } else {
                Ok(None)
            }
        }

        async fn notify(&self, api: crate::rpc::Api, req: bytes::Bytes) -> Result<bool, Error> {
            if api == API_VFS_OVERWRITE_ASYNC_ABORT {
                let stream_id: StreamId = bincode::deserialize(&req).unwrap();
                if let Some(tx) = self.abort.lock().take() {
                    let _ = tx.send(stream_id);
                }
                Ok(true)
            } else if api == API_VFS_WRITE_CHUNK {
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::rpc::Dispatcher for AbortCapture {
        async fn invoke(
            &self,
            _api: crate::rpc::Api,
            _req: bytes::Bytes,
        ) -> Result<Option<bytes::Bytes>, Error> {
            Ok(None)
        }

        async fn notify(&self, api: crate::rpc::Api, req: bytes::Bytes) -> Result<bool, Error> {
            if api == API_VFS_OVERWRITE_ASYNC_ABORT {
                let stream_id: StreamId = bincode::deserialize(&req).unwrap();
                if let Some(tx) = self.0.lock().take() {
                    let _ = tx.send(stream_id);
                }
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    #[tokio::test]
    async fn dropping_channel_reader_aborts_invoke_task() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let invoke_handle = tokio::spawn(async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<Result<Result<(), Error>, Error>>().await
        });
        started_rx.await.unwrap();

        let (_chunk_tx, chunk_rx) = mpsc::channel(1);
        let reader = ChannelAsyncRead {
            rx: chunk_rx,
            current_chunk: Vec::new(),
            chunk_offset: 0,
            invoke_handle: Some(invoke_handle),
            _guard: StreamGuard {
                stream_id: StreamId(1),
                pending: Default::default(),
            },
        };
        drop(reader);

        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("invoke task survived reader drop")
            .unwrap();
    }

    #[tokio::test]
    async fn read_stream_error_surfaces_instead_of_hanging() {
        let (chunk_tx, chunk_rx) = mpsc::channel(4);
        chunk_tx.send(b"data".to_vec()).await.unwrap();
        // `chunk_tx` stays alive (mimicking the pending-streams map entry) so
        // the channel never closes; only the invoke result carries the failure.
        let invoke_handle = tokio::spawn(std::future::ready(Ok::<_, Error>(Err(Error::custom(
            "simulated read failure",
        )))));

        let mut reader = ChannelAsyncRead {
            rx: chunk_rx,
            current_chunk: Vec::new(),
            chunk_offset: 0,
            invoke_handle: Some(invoke_handle),
            _guard: StreamGuard {
                stream_id: StreamId(1),
                pending: Default::default(),
            },
        };

        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"data");

        let err = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read(&mut buf))
            .await
            .expect("reader hung instead of surfacing the stream error")
            .unwrap_err();
        assert!(err.to_string().contains("simulated read failure"), "{err}");
    }

    #[tokio::test]
    async fn read_stream_sentinel_still_signals_eof() {
        let (chunk_tx, chunk_rx) = mpsc::channel(4);
        chunk_tx.send(b"data".to_vec()).await.unwrap();
        chunk_tx.send(Vec::new()).await.unwrap();
        // Invoke never resolves — EOF must come from the in-band sentinel.
        let invoke_handle = tokio::spawn(async move {
            let _tx = chunk_tx;
            std::future::pending::<Result<Result<(), Error>, Error>>().await
        });

        let mut reader = ChannelAsyncRead {
            rx: chunk_rx,
            current_chunk: Vec::new(),
            chunk_offset: 0,
            invoke_handle: Some(invoke_handle),
            _guard: StreamGuard {
                stream_id: StreamId(1),
                pending: Default::default(),
            },
        };

        use tokio::io::AsyncReadExt;
        let mut out = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reader.read_to_end(&mut out),
        )
        .await
        .expect("sentinel EOF did not terminate the read")
        .unwrap();
        assert_eq!(out, b"data");
    }

    #[tokio::test]
    async fn dropping_remote_writer_signals_abort() {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let client = crate::rpc::Communicator::new(client_stream);
        let (abort_tx, abort_rx) = tokio::sync::oneshot::channel();
        let _server = crate::rpc::Communicator::with_dispatcher(
            AbortCapture(parking_lot::Mutex::new(Some(abort_tx))),
            server_stream,
        );

        drop(RemoteVfsWriter {
            stream_id: StreamId(42),
            communicator: client,
            next_seq: 0,
            active: true,
        });

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), abort_rx)
                .await
                .expect("abort signal was not delivered")
                .unwrap(),
            StreamId(42)
        );
    }

    #[tokio::test]
    async fn cancelling_finish_still_signals_abort() {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let client = crate::rpc::Communicator::new(client_stream);
        let (abort_tx, abort_rx) = tokio::sync::oneshot::channel();
        let (finish_started_tx, finish_started_rx) = tokio::sync::oneshot::channel();
        let _server = crate::rpc::Communicator::with_dispatcher(
            FinishCapture {
                abort: parking_lot::Mutex::new(Some(abort_tx)),
                finish_started: parking_lot::Mutex::new(Some(finish_started_tx)),
            },
            server_stream,
        );

        let finish = tokio::spawn(
            Box::new(RemoteVfsWriter {
                stream_id: StreamId(43),
                communicator: client,
                next_seq: 0,
                active: true,
            })
            .finish(),
        );
        finish_started_rx.await.unwrap();
        finish.abort();

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), abort_rx)
                .await
                .expect("finish cancellation did not send abort")
                .unwrap(),
            StreamId(43)
        );
    }
}
