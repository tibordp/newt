//! `VfsDispatcher` (host-side) and `VfsReadChunkDispatcher` (agent-side):
//! the API_HOST_VFS_* surface that lets a `RemoteVfs` running on the agent
//! drive a real VFS on the host.
//!
//! `VfsDispatcher::invoke` handles request/response calls; chunk streams are
//! split across notifications:
//!   - reads: handler streams chunks via `API_HOST_VFS_READ_CHUNK` notifications,
//!     terminating with an empty payload as the EOF sentinel.
//!   - writes: agent sends `API_HOST_VFS_WRITE_CHUNK` notifications until an
//!     empty sentinel; `OVERWRITE_ASYNC_BEGIN` returns a fresh `StreamId`,
//!     `OVERWRITE_ASYNC_FINISH` awaits the writer task.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use parking_lot::Mutex;

use super::{
    API_HOST_VFS_AVAILABLE_SPACE, API_HOST_VFS_COPY_WITHIN, API_HOST_VFS_CREATE_DIRECTORY,
    API_HOST_VFS_CREATE_SYMLINK, API_HOST_VFS_FILE_DETAILS, API_HOST_VFS_FILE_INFO,
    API_HOST_VFS_FS_STATS, API_HOST_VFS_GET_METADATA, API_HOST_VFS_HARD_LINK,
    API_HOST_VFS_LIST_FILES, API_HOST_VFS_OPEN_READ_ASYNC, API_HOST_VFS_OVERWRITE_ASYNC_ABORT,
    API_HOST_VFS_OVERWRITE_ASYNC_BEGIN, API_HOST_VFS_OVERWRITE_ASYNC_FINISH,
    API_HOST_VFS_POLL_CHANGES, API_HOST_VFS_READ_CHUNK, API_HOST_VFS_READ_RANGE,
    API_HOST_VFS_REMOVE_DIR, API_HOST_VFS_REMOVE_FILE, API_HOST_VFS_REMOVE_TREE,
    API_HOST_VFS_RENAME, API_HOST_VFS_SET_METADATA, API_HOST_VFS_TOUCH, API_HOST_VFS_TRASH_ITEM,
    API_HOST_VFS_TRUNCATE, API_HOST_VFS_WRITE_CHUNK, PendingVfsReadStreams, decode, encode,
    try_encode,
};
use crate::Error;
use crate::filesystem::StreamId;
use crate::rpc::{Api, Dispatcher, Message, Outbox};
use crate::vfs::{VFS_READ_CHUNK_SIZE, Vfs};

struct WriteSession {
    tx: tokio::sync::mpsc::Sender<WriteCommand>,
    expected_seq: u64,
}

enum WriteCommand {
    Data(Vec<u8>),
    Finish,
}

struct WriteSessionCleanup {
    stream_id: StreamId,
    sessions: PendingVfsWriteSessions,
}

impl Drop for WriteSessionCleanup {
    fn drop(&mut self) {
        self.sessions.lock().remove(&self.stream_id);
    }
}

type PendingVfsWriteSessions = Arc<Mutex<HashMap<StreamId, WriteSession>>>;

/// Shared state for write sessions, accessible from both invoke and notify
/// handlers. The JoinHandle map lets the FINISH invoke await the writer task.
type WriteTaskHandles = Arc<Mutex<HashMap<StreamId, tokio::task::JoinHandle<Result<(), Error>>>>>;

pub struct VfsDispatcher {
    vfs: Arc<dyn Vfs>,
    outbox: Outbox,
    write_sessions: PendingVfsWriteSessions,
    write_task_handles: WriteTaskHandles,
    next_stream_id: AtomicU64,
}

