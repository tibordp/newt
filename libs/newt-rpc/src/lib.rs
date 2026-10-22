use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use bytes::{Buf, BufMut};
use futures::FutureExt;
use log::{error, info, warn};
use parking_lot::Mutex;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    task::AbortHandle,
};

use tokio_util::codec::Framed;

use newt_common::Error;

mod streams;
pub use streams::{ByteReader, CHUNK_SIZE, StreamId, StreamReceiver, StreamSender, pump};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Api(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RequestId(pub u64);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Message {
    InvokeRequest(Api, RequestId, bytes::Bytes),
    InvokeResponse(RequestId, bytes::Bytes),
    InvokeCancel(RequestId),
    Notify(Api, bytes::Bytes),
    /// `Notify` on the high lane, so it can be sent from a sync context.
    Signal(Api, bytes::Bytes),
    StreamData(StreamId, bytes::Bytes),
    StreamEnd(StreamId),
    StreamReset(StreamId, Error),
    StreamCredit(StreamId, u32),
    StreamCancel(StreamId),
}

impl Message {
    /// The high lane is unbounded, so these can be sent from a sync context
    /// (a `Drop`); each is small, or bounded by something else. A request
    /// shares its cancel's lane so the cancel can't overtake it.
    fn is_high_priority(&self) -> bool {
        matches!(
            self,
            Message::InvokeRequest(..)
                | Message::InvokeCancel(_)
                | Message::Signal(..)
                | Message::StreamReset(..)
                | Message::StreamCredit(..)
                | Message::StreamCancel(_)
        )
    }
}

// --- Priority-aware outbox ---

/// Shallow: whatever waits here delays every response queued behind it,
/// and streams bound what they send ahead with their own credit.
const LOW_PRIORITY_CHANNEL_CAPACITY: usize = 8;

/// Sender half of the priority-aware outbox. Cloneable.
///
/// Messages are auto-classified by variant into high-priority (unbounded) and
/// low-priority (bounded) internal channels.
#[derive(Clone)]
pub struct Outbox {
    high: tokio::sync::mpsc::UnboundedSender<Message>,
    low: tokio::sync::mpsc::Sender<Message>,
    streams: Arc<streams::Streams>,
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

    /// Send a notification on the low lane.
    pub async fn notify<T: serde::Serialize>(&self, api: Api, payload: &T) -> Result<(), Error> {
        let bytes = bincode::serialize(payload).expect("RPC encode");
        self.send(Message::Notify(api, bytes.into()))
            .await
            .map_err(|_| Error::connection())
    }
}

/// Receiver half of the priority-aware outbox.
pub struct OutboxReceiver {
    high: tokio::sync::mpsc::UnboundedReceiver<Message>,
    low: tokio::sync::mpsc::Receiver<Message>,
}

impl OutboxReceiver {
    /// Wait for the next message, preferring high-priority.
    async fn recv(&mut self) -> Option<Message> {
        tokio::select! {
            biased;
            msg = self.high.recv() => msg,
            msg = self.low.recv() => msg,
        }
    }

    /// Non-blocking receive, preferring high-priority.
    fn try_recv(&mut self) -> Result<Message, tokio::sync::mpsc::error::TryRecvError> {
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
            streams: Default::default(),
        },
        OutboxReceiver {
            high: high_rx,
            low: low_rx,
        },
    )
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
                src.advance(15);
                let slice = src.split_to(len).freeze();

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
                src.advance(13);
                let slice = src.split_to(len).freeze();

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
                if src.len() < 1 + 2 + 4 {
                    return Ok(None);
                }
                let api = (&src[1..3]).read_u16::<NetworkEndian>().unwrap();
                let len = (&src[3..7]).read_u32::<NetworkEndian>().unwrap() as usize;

                if src.len() < 7 + len {
                    src.reserve(7 + len - src.len());
                    return Ok(None);
                }
                src.advance(7);
                let slice = src.split_to(len).freeze();

                Ok(Some(Message::Notify(Api(api), slice)))
            }
            5 => {
                if src.len() < 1 + 2 + 4 {
                    return Ok(None);
                }
                let api = (&src[1..3]).read_u16::<NetworkEndian>().unwrap();
                let len = (&src[3..7]).read_u32::<NetworkEndian>().unwrap() as usize;

                if src.len() < 7 + len {
                    src.reserve(7 + len - src.len());
                    return Ok(None);
                }
                src.advance(7);
                let slice = src.split_to(len).freeze();

                Ok(Some(Message::Signal(Api(api), slice)))
            }
            6 | 8 => {
                if src.len() < 1 + 8 + 4 {
                    return Ok(None);
                }
                let id = StreamId((&src[1..9]).read_u64::<NetworkEndian>().unwrap());
                let len = (&src[9..13]).read_u32::<NetworkEndian>().unwrap() as usize;

                if src.len() < 13 + len {
                    src.reserve(13 + len - src.len());
                    return Ok(None);
                }
                let kind = src[0];
                src.advance(13);
                let slice = src.split_to(len).freeze();

                if kind == 6 {
                    Ok(Some(Message::StreamData(id, slice)))
                } else {
                    let error = bincode::deserialize(&slice).expect("RPC stream reset decode");
                    Ok(Some(Message::StreamReset(id, error)))
                }
            }
            7 | 10 => {
                if src.len() < 1 + 8 {
                    return Ok(None);
                }
                let id = StreamId((&src[1..9]).read_u64::<NetworkEndian>().unwrap());
                let kind = src[0];
                src.advance(9);
                if kind == 7 {
                    Ok(Some(Message::StreamEnd(id)))
                } else {
                    Ok(Some(Message::StreamCancel(id)))
                }
            }
            9 => {
                if src.len() < 1 + 8 + 4 {
                    return Ok(None);
                }
                let id = StreamId((&src[1..9]).read_u64::<NetworkEndian>().unwrap());
                let frames = (&src[9..13]).read_u32::<NetworkEndian>().unwrap();
                src.advance(13);
                Ok(Some(Message::StreamCredit(id, frames)))
            }
            _ => Err(Error::custom("invalid message kind")),
        }
    }
}

