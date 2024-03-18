use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use bytes::{Buf, BufMut, Bytes};
use log::{error, info};
use parking_lot::Mutex;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    task::AbortHandle,
};

use tokio_util::codec::Framed;

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Api(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RequestId(pub u64);

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

        if src.is_empty() {
            return Ok(None);
        }

        match src[0] {
            0 => {
                if src.len() < 1 + 1 {
                    return Ok(None);
                }
                let pong = src[1] != 0;
                src.advance(2);

                Ok(Some(Message::Ping(pong)))
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

                Ok(Some(Message::InvokeRequest(
                    Api(api),
                    RequestId(request_id),
                    slice,
                )))
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

                Ok(Some(Message::InvokeResponse(RequestId(request_id), slice)))
            }
            3 => {
                if src.len() < 1 + 8 {
                    return Ok(None);
                }
                let request_id = (&src[1..9]).read_u64::<NetworkEndian>().unwrap();
                src.advance(9);
                Ok(Some(Message::InvokeCancel(RequestId(request_id))))
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

                Ok(Some(Message::Notify(Api(api), slice)))
            }
            _ => Err(Error::Custom("invalid message kind".to_string())),
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
pub trait Dispatcher: Send + Sync + 'static {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error>;
    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error>;
}

pub struct ChainDispatcher<T1, T2>(T1, T2);

#[async_trait::async_trait]
impl<T1: Dispatcher, T2: Dispatcher> Dispatcher for ChainDispatcher<T1, T2> {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        if let Some(res) = self.0.invoke(api, req.clone()).await? {
            Ok(Some(res))
        } else {
            self.1.invoke(api, req).await
        }
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if self.0.notify(api, req.clone()).await? {
            Ok(true)
        } else {
            self.1.notify(api, req).await
        }
    }
}

pub trait DispatcherExt: Dispatcher + Sized {
    fn chain<Other: Dispatcher>(self, next: Other) -> ChainDispatcher<Self, Other> {
        ChainDispatcher(self, next)
    }
}

impl<T: Dispatcher> DispatcherExt for T {}

struct NullDispatcher;

#[async_trait::async_trait]
impl Dispatcher for NullDispatcher {
    async fn invoke(&self, _api: Api, _req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        Ok(None)
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}

/// Attempt to cancel a remote invocation when the guard is dropped.
pub struct CancelGuard<'a>(&'a CommunicatorInner, RequestId);
impl<'a> Drop for CancelGuard<'a> {
    fn drop(&mut self) {
        self.0.response.lock().remove(&self.1);
        let _ = self.0.outbox.send(Message::InvokeCancel(self.1));
    }
}

pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Stream for T {}

struct CommunicatorInner {
    request_id: AtomicU64,
    tasks: Mutex<HashMap<RequestId, AbortHandle>>,
    response: Mutex<HashMap<RequestId, tokio::sync::oneshot::Sender<bytes::Bytes>>>,
    dispatcher: Arc<dyn Dispatcher>,
    outbox: tokio::sync::mpsc::UnboundedSender<Message>,
}

impl CommunicatorInner {
    async fn handle_connection<S: Stream>(
        self: Arc<Self>,
        stream: S,
        mut inbox: tokio::sync::mpsc::UnboundedReceiver<Message>,
    ) -> Result<(), Error> {
        use futures::SinkExt;
        use futures::StreamExt;

        let (mut tx, mut rx) = Framed::new(stream, MessageCodec {}).split();

        let sender = {
            tokio::spawn(async move {
                while let Some(msg) = inbox.recv().await {
                    tx.feed(msg).await?;
                    // Since this is an interactive RPC, we flush after every message
                    tx.flush().await?;
                }
                info!("outbox closed");

                Ok::<(), Error>(())
            })
        };

        let result = loop {
            match rx.next().await {
                Some(Ok(msg)) => match msg {
                    Message::Ping(response) => {
                        if !response {
                            let _ = self.outbox.send(Message::Ping(true));
                        } else {
                            info!("ping response received");
                        }
                    }
                    Message::InvokeRequest(api, id, payload) => {
                        let dispatcher = self.dispatcher.clone();
                        let outbox = self.outbox.clone();
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
                        let dispatcher = self.dispatcher.clone();
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
                },
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
}

#[derive(Clone)]
pub struct Communicator(Arc<CommunicatorInner>);

impl Communicator {
    pub fn new<S: Stream>(stream: S) -> Self {
        Self::with_dispatcher(NullDispatcher, stream)
    }

    pub fn with_dispatcher<D: Dispatcher, S: Stream>(dispatcher: D, stream: S) -> Self {
        let (outbox, inbox) = tokio::sync::mpsc::unbounded_channel();

        let ret = Arc::new(CommunicatorInner {
            request_id: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            response: Mutex::new(HashMap::new()),
            dispatcher: Arc::new(dispatcher),
            outbox: outbox.clone(),
        });

        let inner = ret.clone();
        tokio::spawn(async move {
            match inner.clone().handle_connection(stream, inbox).await {
                Ok(()) => {
                    info!("connection closed");
                }
                Err(e) => {
                    error!("connection error: {}", e);
                }
            }

            // Cancel all pending tasks
            for (_, handle) in inner.tasks.lock().drain() {
                handle.abort();
            }
            for (_, sender) in inner.response.lock().drain() {
                drop(sender);
            }
        });

        Self(ret)
    }

    pub async fn invoke<Req, Resp>(&self, api: Api, req: &Req) -> Result<Resp, Error>
    where
        Req: serde::Serialize + std::fmt::Debug,
        Resp: for<'de> serde::Deserialize<'de> + std::fmt::Debug,
    {
        let id = RequestId(self.0.request_id.fetch_add(1, Ordering::SeqCst));
        let bytes = bincode::serialize(req).unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.0.response.lock().insert(id, tx);
        let guard = CancelGuard(&self.0, id);

        let message = Message::InvokeRequest(api, id, bytes.into());
        self.0.outbox.send(message).map_err(|_| Error::Connection)?;

        let resp = rx.await.map_err(|_| Error::Connection)?;

        std::mem::forget(guard);
        Ok(bincode::deserialize(&resp[..]).unwrap())
    }

    pub async fn notify<Req>(&self, api: Api, req: &Req) -> Result<(), Error>
    where
        Req: serde::Serialize + std::fmt::Debug,
    {
        let bytes = bincode::serialize(req).unwrap();

        let message = Message::Notify(api, bytes.into());
        self.0.outbox.send(message).map_err(|_| Error::Connection)?;

        Ok(())
    }

    pub async fn closed(&self) {
        self.0.outbox.closed().await
    }
}
