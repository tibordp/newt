use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Weak,
    },
    thread::JoinHandle,
};

use bytes::{Buf, BufMut, Bytes};
use log::error;
use parking_lot::Mutex;
use tokio::{
    io::{AsyncBufRead, AsyncRead, AsyncWrite},
    task::AbortHandle,
};
use tokio_util::codec::Framed;

use crate::{
    filesystem::{FileList, Filesystem},
    Error,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Api(u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RequestId(u64);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Message {
    Request(Api, RequestId, bytes::Bytes),
    Response(RequestId, bytes::Bytes),
    Cancel(RequestId),
}

struct MessageCodec {}

impl tokio_util::codec::Decoder for MessageCodec {
    type Item = Message;
    type Error = Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        use byteorder::{NetworkEndian, ReadBytesExt};

        if src.len() < 1 {
            return Ok(None);
        }

        let kind = src[0];
        match src[0] {
            1 => {
                if src.len() < 15 {
                    return Ok(None);
                }
                let api = (&src[1..3]).read_u16::<NetworkEndian>().unwrap();
                let request_id = (&src[3..11]).read_u64::<NetworkEndian>().unwrap();
                let len = (&src[11..15]).read_u32::<NetworkEndian>().unwrap() as usize;

                if src.len() < 15 + len {
                    src.reserve(15 + len - src.len());
                    return Ok(None);
                }

                return Ok(Some(Message::Request(
                    Api(api),
                    RequestId(request_id),
                    Bytes::copy_from_slice(&src[15..15 + len]),
                )));
            }
            2 => {
                if src.len() < 1 + 8 + 4 {
                    return Ok(None);
                }
                let request_id = (&src[1..9]).read_u64::<NetworkEndian>().unwrap();
                let len = (&src[9..13]).read_u32::<NetworkEndian>().unwrap() as usize;

                if src.len() < 13 + len {
                    src.reserve(13 + len - src.len());
                    return Ok(None);
                }

                return Ok(Some(Message::Response(
                    RequestId(request_id),
                    Bytes::copy_from_slice(&src[13..13 + len]),
                )));
            }
            3 => {
                if src.len() < 1 + 8 {
                    return Ok(None);
                }
                let request_id = (&src[1..9]).read_u64::<NetworkEndian>().unwrap();
                return Ok(Some(Message::Cancel(RequestId(request_id))));
            }
            _ => {
                return Err(Error::Custom("invalid message kind".to_string()));
            }
        }
    }
}

impl tokio_util::codec::Encoder<Message> for MessageCodec {
    type Error = Error;

    fn encode(&mut self, item: Message, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        match item {
            Message::Request(api, request_id, data) => {
                dst.reserve(1 + 2 + 8 + 4 + data.len());
                dst.put_u8(1);
                dst.put_u16(api.0);
                dst.put_u64(request_id.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
            Message::Response(request_id, data) => {
                dst.reserve(1 + 8 + 4 + data.len());
                dst.put_u8(2);
                dst.put_u64(request_id.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
            Message::Cancel(request_id) => {
                dst.reserve(1 + 8);
                dst.put_u8(3);
                dst.put_u64(request_id.0);
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
pub trait Dispatcher: Send + Sync {
    async fn dispatch(&self, api: Api, req: bytes::Bytes) -> Result<bytes::Bytes, Error>;
}

pub struct Communicator {
    id: AtomicU64,
    tasks: Mutex<HashMap<RequestId, AbortHandle>>,
    response: Mutex<HashMap<RequestId, tokio::sync::oneshot::Sender<bytes::Bytes>>>,
    dispatcher: Option<Arc<dyn Dispatcher>>,
    outbox: Mutex<Option<tokio::sync::mpsc::WeakUnboundedSender<Message>>>,
}

pub struct CancelGuard<'a>(&'a Communicator, RequestId);
impl<'a> Drop for CancelGuard<'a> {
    fn drop(&mut self) {
        self.0.response.lock().remove(&self.1);
        if let Some(tx) = self.0.outbox.lock().as_ref().and_then(|s| s.upgrade()) {
            let _ = tx.send(Message::Cancel(self.1));
        }
    }
}

impl Communicator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            id: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            response: Mutex::new(HashMap::new()),
            dispatcher: None,
            outbox: Mutex::new(None),
        })
    }

    pub fn with_dispatcher<D: Dispatcher + 'static>(dispatcher: D) -> Arc<Self> {
        Arc::new(Self {
            id: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            response: Mutex::new(HashMap::new()),
            dispatcher: Some(Arc::new(dispatcher)),
            outbox: Mutex::new(None),
        })
    }

    pub async fn process<S: AsyncRead + AsyncWrite + Unpin>(
        self: Arc<Self>,
        stream: S,
    ) -> Result<(), Error> {
        use futures::SinkExt;
        use tokio_stream::StreamExt;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        *self.outbox.lock() = Some(tx.downgrade());
        let mut stream = Framed::new(stream, MessageCodec {});

        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    stream.send(msg).await?;
                }
                Some(result) = stream.next() => match result {
                    Ok(msg) => {
                        match msg {
                            Message::Request(api, id, payload) => {
                                let dispatcher = self.dispatcher.clone().expect(
                                    "received a request message on a communicator without a dispatcher",
                                );
                                let outbox = tx.clone();
                                self.tasks.lock().insert(
                                    id,
                                    tokio::spawn(async move {
                                        match dispatcher.dispatch(api, payload).await {
                                            Ok(resp) => {
                                                let _ = outbox.send(Message::Response(id, resp));
                                            }
                                            Err(_) => {
                                                error!("error dispatching request")
                                            }
                                        }
                                    })
                                    .abort_handle(),
                                );
                            }
                            Message::Response(id, payload) => {
                                if let Some(sender) = self.response.lock().remove(&id) {
                                    let _ = sender.send(payload);
                                }
                            }
                            Message::Cancel(id) => {
                                if let Some(task) = self.tasks.lock().remove(&id) {
                                    task.abort();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        return Err(e);
                    }
                },
                else => {
                    break;
                }
            }
        }

        Ok(())
    }

    pub async fn invoke<Req, Resp>(&self, api: Api, req: &Req) -> Result<Resp, Error>
    where
        Req: serde::Serialize + for<'de> serde::Deserialize<'de>,
        Resp: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let id = RequestId(self.id.fetch_add(0, Ordering::SeqCst));
        let mut bytes: Vec<u8> = Vec::new();
        ciborium::into_writer(req, &mut bytes).unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.response.lock().insert(id, tx);
        let guard = CancelGuard(self, id);

        self.outbox
            .lock()
            .as_ref()
            .and_then(|s| s.upgrade())
            .ok_or_else(|| Error::Custom("communicator is not connected".into()))?
            .send(Message::Request(api, id, bytes.into()))
            .map_err(|_| Error::Custom("could not send".into()))?;

        let resp = rx
            .await
            .map_err(|_| Error::Custom("could not receive".into()))?;

        std::mem::forget(guard);
        Ok(ciborium::from_reader(std::io::Cursor::new(resp)).unwrap())
    }
}

struct RemoteFileSystem {
    communicator: Arc<Communicator>,
}

const API_POLL_CHANGES: Api = Api(0);
const API_LIST_FILES: Api = Api(1);
const API_RENAME: Api = Api(2);
const API_CREATE_DIRECTORY: Api = Api(3);
const API_DELETE_ALL: Api = Api(4);

#[async_trait::async_trait]
impl Filesystem for RemoteFileSystem {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_POLL_CHANGES, &path)
            .await
            .map_err(|_| Error::Connection)?;

        Ok(ret?)
    }
    async fn list_files(&self, path: PathBuf) -> Result<FileList, Error> {
        let ret: Result<FileList, Error> = self
            .communicator
            .invoke(API_LIST_FILES, &path)
            .await
            .map_err(|_| Error::Connection)?;

        Ok(ret?)
    }
    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_RENAME, &(old_path, new_path))
            .await
            .map_err(|_| Error::Connection)?;

        Ok(ret?)
    }
    async fn create_directory(&self, path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_CREATE_DIRECTORY, &path)
            .await
            .map_err(|_| Error::Connection)?;

        Ok(ret?)
    }
    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_DELETE_ALL, &paths)
            .await
            .map_err(|_| Error::Connection)?;

        Ok(ret?)
    }
}