impl tokio_util::codec::Encoder<Message> for MessageCodec {
    type Error = Error;

    fn encode(&mut self, item: Message, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        match item {
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
            Message::Signal(api, data) => {
                dst.reserve(1 + 2 + 4 + data.len());
                dst.put_u8(5);
                dst.put_u16(api.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
            Message::StreamData(id, data) => {
                dst.reserve(1 + 8 + 4 + data.len());
                dst.put_u8(6);
                dst.put_u64(id.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
            Message::StreamEnd(id) => {
                dst.reserve(1 + 8);
                dst.put_u8(7);
                dst.put_u64(id.0);
            }
            Message::StreamReset(id, error) => {
                let data = bincode::serialize(&error).expect("RPC stream reset encode");
                dst.reserve(1 + 8 + 4 + data.len());
                dst.put_u8(8);
                dst.put_u64(id.0);
                dst.put_u32(data.len() as u32);
                dst.put_slice(&data);
            }
            Message::StreamCredit(id, frames) => {
                dst.reserve(1 + 8 + 4);
                dst.put_u8(9);
                dst.put_u64(id.0);
                dst.put_u32(frames);
            }
            Message::StreamCancel(id) => {
                dst.reserve(1 + 8);
                dst.put_u8(10);
                dst.put_u64(id.0);
            }
        }

        Ok(())
    }
}

/// Serves the APIs it knows and returns `None`/`false` for the rest, which
/// the next dispatcher in a chain then gets. Both ends of a connection are
/// built from the same source, so an API that nothing serves is a bug, and
/// ends the connection.
#[async_trait::async_trait]
pub trait Dispatcher: Send + Sync + 'static {
    async fn invoke(&self, _api: Api, _req: bytes::Bytes) -> Option<bytes::Bytes> {
        None
    }

    /// Runs on the connection's read loop, ahead of everything that
    /// arrives after it, so it must not wait on anything.
    fn notify(&self, _api: Api, _req: bytes::Bytes) -> bool {
        false
    }
}

/// A dispatcher a connection may or may not have.
#[async_trait::async_trait]
impl<D: Dispatcher> Dispatcher for Option<D> {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        self.as_ref()?.invoke(api, req).await
    }

    fn notify(&self, api: Api, req: bytes::Bytes) -> bool {
        self.as_ref().is_some_and(|d| d.notify(api, req))
    }
}

pub struct ChainDispatcher<T1, T2>(T1, T2);

#[async_trait::async_trait]
impl<T1: Dispatcher, T2: Dispatcher> Dispatcher for ChainDispatcher<T1, T2> {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        match self.0.invoke(api, req.clone()).await {
            Some(res) => Some(res),
            None => self.1.invoke(api, req).await,
        }
    }

    fn notify(&self, api: Api, req: bytes::Bytes) -> bool {
        self.0.notify(api, req.clone()) || self.1.notify(api, req)
    }
}

pub trait DispatcherExt: Dispatcher + Sized {
    fn chain<Other: Dispatcher>(self, next: Other) -> ChainDispatcher<Self, Other> {
        ChainDispatcher(self, next)
    }
}

impl<T: Dispatcher> DispatcherExt for T {}

struct NullDispatcher;

impl Dispatcher for NullDispatcher {}

/// Cancels an invocation dropped before its response arrived.
struct CancelGuard<'a>(&'a CommunicatorInner, RequestId);

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        let waiting = self
            .0
            .response
            .lock()
            .as_mut()
            .and_then(|calls| calls.remove(&self.1));
        if waiting.is_some() {
            let _ = self.0.outbox.send_high(Message::InvokeCancel(self.1));
        }
    }
}

