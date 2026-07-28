//! `VfsDispatcher` and `VfsReadChunkDispatcher`: the API_VFS_* surface that
//! lets a `RemoteVfs` on one side of a connection drive a real VFS on the
//! other. `VfsDispatcher` serves the VFS; `VfsReadChunkDispatcher` runs on the
//! caller's side to route read-chunk notifications back into the stream.
//! Direction is symmetric — the caller may be the agent reaching the host's
//! VFS, or the host reaching a spawned sub-agent's VFS (an agent mount).
//!
//! `VfsDispatcher::invoke` handles request/response calls; chunk streams are
//! split across notifications:
//!   - reads: handler streams chunks via `API_VFS_READ_CHUNK` notifications,
//!     terminating with an empty payload as the EOF sentinel.
//!   - writes: caller sends `API_VFS_WRITE_CHUNK` notifications until an
//!     empty sentinel; `OVERWRITE_ASYNC_BEGIN` returns a fresh `StreamId`,
//!     `OVERWRITE_ASYNC_FINISH` awaits the writer task.
//!   - positioned reads: `OPEN_READ_AT` mints a server-held
//!     `VfsRandomReader` keyed by `StreamId`, `READ_AT` is plain
//!     request/response per chunk, and `READ_AT_CLOSE` (sent on proxy
//!     drop) reaps the handle.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use parking_lot::Mutex;

use super::{
    API_VFS_AVAILABLE_SPACE, API_VFS_COPY_WITHIN, API_VFS_CREATE_DIRECTORY, API_VFS_CREATE_SYMLINK,
    API_VFS_FILE_DETAILS, API_VFS_FILE_INFO, API_VFS_FS_STATS, API_VFS_GET_METADATA,
    API_VFS_HARD_LINK, API_VFS_LIST_FILES, API_VFS_OPEN_READ_ASYNC, API_VFS_OPEN_READ_AT,
    API_VFS_OVERWRITE_ASYNC_ABORT, API_VFS_OVERWRITE_ASYNC_BEGIN, API_VFS_OVERWRITE_ASYNC_FINISH,
    API_VFS_POLL_CHANGES, API_VFS_READ_AT, API_VFS_READ_AT_CLOSE, API_VFS_READ_CHUNK,
    API_VFS_READ_RANGE, API_VFS_REMOVE_DIR, API_VFS_REMOVE_FILE, API_VFS_REMOVE_TREE,
    API_VFS_RENAME, API_VFS_SAME_FILE, API_VFS_SET_METADATA, API_VFS_TOUCH, API_VFS_TRASH_ITEM,
    API_VFS_TRUNCATE, API_VFS_WRITE_CHUNK, decode, encode,
};
use crate::Error;
use crate::filesystem::StreamId;
use crate::rpc::{Api, Dispatcher, Outbox};
use crate::vfs::Vfs;

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

/// Caller-side state for one in-flight chunked read stream: sequenced
/// chunks are routed here by `VfsReadChunkDispatcher` until the empty
/// EOF sentinel.
pub struct ReadStream {
    pub tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub expected_seq: u64,
}

pub type PendingVfsReadStreams = Arc<parking_lot::Mutex<HashMap<StreamId, ReadStream>>>;

pub struct VfsDispatcher {
    vfs: Arc<dyn Vfs>,
    outbox: Outbox,
    write_sessions: PendingVfsWriteSessions,
    write_task_handles: WriteTaskHandles,
    read_at_sessions: super::ReadAtSessions,
    next_stream_id: AtomicU64,
}