pub struct FilesystemDispatcher {
    filesystem: Box<dyn Filesystem>,
}

impl FilesystemDispatcher {
    pub fn new<F: Filesystem + 'static>(filesystem: F) -> Self {
        Self { filesystem: Box::new(filesystem) }
    }
}

#[async_trait::async_trait]
impl Dispatcher for FilesystemDispatcher {
    async fn dispatch(&self, api: Api, req: bytes::Bytes) -> Result<bytes::Bytes, Error> {
        let mut result = Vec::new();

        match api {
            API_POLL_CHANGES => {
                let path: PathBuf = ciborium::from_reader(std::io::Cursor::new(req)).unwrap();
                let ret = self.filesystem.poll_changes(path).await;

                ciborium::into_writer(&ret, &mut result)
            }
            API_LIST_FILES => {
                let path: PathBuf = ciborium::from_reader(std::io::Cursor::new(req)).unwrap();
                let ret = self.filesystem.list_files(path).await;

                ciborium::into_writer(&ret, &mut result)
            }
            API_RENAME => {
                let (old_path, new_path): (PathBuf, PathBuf) =
                    ciborium::from_reader(std::io::Cursor::new(req)).unwrap();
                let ret = self.filesystem.rename(old_path, new_path).await;

                ciborium::into_writer(&ret, &mut result)
            }
            API_CREATE_DIRECTORY => {
                let path: PathBuf = ciborium::from_reader(std::io::Cursor::new(req)).unwrap();
                let ret = self.filesystem.create_directory(path).await;

                ciborium::into_writer(&ret, &mut result)
            }
            API_DELETE_ALL => {
                let paths: Vec<PathBuf> = ciborium::from_reader(std::io::Cursor::new(req)).unwrap();
                let ret = self.filesystem.delete_all(paths).await;

                ciborium::into_writer(&ret, &mut result)
            }
            _ => return Err(Error::Custom("unknown api".into())),
        }
        .map_err(|_| Error::Custom("could not serialize".into()))?;

        Ok(result.into())
    }
}