impl VfsDispatcher {
    pub fn new(vfs: Arc<dyn Vfs>, outbox: Outbox) -> Self {
        Self {
            vfs,
            outbox,
            write_sessions: Arc::new(Mutex::new(HashMap::new())),
            write_task_handles: Arc::new(Mutex::new(HashMap::new())),
            next_stream_id: AtomicU64::new(1),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        use crate::vfs::path::PathBuf;

        let ret = match api {
            API_HOST_VFS_LIST_FILES => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.list_files(&path, None).await;
                encode(&ret)?
            }
            API_HOST_VFS_POLL_CHANGES => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.poll_changes(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_FS_STATS => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.fs_stats(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_OPEN_READ_ASYNC => {
                let (path, stream_id): (PathBuf, StreamId) = decode(&req[..])?;
                let descriptor = self.vfs.descriptor();
                let outbox = self.outbox.clone();

                // Stream errors must land in `ret` — the encoded response is
                // the only way the remote reader learns the stream failed.
                let ret: Result<(), Error> = if descriptor.can_read_async() {
                    async {
                        use tokio::io::AsyncReadExt;
                        let mut reader = self.vfs.open_read_async(&path).await?;
                        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
                        let mut seq: u64 = 0;
                        loop {
                            let n = reader.read(&mut buf).await.map_err(Error::from)?;
                            if n == 0 {
                                break;
                            }
                            let chunk = buf[..n].to_vec();
                            if let Some(bytes) = try_encode(&(stream_id, seq, chunk)) {
                                outbox
                                    .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                                    .await
                                    .map_err(|_| Error::connection())?;
                            }
                            seq += 1;
                        }
                        // Send empty sentinel to signal EOF.
                        if let Some(bytes) = try_encode(&(stream_id, seq, Vec::<u8>::new())) {
                            outbox
                                .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                                .await
                                .map_err(|_| Error::connection())?;
                        }
                        Ok(())
                    }
                    .await
                } else if descriptor.can_read_sync() {
                    async {
                        let reader = self.vfs.open_read_sync(&path).await?;
                        // Receiver drop is the cancellation path; the token is
                        // never cancelled here.
                        let (mut chunks, read_task) = crate::operation::bridge_sync_reader(
                            reader,
                            tokio_util::sync::CancellationToken::new(),
                        );
                        let mut seq: u64 = 0;
                        while let Some(chunk) = chunks.recv().await {
                            if let Some(bytes) = try_encode(&(stream_id, seq, chunk?)) {
                                outbox
                                    .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                                    .await
                                    .map_err(|_| Error::connection())?;
                            }
                            seq += 1;
                        }
                        read_task.await?;
                        if let Some(bytes) = try_encode(&(stream_id, seq, Vec::<u8>::new())) {
                            outbox
                                .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                                .await
                                .map_err(|_| Error::connection())?;
                        }
                        Ok(())
                    }
                    .await
                } else {
                    Err(Error::not_supported())
                };

                encode(&ret)?
            }
            API_HOST_VFS_READ_RANGE => {
                let (path, offset, length): (PathBuf, u64, u64) = decode(&req[..])?;
                let ret = self.vfs.read_range(&path, offset, length).await;
                encode(&ret)?
            }
            API_HOST_VFS_FILE_DETAILS => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.file_details(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_FILE_INFO => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.file_info(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_OVERWRITE_ASYNC_BEGIN => {
                let path: PathBuf = decode(&req[..])?;
                let descriptor = self.vfs.descriptor();

                let ret: Result<StreamId, Error> = if descriptor.can_overwrite_async() {
                    let writer = self.vfs.overwrite_async(&path).await?;
                    let stream_id = StreamId(
                        self.next_stream_id
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                    );

                    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<WriteCommand>(4);
                    self.write_sessions.lock().insert(
                        stream_id,
                        WriteSession {
                            tx: chunk_tx,
                            expected_seq: 0,
                        },
                    );

                    let write_task_handles = self.write_task_handles.clone();
                    let write_sessions = self.write_sessions.clone();
                    let handle = tokio::spawn(async move {
                        let _cleanup = WriteSessionCleanup {
                            stream_id,
                            sessions: write_sessions,
                        };
                        let mut writer = writer;
                        while let Some(command) = chunk_rx.recv().await {
                            match command {
                                WriteCommand::Data(data) => {
                                    writer.write(&data).await?;
                                }
                                WriteCommand::Finish => return writer.finish().await,
                            }
                        }
                        // Sender disappearance without Finish is cancellation:
                        // drop the writer without committing it.
                        Ok(())
                    });
                    write_task_handles.lock().insert(stream_id, handle);

                    Ok(stream_id)
                } else if descriptor.can_overwrite_sync() {
                    let writer = self.vfs.overwrite_sync(&path).await?;
                    let stream_id = StreamId(
                        self.next_stream_id
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                    );

                    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<WriteCommand>(4);
                    self.write_sessions.lock().insert(
                        stream_id,
                        WriteSession {
                            tx: chunk_tx,
                            expected_seq: 0,
                        },
                    );

                    let write_task_handles = self.write_task_handles.clone();
                    let write_sessions = self.write_sessions.clone();
                    let handle = tokio::task::spawn_blocking(move || {
                        use std::io::Write;
                        let _cleanup = WriteSessionCleanup {
                            stream_id,
                            sessions: write_sessions,
                        };
                        let mut writer = writer;
                        while let Some(command) =
                            tokio::runtime::Handle::current().block_on(chunk_rx.recv())
                        {
                            match command {
                                WriteCommand::Data(data) => writer.write_all(&data)?,
                                WriteCommand::Finish => {
                                    writer.flush()?;
                                    return Ok(());
                                }
                            }
                        }
                        // Cancellation closes the channel. Dropping the file is
                        // sufficient; do not turn an abort into a successful finish.
                        Ok(())
                    });
                    write_task_handles.lock().insert(stream_id, handle);

                    Ok(stream_id)
                } else {
                    Err(Error::not_supported())
                };

                encode(&ret)?
            }
            API_HOST_VFS_OVERWRITE_ASYNC_FINISH => {
                let stream_id: StreamId = decode(&req[..])?;
                // The sentinel (empty chunk) already closed the data channel.
                // Wait for the writer task to finish and propagate its result.
                let handle = self.write_task_handles.lock().remove(&stream_id);
                let ret: Result<(), Error> = match handle {
                    Some(h) => match h.await {
                        Ok(r) => r,
                        Err(e) => Err(Error::custom(format!("writer task failed: {}", e))),
                    },
                    None => {
                        // Writer task already finished or was never started.
                        Ok(())
                    }
                };
                encode(&ret)?
            }
            API_HOST_VFS_CREATE_DIRECTORY => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.create_directory(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_CREATE_SYMLINK => {
                let (link, target): (PathBuf, String) = decode(&req[..])?;
                let ret = self.vfs.create_symlink(&link, &target).await;
                encode(&ret)?
            }
            API_HOST_VFS_TOUCH => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.touch(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_TRUNCATE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.truncate(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_REMOVE_FILE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_file(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_REMOVE_DIR => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_dir(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_REMOVE_TREE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_tree(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_TRASH_ITEM => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.trash_item(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_GET_METADATA => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.get_metadata(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_SET_METADATA => {
                let (path, meta): (PathBuf, crate::vfs::VfsMetadata) = decode(&req[..])?;
                let ret = self.vfs.set_metadata(&path, &meta).await;
                encode(&ret)?
            }
            API_HOST_VFS_AVAILABLE_SPACE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.available_space(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_RENAME => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.rename(&from, &to).await;
                encode(&ret)?
            }
            API_HOST_VFS_COPY_WITHIN => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.copy_within(&from, &to).await;
                encode(&ret)?
            }
            API_HOST_VFS_HARD_LINK => {
                let (link, target): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.hard_link(&link, &target).await;
                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == API_HOST_VFS_WRITE_CHUNK {
            let (stream_id, seq, data): (StreamId, u64, Vec<u8>) = decode(&req[..])?;

            let command_tx = {
                let mut sessions = self.write_sessions.lock();
                let session = sessions.get_mut(&stream_id);
                match session {
                    Some(session) => {
                        assert!(
                            seq == session.expected_seq,
                            "VFS write chunk out of order for stream {:?}: expected seq {}, got {}",
                            stream_id,
                            session.expected_seq,
                            seq,
                        );
                        session.expected_seq += 1;

                        if data.is_empty() {
                            // Remove the map-owned sender, but retain this clone
                            // long enough to deliver the explicit Finish command.
                            sessions
                                .remove(&stream_id)
                                .map(|session| (session.tx, WriteCommand::Finish))
                        } else {
                            Some((session.tx.clone(), WriteCommand::Data(data)))
                        }
                    }
                    None => None,
                }
            };
            if let Some((tx, command)) = command_tx {
                let _ = tx.send(command).await;
            }
            Ok(true)
        } else if api == API_HOST_VFS_OVERWRITE_ASYNC_ABORT {
            let stream_id: StreamId = decode(&req[..])?;
            // Removing the last sender wakes a running sync writer. Aborting
            // also stops an async writer and prevents a queued spawn_blocking
            // closure from starting. Notification dispatch is ordered, so a
            // backpressured chunk handler must return before ABORT is handled.
            self.write_sessions.lock().remove(&stream_id);
            if let Some(handle) = self.write_task_handles.lock().remove(&stream_id) {
                handle.abort();
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// VfsReadChunkDispatcher — agent-side: routes read-chunk notifications
// from the host into the correct RemoteVfs stream.
// ---------------------------------------------------------------------------

pub struct VfsReadChunkDispatcher {
    api: Api,
    pending_read_streams: PendingVfsReadStreams,
}

impl VfsReadChunkDispatcher {
    pub fn new(pending_read_streams: PendingVfsReadStreams) -> Self {
        Self::for_api(API_HOST_VFS_READ_CHUNK, pending_read_streams)
    }

    /// The same sequenced-chunk routing for another notification verb
    /// (e.g. `API_HOST_FETCH_AGENT_CHUNK`), with its own stream map.
    pub fn for_api(api: Api, pending_read_streams: PendingVfsReadStreams) -> Self {
        Self {
            api,
            pending_read_streams,
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsReadChunkDispatcher {
    async fn invoke(&self, _api: Api, _req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        Ok(None)
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == self.api {
            let (stream_id, seq, data): (StreamId, u64, Vec<u8>) = decode(&req[..])?;

            let tx = {
                let mut streams = self.pending_read_streams.lock();
                let stream = streams.get_mut(&stream_id);
                match stream {
                    Some(stream) => {
                        assert!(
                            seq == stream.expected_seq,
                            "VFS read chunk out of order for stream {:?}: expected seq {}, got {}",
                            stream_id,
                            stream.expected_seq,
                            seq,
                        );
                        stream.expected_seq += 1;
                        let tx = stream.tx.clone();

                        if data.is_empty() {
                            // Sentinel — remove from map so the channel closes
                            // after this send (the tx clone is the last sender).
                            streams.remove(&stream_id);
                        }
                        Some(tx)
                    }
                    None => None,
                }
            };
            if let Some(tx) = tx {
                // Send the chunk (or empty sentinel) — the reader distinguishes.
                let _ = tx.send(data).await;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::rpc::{Communicator, Dispatcher};
    use crate::test_support::mock_vfs::{MockVfs, MockVfsConfig};
    use crate::vfs::path::PathBuf;

    struct EndlessReader {
        reads: Arc<AtomicUsize>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Read for EndlessReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            buf.fill(1);
            Ok(buf.len())
        }
    }

    impl Drop for EndlessReader {
        fn drop(&mut self) {
            if let Some(tx) = self.dropped.take() {
                let _ = tx.send(());
            }
        }
    }

    #[tokio::test]
    async fn sync_reader_stops_when_async_consumer_is_dropped() {
        let reads = Arc::new(AtomicUsize::new(0));
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let reader = Box::new(EndlessReader {
            reads: reads.clone(),
            dropped: Some(dropped_tx),
        });

        let (mut chunks, read_task) = crate::operation::bridge_sync_reader(
            reader,
            tokio_util::sync::CancellationToken::new(),
        );
        chunks.recv().await.unwrap().unwrap();
        drop(chunks);

        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("blocking reader did not stop when its consumer was dropped")
            .unwrap();
        read_task.await.unwrap();

        // Four buffered chunks, the one received above, and at most one send
        // racing with receiver drop.
        assert!(reads.load(Ordering::SeqCst) <= 6);
    }

    #[tokio::test]
    async fn abort_removes_sync_write_session_and_handle() {
        let vfs = MockVfs::builder()
            .config(MockVfsConfig {
                can_overwrite_sync: true,
                can_overwrite_async: false,
                ..Default::default()
            })
            .build();
        let (outbox, _outbox_rx) = Communicator::create_outbox();
        let dispatcher = super::VfsDispatcher::new(vfs, outbox);

        let response = dispatcher
            .invoke(
                super::API_HOST_VFS_OVERWRITE_ASYNC_BEGIN,
                super::encode(&PathBuf::from_wire_str("/partial"))
                    .unwrap()
                    .into(),
            )
            .await
            .unwrap()
            .unwrap();
        let stream_id: Result<crate::filesystem::StreamId, crate::Error> =
            super::decode(&response).unwrap();
        let stream_id = stream_id.unwrap();
        assert!(dispatcher.write_sessions.lock().contains_key(&stream_id));
        assert!(
            dispatcher
                .write_task_handles
                .lock()
                .contains_key(&stream_id)
        );

        dispatcher
            .notify(
                super::API_HOST_VFS_OVERWRITE_ASYNC_ABORT,
                super::encode(&stream_id).unwrap().into(),
            )
            .await
            .unwrap();

        assert!(!dispatcher.write_sessions.lock().contains_key(&stream_id));
        assert!(
            !dispatcher
                .write_task_handles
                .lock()
                .contains_key(&stream_id)
        );
    }

    #[tokio::test]
    async fn read_stream_error_is_encoded_into_response() {
        use crate::test_support::mock_vfs::FailureSpec;

        let vfs = MockVfs::builder()
            .file("/f", b"content")
            .failure(FailureSpec {
                path: PathBuf::from_wire_str("/f"),
                operation: "open_read_sync",
                error: crate::Error::custom("simulated open failure"),
                remaining: None,
            })
            .build();
        let (outbox, _outbox_rx) = Communicator::create_outbox();
        let dispatcher = super::VfsDispatcher::new(vfs, outbox);

        // The error must come back as an encoded response — a bare Err from
        // invoke() produces no InvokeResponse and hangs the remote reader.
        let response = dispatcher
            .invoke(
                super::API_HOST_VFS_OPEN_READ_ASYNC,
                super::encode(&(PathBuf::from_wire_str("/f"), crate::filesystem::StreamId(7)))
                    .unwrap()
                    .into(),
            )
            .await
            .expect("stream errors must not escape the invoke")
            .unwrap();
        let ret: Result<(), crate::Error> = super::decode(&response).unwrap();
        assert!(
            ret.unwrap_err().message.contains("simulated open failure"),
            "response must carry the stream error"
        );
    }
}