/// The byte pipe a connection runs over.
pub trait Transport: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Transport for T {}

struct CommunicatorInner {
    request_id: AtomicU64,
    tasks: Mutex<HashMap<RequestId, AbortHandle>>,
    /// Calls waiting on a response; `None` once the connection has ended,
    /// so a call can't start waiting on one that never comes.
    response: Mutex<Option<HashMap<RequestId, tokio::sync::oneshot::Sender<bytes::Bytes>>>>,
    dispatcher: Arc<dyn Dispatcher>,
    outbox: Outbox,
    /// Ends the connection: [`Communicator::shutdown`], or an invoke
    /// handler that panicked.
    closing: tokio_util::sync::CancellationToken,
    /// The connection has ended and everything waiting on it has failed.
    closed: tokio_util::sync::CancellationToken,
}

impl CommunicatorInner {
    /// Serve the connection until it ends, then fail everything still
    /// waiting on it.
    async fn run<T: Transport>(self: Arc<Self>, transport: T, inbox: OutboxReceiver) {
        use futures::StreamExt;

        let (tx, rx) = Framed::new(transport, MessageCodec {}).split();
        let mut sender = tokio::spawn(write_all(tx, inbox));

        // A panic is a bug at one end or the other; like a failed write, it
        // ends the connection, so nothing is left waiting on it.
        tokio::select! {
            served = AssertUnwindSafe(self.clone().serve(rx)).catch_unwind() => match served {
                Ok(Ok(())) => info!("connection closed"),
                Ok(Err(e)) => error!("connection error: {}", e),
                Err(_) => error!("connection ended by a panic"),
            },
            // The messages a failed write was carrying are lost, and the
            // calls waiting on them would otherwise wait until the read side
            // happened to end too.
            written = &mut sender => match written {
                Ok(Ok(())) => info!("connection closed"),
                Ok(Err(e)) => warn!("connection write error: {}", e),
                Err(_) => error!("connection ended by a panic"),
            },
        }

        sender.abort();
        for (_, task) in self.tasks.lock().drain() {
            task.abort();
        }
        self.response.lock().take();
        self.outbox.streams.close();
        self.closed.cancel();
    }

    async fn serve<R>(self: Arc<Self>, mut rx: R) -> Result<(), Error>
    where
        R: futures::Stream<Item = Result<Message, Error>> + Unpin,
    {
        use futures::StreamExt;

        loop {
            let msg = tokio::select! {
                next = rx.next() => match next {
                    Some(msg) => msg?,
                    None => return Ok(()),
                },
                _ = self.closing.cancelled() => return Ok(()),
            };
            match msg {
                Message::InvokeRequest(api, id, payload) => {
                    let dispatcher = self.dispatcher.clone();
                    let outbox = self.outbox.clone();
                    let inner = self.clone();
                    // Hold the map lock across spawn+insert: the task's final
                    // action is removing its own entry, and if it completed
                    // before the insert the finished task's handle would sit
                    // in the map forever.
                    let mut tasks = self.tasks.lock();
                    let handle = tokio::spawn(async move {
                        let invoked = AssertUnwindSafe(async {
                            let response = dispatcher.invoke(api, payload).await;
                            response.unwrap_or_else(|| panic!("nothing serves {api:?}"))
                        });
                        match invoked.catch_unwind().await {
                            Ok(response) => {
                                let _ = outbox.send(Message::InvokeResponse(id, response)).await;
                            }
                            // The caller would wait forever for an answer; end
                            // the connection instead, failing every call.
                            Err(_) => inner.closing.cancel(),
                        }
                        inner.tasks.lock().remove(&id);
                    });
                    tasks.insert(id, handle.abort_handle());
                }
                Message::InvokeResponse(id, payload) => {
                    let waiting = self
                        .response
                        .lock()
                        .as_mut()
                        .and_then(|calls| calls.remove(&id));
                    if let Some(sender) = waiting {
                        let _ = sender.send(payload);
                    }
                }
                Message::InvokeCancel(id) => {
                    if let Some(task) = self.tasks.lock().remove(&id) {
                        task.abort();
                    }
                }
                Message::Notify(api, payload) | Message::Signal(api, payload) => {
                    let served = self.dispatcher.notify(api, payload);
                    assert!(served, "nothing serves {api:?}");
                }
                Message::StreamData(id, data) => self.outbox.streams.on_data(id, data),
                Message::StreamEnd(id) => self.outbox.streams.on_end(id, Ok(())),
                Message::StreamReset(id, error) => self.outbox.streams.on_end(id, Err(error)),
                Message::StreamCredit(id, frames) => self.outbox.streams.on_credit(id, frames),
                Message::StreamCancel(id) => self.outbox.streams.on_cancel(id),
            }
        }
    }
}

