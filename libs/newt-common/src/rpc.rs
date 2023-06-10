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
use log::{debug, error, info};
use parking_lot::Mutex;
use tokio::{
    io::{AsyncBufRead, AsyncRead, AsyncWrite},
    task::AbortHandle,
};
use tokio_stream::wrappers::UnboundedReceiverStream;
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
    Ping(bool),
    InvokeRequest(Api, RequestId, bytes::Bytes),
    InvokeResponse(RequestId, bytes::Bytes),
    InvokeCancel(RequestId),
    Notify(Api, bytes::Bytes),
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

        match src[0] {
            0 => {
                if src.len() < 1 + 1 {
                    return Ok(None);
                }
                let pong = src[1] != 0;
                src.advance(2);
                return Ok(Some(Message::Ping(pong)));
            }
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
                let slice = Bytes::copy_from_slice(&src[15..15 + len]);
                src.advance(15 + len);

                return Ok(Some(Message::InvokeRequest(
                    Api(api),
                    RequestId(request_id),
                    slice,
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
                let slice = Bytes::copy_from_slice(&src[13..13 + len]);
                src.advance(13 + len);

                return Ok(Some(Message::InvokeResponse(RequestId(request_id), slice)));
            }
            3 => {
                if src.len() < 1 + 8 {
                    return Ok(None);
                }
                let request_id = (&src[1..9]).read_u64::<NetworkEndian>().unwrap();
                src.advance(9);
                return Ok(Some(Message::InvokeCancel(RequestId(request_id))));
            }
            4 => {
                if src.len() < 1 + 2 {
                    return Ok(None);
                }
                let api = (&src[1..3]).read_u16::<NetworkEndian>().unwrap();
                let len = (&src[3..7]).read_u32::<NetworkEndian>().unwrap() as usize;

                if src.len() < 7 + len {
                    src.reserve(7 + len - src.len());
                    return Ok(None);
                }
                let slice = Bytes::copy_from_slice(&src[7..7 + len]);
                src.advance(7 + len);

                return Ok(Some(Message::Notify(Api(api), slice)));
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
            Message::Ping(response) => {
                dst.reserve(1 + 1);
                dst.put_u8(0);
                dst.put_u8(response as u8);
            }
            Message::InvokeRequest(api, request_id, data) => {
                dst.reserve(1 + 2 + 8 + 4 + data.len());
                dst.put_u8(1);
                dst.put_u16(api.0);
                dst.put_u64(request_id.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
            Message::InvokeResponse(request_id, data) => {
                dst.reserve(1 + 8 + 4 + data.len());
                dst.put_u8(2);
                dst.put_u64(request_id.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
            Message::InvokeCancel(request_id) => {
                dst.reserve(1 + 8);
                dst.put_u8(3);
                dst.put_u64(request_id.0);
            }
            Message::Notify(api, data) => {
                dst.reserve(1 + 2 + 4 + data.len());
                dst.put_u8(4);
                dst.put_u16(api.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
pub trait Dispatcher: Send + Sync {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error>;
    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error>;
}

pub struct Communicator {
    request_id: AtomicU64,
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
            let _ = tx.send(Message::InvokeCancel(self.1));
        }
    }
}

impl Communicator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            request_id: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            response: Mutex::new(HashMap::new()),
            dispatcher: None,
            outbox: Mutex::new(None),
        })
    }

    pub fn with_dispatcher<D: Dispatcher + 'static>(dispatcher: D) -> Arc<Self> {
        Arc::new(Self {
            request_id: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            response: Mutex::new(HashMap::new()),
            dispatcher: Some(Arc::new(dispatcher)),
            outbox: Mutex::new(None),
        })
    }

    pub async fn handle_connection<S: AsyncRead + AsyncWrite + Send + Unpin + 'static>(
        self: Arc<Self>,
        stream: S,
    ) -> Result<(), Error> {
        use futures::SinkExt;
        use futures::StreamExt;

        let (outbox, mut inbox) = tokio::sync::mpsc::unbounded_channel();
        *self.outbox.lock() = Some(outbox.downgrade());

        let (mut tx, mut rx) = Framed::new(stream, MessageCodec {}).split();

        let sender = {
            tokio::spawn(async move {
                while let Some(msg) = inbox.recv().await {
                    if let Err(e) = tx.send(msg).await {
                        return Err(e);
                    }
                }

                Ok(())
            })
        };

        let result = loop {
            match rx.next().await {
                Some(Ok(msg)) => {
                    match msg {
                        Message::Ping(response) => {
                            if !response {
                                let _ = outbox.send(Message::Ping(true));
                            } else {
                                info!("ping response received");
                            }
                        }
                        Message::InvokeRequest(api, id, payload) => {
                            let dispatcher = self.dispatcher.clone().expect(
                                "received a request message on a communicator without a dispatcher",
                            );
                            let outbox = outbox.clone();
                            self.tasks.lock().insert(
                                id,
                                tokio::spawn(async move {
                                    match dispatcher.invoke(api, payload).await {
                                        Ok(Some(resp)) => {
                                            let _ = outbox.send(Message::InvokeResponse(id, resp));
                                        }
                                        Ok(None) => {
                                            error!("unknown API invoked");
                                        }
                                        Err(e) => {
                                            error!("error handling request: {}", e);
                                        }
                                    }
                                })
                                .abort_handle(),
                            );
                        }
                        Message::InvokeResponse(id, payload) => {
                            if let Some(sender) = self.response.lock().remove(&id) {
                                let _ = sender.send(payload);
                            }
                        }
                        Message::Notify(api, payload) => {
                            let dispatcher = self.dispatcher.clone().expect(
                                "received a request message on a communicator without a dispatcher",
                            );
                            tokio::spawn(async move {
                                match dispatcher.notify(api, payload).await {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        error!("unknown API invoked");
                                    }
                                    Err(e) => {
                                        error!("error handling notification: {}", e)
                                    }
                                }
                            });
                        }
                        Message::InvokeCancel(id) => {
                            if let Some(task) = self.tasks.lock().remove(&id) {
                                task.abort();
                            }
                        }
                    }
                }
                Some(Err(e)) => {
                    break Err(e);
                }
                None => {
                    break Ok(());
                }
            }
        };

        sender.await??;
        result
    }

    pub async fn invoke<Req, Resp>(&self, api: Api, req: &Req) -> Result<Resp, Error>
    where
        Req: serde::Serialize + for<'de> serde::Deserialize<'de>,
        Resp: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let id = RequestId(self.request_id.fetch_add(1, Ordering::SeqCst));
        let bytes = bincode::serialize(req).unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.response.lock().insert(id, tx);
        let guard = CancelGuard(self, id);

        let message = Message::InvokeRequest(api, id, bytes.into());
        self.outbox
            .lock()
            .as_ref()
            .and_then(|s| s.upgrade())
            .ok_or_else(|| Error::Connection)?
            .send(message)
            .map_err(|_| Error::Connection)?;

        let resp = rx.await.map_err(|_| Error::Connection)?;

        std::mem::forget(guard);
        Ok(bincode::deserialize(&resp[..]).unwrap())
    }
}

