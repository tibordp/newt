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
    API_HOST_VFS_LIST_FILES, API_HOST_VFS_OPEN_READ_ASYNC, API_HOST_VFS_OVERWRITE_ASYNC_BEGIN,
    API_HOST_VFS_OVERWRITE_ASYNC_FINISH, API_HOST_VFS_POLL_CHANGES, API_HOST_VFS_READ_CHUNK,
    API_HOST_VFS_READ_RANGE, API_HOST_VFS_REMOVE_DIR, API_HOST_VFS_REMOVE_FILE,
    API_HOST_VFS_REMOVE_TREE, API_HOST_VFS_RENAME, API_HOST_VFS_SET_METADATA, API_HOST_VFS_TOUCH,
    API_HOST_VFS_TRUNCATE, API_HOST_VFS_WRITE_CHUNK, PendingVfsReadStreams, decode, encode,
    try_encode,
};
use crate::Error;
use crate::filesystem::StreamId;
use crate::rpc::{Api, Dispatcher, Message, Outbox};
use crate::vfs::{VFS_READ_CHUNK_SIZE, Vfs};

struct WriteSession {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    expected_seq: u64,
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

                let ret: Result<(), Error> = if descriptor.can_read_async() {
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
                            let _ = outbox
                                .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                                .await;
                        }
                        seq += 1;
                    }
                    // Send empty sentinel to signal EOF.
                    if let Some(bytes) = try_encode(&(stream_id, seq, Vec::<u8>::new())) {
                        let _ = outbox
                            .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                            .await;
                    }
                    Ok(())
                } else if descriptor.can_read_sync() {
                    let mut reader = self.vfs.open_read_sync(&path).await?;
                    let outbox = outbox.clone();
                    tokio::task::spawn_blocking(move || {
                        use std::io::Read;
                        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
                        let mut seq: u64 = 0;
                        loop {
                            let n = reader.read(&mut buf)?;
                            if n == 0 {
                                break;
                            }
                            let chunk = buf[..n].to_vec();
                            if let Some(bytes) = try_encode(&(stream_id, seq, chunk)) {
                                let _ = outbox.blocking_send_low(Message::Notify(
                                    API_HOST_VFS_READ_CHUNK,
                                    bytes.into(),
                                ));
                            }
                            seq += 1;
                        }
                        // Send empty sentinel to signal EOF.
                        if let Some(bytes) = try_encode(&(stream_id, seq, Vec::<u8>::new())) {
                            let _ = outbox.blocking_send_low(Message::Notify(
                                API_HOST_VFS_READ_CHUNK,
                                bytes.into(),
                            ));
                        }
                        Ok::<(), Error>(())
                    })
                    .await?
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

                    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
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
                        let mut writer = writer;
                        while let Some(data) = chunk_rx.recv().await {
                            writer.write(&data).await?;
                        }
                        writer.finish().await?;
                        write_sessions.lock().remove(&stream_id);
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

                    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
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
                        let mut writer = writer;
                        while let Some(data) =
                            tokio::runtime::Handle::current().block_on(chunk_rx.recv())
                        {
                            writer.write_all(&data)?;
                        }
                        drop(writer); // flushes
                        write_sessions.lock().remove(&stream_id);
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

            let tx = {
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
                            // Sentinel — remove session to close the channel.
                            sessions.remove(&stream_id);
                            None
                        } else {
                            Some(session.tx.clone())
                        }
                    }
                    None => None,
                }
            };
            if let Some(tx) = tx {
                let _ = tx.send(data).await;
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
