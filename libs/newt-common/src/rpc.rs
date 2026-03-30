use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
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

impl Message {
    fn is_high_priority(&self) -> bool {
        matches!(
            self,
            Message::Ping(_) | Message::InvokeRequest(..) | Message::InvokeCancel(_)
        )
    }
}

// --- Priority-aware outbox ---

const LOW_PRIORITY_CHANNEL_CAPACITY: usize = 64;

/// Sender half of the priority-aware outbox. Cloneable.
///
/// Messages are auto-classified by variant into high-priority (unbounded) and
/// low-priority (bounded) internal channels.
#[derive(Clone)]
pub struct Outbox {
    high: tokio::sync::mpsc::UnboundedSender<Message>,
    low: tokio::sync::mpsc::Sender<Message>,
}

impl Outbox {
    /// Synchronous, non-blocking send for high-priority messages only.
    ///
    /// Panics if `msg` is not high-priority. The high-priority channel is
    /// unbounded, so this never blocks due to capacity — it only fails if the
    /// receiver has been dropped.
    pub fn send_high(
        &self,
        msg: Message,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<Message>> {
        assert!(
            msg.is_high_priority(),
            "send_high called with low-priority message: {:?}",
            std::mem::discriminant(&msg),
        );
        self.high.send(msg)
    }

    /// Async send with backpressure. High-priority messages go through the
    /// unbounded channel and never block. Low-priority messages await capacity
    /// on the bounded queue.
    pub async fn send(
        &self,
        msg: Message,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<Message>> {
        if msg.is_high_priority() {
            self.high.send(msg)
        } else {
            self.low.send(msg).await
        }
    }

    /// Blocking send for low-priority messages. Intended for use inside
    /// `spawn_blocking` where `.await` is not available.
    pub fn blocking_send_low(
        &self,
        msg: Message,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<Message>> {
        assert!(
            !msg.is_high_priority(),
            "blocking_send_low called with high-priority message: {:?}",
            std::mem::discriminant(&msg),
        );
        self.low.blocking_send(msg)
    }

    /// Completes when the receiver half is dropped.
    pub async fn closed(&self) {
        self.high.closed().await
    }
}

/// Receiver half of the priority-aware outbox.
pub struct OutboxReceiver {
    high: tokio::sync::mpsc::UnboundedReceiver<Message>,
    low: tokio::sync::mpsc::Receiver<Message>,
}

impl OutboxReceiver {
    /// Wait for the next message, preferring high-priority.
    pub async fn recv(&mut self) -> Option<Message> {
        tokio::select! {
            biased;
            msg = self.high.recv() => msg,
            msg = self.low.recv() => msg,
        }
    }

    /// Non-blocking receive, preferring high-priority.
    pub fn try_recv(&mut self) -> Result<Message, tokio::sync::mpsc::error::TryRecvError> {
        self.high.try_recv().or_else(|_| self.low.try_recv())
    }
}

pub fn create_outbox() -> (Outbox, OutboxReceiver) {
    let (high_tx, high_rx) = tokio::sync::mpsc::unbounded_channel();
    let (low_tx, low_rx) = tokio::sync::mpsc::channel(LOW_PRIORITY_CHANNEL_CAPACITY);
    (
        Outbox {
            high: high_tx,
            low: low_tx,
        },
        OutboxReceiver {
            high: high_rx,
            low: low_rx,
        },
    )
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;

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
            _ => Err(Error::custom("invalid message kind")),
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
        let _ = self.0.outbox.send_high(Message::InvokeCancel(self.1));
    }
}

pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Stream for T {}

struct CommunicatorInner {
    request_id: AtomicU64,
    tasks: Mutex<HashMap<RequestId, AbortHandle>>,
    response: Mutex<HashMap<RequestId, tokio::sync::oneshot::Sender<bytes::Bytes>>>,
    dispatcher: Arc<dyn Dispatcher>,
    outbox: Outbox,
}

impl CommunicatorInner {
    async fn handle_connection<S: Stream>(
        self: Arc<Self>,
        stream: S,
        mut inbox: OutboxReceiver,
    ) -> Result<(), Error> {
        use futures::SinkExt;
        use futures::StreamExt;

        let (mut tx, mut rx) = Framed::new(stream, MessageCodec {}).split();

        let sender = {
            tokio::spawn(async move {
                while let Some(msg) = inbox.recv().await {
                    tx.feed(msg).await?;
                    // Drain any additional messages that are already available
                    // before flushing, so consecutive messages share one flush.
                    // Priority is enforced by try_recv (high before low).
                    while let Ok(msg) = inbox.try_recv() {
                        tx.feed(msg).await?;
                    }
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
                            let _ = self.outbox.send_high(Message::Ping(true));
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
                                        let _ =
                                            outbox.send(Message::InvokeResponse(id, resp)).await;
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
                        // Notifications are processed inline (not spawned) to
                        // guarantee ordering with respect to subsequent
                        // invoke responses/requests.  This is critical for
                        // streaming protocols (read/write chunks) where the
                        // invoke that follows the notification stream must
                        // not be processed before all chunk notifications.
                        match self.dispatcher.notify(api, payload).await {
                            Ok(true) => {}
                            Ok(false) => {
                                error!("unknown API invoked");
                            }
                            Err(e) => {
                                error!("error handling notification: {}", e)
                            }
                        }
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

        // Abort all pending handler tasks so their outbox sender clones are
        // dropped, then abort the sender task.  Without this, the sender task
        // would wait forever for the channel to close (since handler tasks and
        // CommunicatorInner.outbox keep senders alive), deadlocking shutdown.
        for (_, handle) in self.tasks.lock().drain() {
            handle.abort();
        }
        sender.abort();

        result
    }
}

#[derive(Clone)]
pub struct Communicator(Arc<CommunicatorInner>);

impl Communicator {
    pub fn new<S: Stream>(stream: S) -> Self {
        Self::with_dispatcher(NullDispatcher, stream)
    }

    /// Create a pre-connected outbox that can be passed to
    /// `with_dispatcher_and_outbox`. This allows other components (like
    /// OperationDispatcher) to send messages before the Communicator is built.
    pub fn create_outbox() -> (Outbox, OutboxReceiver) {
        create_outbox()
    }

    pub fn with_dispatcher<D: Dispatcher, S: Stream>(dispatcher: D, stream: S) -> Self {
        let (outbox, inbox) = Self::create_outbox();
        Self::with_dispatcher_and_outbox(dispatcher, stream, outbox, inbox)
    }

    pub fn with_dispatcher_and_outbox<D: Dispatcher, S: Stream>(
        dispatcher: D,
        stream: S,
        outbox: Outbox,
        inbox: OutboxReceiver,
    ) -> Self {
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
        self.0
            .outbox
            .send_high(message)
            .map_err(|_| Error::connection())?;

        let resp = rx.await.map_err(|_| Error::connection())?;

        std::mem::forget(guard);
        Ok(bincode::deserialize(&resp[..]).unwrap())
    }

    pub async fn notify<Req>(&self, api: Api, req: &Req) -> Result<(), Error>
    where
        Req: serde::Serialize + std::fmt::Debug,
    {
        let bytes = bincode::serialize(req).unwrap();

        let message = Message::Notify(api, bytes.into());
        self.0
            .outbox
            .send(message)
            .await
            .map_err(|_| Error::connection())?;

        Ok(())
    }

    pub fn outbox(&self) -> Outbox {
        self.0.outbox.clone()
    }

    pub async fn closed(&self) {
        self.0.outbox.closed().await
    }
}