pub struct RemoteFileSystem {
    communicator: Arc<Communicator>,
}

impl RemoteFileSystem {
    pub fn new(communicator: Arc<Communicator>) -> Self {
        Self { communicator }
    }
}

const API_POLL_CHANGES: Api = Api(0);
const API_LIST_FILES: Api = Api(1);
const API_RENAME: Api = Api(2);
const API_CREATE_DIRECTORY: Api = Api(3);
const API_DELETE_ALL: Api = Api(4);

#[async_trait::async_trait]
impl Filesystem for RemoteFileSystem {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self.communicator.invoke(API_POLL_CHANGES, &path).await?;

        Ok(ret?)
    }
    async fn list_files(&self, path: PathBuf) -> Result<FileList, Error> {
        let ret: Result<FileList, Error> = self.communicator.invoke(API_LIST_FILES, &path).await?;

        Ok(ret?)
    }
    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_RENAME, &(old_path, new_path))
            .await?;

        Ok(ret?)
    }
    async fn create_directory(&self, path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(API_CREATE_DIRECTORY, &path)
            .await?;

        Ok(ret?)
    }
    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error> {
        let ret: Result<(), Error> = self.communicator.invoke(API_DELETE_ALL, &paths).await?;

        Ok(ret?)
    }
}

pub struct FilesystemDispatcher {
    filesystem: Box<dyn Filesystem>,
}

impl FilesystemDispatcher {
    pub fn new<F: Filesystem + 'static>(filesystem: F) -> Self {
        Self {
            filesystem: Box::new(filesystem),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for FilesystemDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_POLL_CHANGES => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.poll_changes(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_LIST_FILES => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.list_files(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_RENAME => {
                let (old_path, new_path): (PathBuf, PathBuf) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.rename(old_path, new_path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_CREATE_DIRECTORY => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.create_directory(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_DELETE_ALL => {
                let paths: Vec<PathBuf> = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.delete_all(paths).await;

                bincode::serialize(&ret).unwrap()
            }
            _ => return Ok(None)
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        match api {
            _ => Ok(false),
        }
    }
}