impl VfsDispatcher {
    pub fn new(vfs: Arc<dyn Vfs>, outbox: Outbox) -> Self {
        Self {
            vfs,
            outbox,
            write_sessions: Arc::new(Mutex::new(HashMap::new())),
            write_task_handles: Arc::new(Mutex::new(HashMap::new())),
            read_at_sessions: super::ReadAtSessions::new(),
            next_stream_id: AtomicU64::new(1),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        use crate::vfs::path::PathBuf;

        let ret = match api {
            API_VFS_LIST_FILES => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.list_files(&path, None).await;
                encode(&ret)?
            }
            API_VFS_POLL_CHANGES => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.poll_changes(&path).await;
                encode(&ret)?
            }
            API_VFS_FS_STATS => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.fs_stats(&path).await;
                encode(&ret)?
            }
            API_VFS_OPEN_READ_ASYNC => {
                let (path, stream_id): (PathBuf, StreamId) = decode(&req[..])?;
                let outbox = self.outbox.clone();

                // Stream errors must land in `ret` — the encoded response is
                // the only way the remote reader learns the stream failed.
                let ret: Result<(), Error> = async {
                    let mut reader = self.vfs.open_read_async(&path).await?;
                    super::send_chunk_stream(
                        &outbox,
                        API_VFS_READ_CHUNK,
                        stream_id,
                        reader.as_mut(),
                        super::OnReadError::Abort,
                    )
                    .await
                }
                .await;

                encode(&ret)?
            }
            API_VFS_READ_RANGE => {
                let (path, offset, length): (PathBuf, u64, u64) = decode(&req[..])?;
                let ret = self.vfs.read_range(&path, offset, length).await;
                encode(&ret)?
            }
            API_VFS_OPEN_READ_AT => {
                let path: PathBuf = decode(&req[..])?;
                let ret: Result<StreamId, Error> = self
                    .vfs
                    .open_read_at(&path)
                    .await
                    .map(|reader| self.read_at_sessions.open(reader));
                encode(&ret)?
            }
            API_VFS_READ_AT => {
                let (stream_id, offset, len): (StreamId, u64, u64) = decode(&req[..])?;
                let ret = self.read_at_sessions.read_at(stream_id, offset, len).await;
                encode(&ret)?
            }
            API_VFS_FILE_DETAILS => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.file_details(&path).await;
                encode(&ret)?
            }
            API_VFS_FILE_INFO => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.file_info(&path).await;
                encode(&ret)?
            }
            API_VFS_OVERWRITE_ASYNC_BEGIN => {
                let path: PathBuf = decode(&req[..])?;

                let ret: Result<StreamId, Error> = match self.vfs.overwrite_async(&path).await {
                    Ok(writer) => {
                        let stream_id = StreamId(
                            self.next_stream_id
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                        );

                        let (chunk_tx, mut chunk_rx) =
                            tokio::sync::mpsc::channel::<WriteCommand>(4);
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
                    }
                    Err(e) => Err(e),
                };

                encode(&ret)?
            }
            API_VFS_OVERWRITE_ASYNC_FINISH => {
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
            API_VFS_CREATE_DIRECTORY => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.create_directory(&path).await;
                encode(&ret)?
            }
            API_VFS_CREATE_SYMLINK => {
                let (link, target): (PathBuf, String) = decode(&req[..])?;
                let ret = self.vfs.create_symlink(&link, &target).await;
                encode(&ret)?
            }
            API_VFS_TOUCH => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.touch(&path).await;
                encode(&ret)?
            }
            API_VFS_TRUNCATE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.truncate(&path).await;
                encode(&ret)?
            }
            API_VFS_REMOVE_FILE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_file(&path).await;
                encode(&ret)?
            }
            API_VFS_REMOVE_DIR => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_dir(&path).await;
                encode(&ret)?
            }
            API_VFS_REMOVE_TREE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_tree(&path).await;
                encode(&ret)?
            }
            API_VFS_TRASH_ITEM => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.trash_item(&path).await;
                encode(&ret)?
            }
            API_VFS_GET_METADATA => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.get_metadata(&path).await;
                encode(&ret)?
            }
            API_VFS_SET_METADATA => {
                let (path, meta): (PathBuf, crate::vfs::VfsMetadata) = decode(&req[..])?;
                let ret = self.vfs.set_metadata(&path, &meta).await;
                encode(&ret)?
            }
            API_VFS_AVAILABLE_SPACE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.available_space(&path).await;
                encode(&ret)?
            }
            API_VFS_SAME_FILE => {
                let (a, b): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.same_file(&a, &b).await;
                encode(&ret)?
            }
            API_VFS_RENAME => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.rename(&from, &to).await;
                encode(&ret)?
            }
            API_VFS_COPY_WITHIN => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.copy_within(&from, &to).await;
                encode(&ret)?
            }
            API_VFS_HARD_LINK => {
                let (link, target): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.hard_link(&link, &target).await;
                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == API_VFS_WRITE_CHUNK {
            let (stream_id, seq, data): (StreamId, u64, serde_bytes::ByteBuf) = decode(&req[..])?;

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
                            Some((session.tx.clone(), WriteCommand::Data(data.into_vec())))
                        }
                    }
                    None => None,
                }
            };
            if let Some((tx, command)) = command_tx {
                let _ = tx.send(command).await;
            }
            Ok(true)
        } else if api == API_VFS_OVERWRITE_ASYNC_ABORT {
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
        } else if api == API_VFS_READ_AT_CLOSE {
            let stream_id: StreamId = decode(&req[..])?;
            self.read_at_sessions.close(stream_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// VfsReadChunkDispatcher — caller-side: routes read-chunk notifications
// from the VFS-serving end into the correct RemoteVfs stream.
// ---------------------------------------------------------------------------

pub struct VfsReadChunkDispatcher {
    api: Api,
    pending_read_streams: PendingVfsReadStreams,
}

impl VfsReadChunkDispatcher {
    pub fn new(pending_read_streams: PendingVfsReadStreams) -> Self {
        Self::for_api(API_VFS_READ_CHUNK, pending_read_streams)
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
            let (stream_id, seq, data): (StreamId, u64, serde_bytes::ByteBuf) = decode(&req[..])?;

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
                let _ = tx.send(data.into_vec()).await;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::rpc::{Communicator, Dispatcher};
    use crate::test_support::mock_vfs::MockVfs;
    use crate::vfs::path::PathBuf;

    #[tokio::test]
    async fn abort_removes_write_session_and_handle() {
        let vfs = MockVfs::builder().build();
        let (outbox, _outbox_rx) = Communicator::create_outbox();
        let dispatcher = super::VfsDispatcher::new(vfs, outbox);

        let response = dispatcher
            .invoke(
                super::API_VFS_OVERWRITE_ASYNC_BEGIN,
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
                super::API_VFS_OVERWRITE_ASYNC_ABORT,
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
    async fn read_at_session_serves_reads_and_close_reaps_it() {
        let vfs = MockVfs::builder().file("/f", b"hello world").build();
        let (outbox, _outbox_rx) = Communicator::create_outbox();
        let dispatcher = super::VfsDispatcher::new(vfs, outbox);

        let response = dispatcher
            .invoke(
                super::API_VFS_OPEN_READ_AT,
                super::encode(&PathBuf::from_wire_str("/f")).unwrap().into(),
            )
            .await
            .unwrap()
            .unwrap();
        let stream_id: Result<crate::filesystem::StreamId, crate::Error> =
            super::decode(&response).unwrap();
        let stream_id = stream_id.unwrap();
        assert!(dispatcher.read_at_sessions.contains(stream_id));

        let response = dispatcher
            .invoke(
                super::API_VFS_READ_AT,
                super::encode(&(stream_id, 6u64, 5u64)).unwrap().into(),
            )
            .await
            .unwrap()
            .unwrap();
        let data: Result<serde_bytes::ByteBuf, crate::Error> = super::decode(&response).unwrap();
        assert_eq!(data.unwrap().as_slice(), b"world");

        dispatcher
            .notify(
                super::API_VFS_READ_AT_CLOSE,
                super::encode(&stream_id).unwrap().into(),
            )
            .await
            .unwrap();
        assert!(!dispatcher.read_at_sessions.contains(stream_id));
    }

    #[tokio::test]
    async fn read_stream_error_is_encoded_into_response() {
        use crate::test_support::mock_vfs::FailureSpec;

        let vfs = MockVfs::builder()
            .file("/f", b"content")
            .failure(FailureSpec {
                path: PathBuf::from_wire_str("/f"),
                operation: "open_read_async",
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
                super::API_VFS_OPEN_READ_ASYNC,
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