/// Write what the outbox hands out until a write fails.
async fn write_all<S>(mut tx: S, mut inbox: OutboxReceiver) -> Result<(), Error>
where
    S: futures::Sink<Message, Error = Error> + Unpin,
{
    use futures::SinkExt;

    while let Some(msg) = inbox.recv().await {
        tx.feed(msg).await?;
        // Drain any additional messages that are already available before
        // flushing, so consecutive messages share one flush. Priority is
        // enforced by try_recv (high before low).
        while let Ok(msg) = inbox.try_recv() {
            tx.feed(msg).await?;
        }
        tx.flush().await?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct Communicator(Arc<CommunicatorInner>);

impl Communicator {
    /// A connection that only calls the other end.
    pub fn new<T: Transport>(transport: T) -> Self {
        Self::with_dispatcher(transport, |_| NullDispatcher)
    }

    /// A connection that serves the other end with the dispatcher `make`
    /// builds from its outbox (which the dispatcher answers streams and
    /// notifications on).
    pub fn with_dispatcher<T: Transport, D: Dispatcher>(
        transport: T,
        make: impl FnOnce(&Outbox) -> D,
    ) -> Self {
        let (outbox, inbox) = create_outbox();
        let inner = Arc::new(CommunicatorInner {
            request_id: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            response: Mutex::new(Some(HashMap::new())),
            dispatcher: Arc::new(make(&outbox)),
            outbox,
            closing: tokio_util::sync::CancellationToken::new(),
            closed: tokio_util::sync::CancellationToken::new(),
        });
        tokio::spawn(inner.clone().run(transport, inbox));
        Self(inner)
    }

    pub async fn invoke<Req, Resp>(&self, api: Api, req: &Req) -> Result<Resp, Error>
    where
        Req: serde::Serialize + std::fmt::Debug,
        Resp: for<'de> serde::Deserialize<'de> + std::fmt::Debug,
    {
        let id = RequestId(self.0.request_id.fetch_add(1, Ordering::Relaxed));
        let bytes = bincode::serialize(req).expect("RPC encode");

        let (tx, rx) = tokio::sync::oneshot::channel();
        match self.0.response.lock().as_mut() {
            Some(calls) => calls.insert(id, tx),
            None => return Err(Error::connection()),
        };
        let _guard = CancelGuard(&self.0, id);

        let message = Message::InvokeRequest(api, id, bytes.into());
        self.0
            .outbox
            .send_high(message)
            .map_err(|_| Error::connection())?;

        let resp = rx.await.map_err(|_| Error::connection())?;
        Ok(bincode::deserialize(&resp[..]).expect("RPC decode"))
    }

    /// A notification on the unbounded high lane, so it can be sent from a
    /// sync context (a `Drop`). Keep it small and rare.
    pub fn signal<Req>(&self, api: Api, req: &Req) -> Result<(), Error>
    where
        Req: serde::Serialize + std::fmt::Debug,
    {
        let bytes = bincode::serialize(req).expect("RPC encode");

        let message = Message::Signal(api, bytes.into());
        self.0
            .outbox
            .send_high(message)
            .map_err(|_| Error::connection())?;

        Ok(())
    }

    pub fn outbox(&self) -> Outbox {
        self.0.outbox.clone()
    }

    /// See [`Outbox::open_stream`].
    pub fn open_stream(&self) -> (StreamId, StreamReceiver) {
        self.0.outbox.open_stream()
    }

    /// See [`Outbox::stream_sender`].
    pub fn stream_sender(&self, id: StreamId) -> StreamSender {
        self.0.outbox.stream_sender(id)
    }

    /// Close the connection: the stream is dropped, so the peer reads EOF,
    /// and calls in flight fail. Clones and the connection's own tasks
    /// otherwise keep it open for as long as the stream is.
    pub fn shutdown(&self) {
        self.0.closing.cancel();
    }

    /// Completes once the connection has ended and everything waiting on
    /// it has failed.
    pub async fn closed(&self) {
        self.0.closed.cancelled().await
    }
}

#[cfg(test)]
mod tests;
