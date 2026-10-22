//! Streams multiplexed over the connection. A stream runs one way, and the
//! reader opens it: opening allocates the id and the buffer, and the reader
//! hands the id to the writer inside an invoke's request or response.
//!
//! - writer → reader: `StreamData` and `StreamEnd` on the low lane, so the
//!   end follows the data; `StreamReset` (the writer is gone) on the high lane.
//! - reader → writer: `StreamCredit` and `StreamCancel` (the reader is gone),
//!   both on the high lane.
//!
//! A frame's direction picks the table its id is looked up in, so the ids
//! each side allocates never collide with the peer's.
//!
//! A writer may have at most `WINDOW` frames the reader hasn't taken yet,
//! and the reader returns credit as it takes them; the read loop therefore
//! never waits on a consumer. A reset can overtake data still queued on the
//! low lane, so it discards whatever the reader had buffered. Frames for a
//! stream that has ended, or whose other end is gone, are dropped.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker, ready};

use bytes::{Buf, Bytes, BytesMut};
use parking_lot::Mutex;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use super::{Message, Outbox};
use newt_common::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StreamId(pub u64);

/// Frames a writer may send ahead of the reader.
const WINDOW: u32 = 32;

/// The size [`pump`] cuts byte streams into.
pub const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Streams {
    next_id: AtomicU64,
    tables: Mutex<Tables>,
}

#[derive(Default)]
struct Tables {
    /// Streams this side reads, by the ids it allocated.
    readers: HashMap<StreamId, Arc<Mutex<Inbound>>>,
    /// Streams this side writes, by the ids the peer allocated.
    writers: HashMap<StreamId, Arc<Outbound>>,
    /// The connection has ended; every stream fails.
    closed: bool,
}

#[derive(Default)]
struct Inbound {
    frames: VecDeque<Bytes>,
    /// How the stream ended: `Ok` once the buffered frames are taken.
    end: Option<Result<(), Error>>,
    waker: Option<Waker>,
}

impl Inbound {
    fn end(&mut self, result: Result<(), Error>) {
        if result.is_err() {
            self.frames.clear();
        }
        self.end = Some(result);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

struct Outbound {
    credit: Semaphore,
    /// The reader is gone, or the connection.
    gone: CancellationToken,
}

impl Outbound {
    fn release(&self) {
        self.gone.cancel();
        self.credit.close();
    }
}

impl Streams {
    fn open(&self) -> (StreamId, Arc<Mutex<Inbound>>) {
        let id = StreamId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let inbound = Arc::new(Mutex::new(Inbound::default()));
        let mut tables = self.tables.lock();
        if tables.closed {
            inbound.lock().end(Err(Error::connection()));
        } else {
            tables.readers.insert(id, inbound.clone());
        }
        (id, inbound)
    }

    fn writer(&self, id: StreamId) -> Arc<Outbound> {
        let outbound = Arc::new(Outbound {
            credit: Semaphore::new(WINDOW as usize),
            gone: CancellationToken::new(),
        });
        let mut tables = self.tables.lock();
        if tables.closed {
            outbound.release();
        } else {
            let previous = tables.writers.insert(id, outbound.clone());
            assert!(previous.is_none(), "two writers for stream {id:?}");
        }
        outbound
    }

    /// Late frames for a stream this side has let go of are expected; a
    /// frame for one it never opened is not.
    fn check_opened(&self, id: StreamId) {
        assert!(
            id.0 < self.next_id.load(Ordering::Relaxed),
            "frame for stream {id:?}, which was never opened"
        );
    }

    pub(super) fn on_data(&self, id: StreamId, data: Bytes) {
        let Some(inbound) = self.tables.lock().readers.get(&id).cloned() else {
            return self.check_opened(id);
        };
        let mut inbound = inbound.lock();
        assert!(
            inbound.frames.len() < WINDOW as usize,
            "stream {id:?} overran its window"
        );
        inbound.frames.push_back(data);
        if let Some(waker) = inbound.waker.take() {
            waker.wake();
        }
    }

    pub(super) fn on_end(&self, id: StreamId, result: Result<(), Error>) {
        let Some(inbound) = self.tables.lock().readers.remove(&id) else {
            return self.check_opened(id);
        };
        inbound.lock().end(result);
    }

    pub(super) fn on_credit(&self, id: StreamId, frames: u32) {
        if let Some(outbound) = self.tables.lock().writers.get(&id) {
            outbound.credit.add_permits(frames as usize);
        }
    }

    pub(super) fn on_cancel(&self, id: StreamId) {
        if let Some(outbound) = self.tables.lock().writers.remove(&id) {
            outbound.release();
        }
    }

    pub(super) fn close(&self) {
        let mut tables = self.tables.lock();
        tables.closed = true;
        for (_, inbound) in tables.readers.drain() {
            inbound.lock().end(Err(Error::connection()));
        }
        for (_, outbound) in tables.writers.drain() {
            outbound.release();
        }
    }
}

impl Outbox {
    /// Open a stream for the peer to write: send it the id.
    pub fn open_stream(&self) -> (StreamId, StreamReceiver) {
        let (id, inbound) = self.streams.open();
        let receiver = StreamReceiver {
            id,
            inbound,
            outbox: self.clone(),
            taken: 0,
        };
        (id, receiver)
    }

    /// Write the stream the peer opened as `id`.
    pub fn stream_sender(&self, id: StreamId) -> StreamSender {
        StreamSender {
            id,
            outbound: self.streams.writer(id),
            outbox: self.clone(),
            ended: false,
        }
    }
}

/// The reading end. Dropping it before the stream ends cancels the stream.
pub struct StreamReceiver {
    id: StreamId,
    inbound: Arc<Mutex<Inbound>>,
    outbox: Outbox,
    /// Frames taken since credit was last returned.
    taken: u32,
}

impl StreamReceiver {
    /// The next frame; `None` once the stream has ended. A failed stream
    /// yields its error once, then ends.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, Error>>> {
        let mut inbound = self.inbound.lock();
        if let Some(frame) = inbound.frames.pop_front() {
            self.taken += 1;
            if self.taken >= WINDOW / 2 && inbound.end.is_none() {
                let _ = self
                    .outbox
                    .send_high(Message::StreamCredit(self.id, self.taken));
                self.taken = 0;
            }
            return Poll::Ready(Some(Ok(frame)));
        }
        match inbound.end.take() {
            None => {
                inbound.waker = Some(cx.waker().clone());
                Poll::Pending
            }
            Some(end) => {
                inbound.end = Some(Ok(()));
                Poll::Ready(end.err().map(Err))
            }
        }
    }

    pub async fn recv(&mut self) -> Option<Result<Bytes, Error>> {
        std::future::poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// The next frame, as an item [`StreamSender::send_item`] sent.
    pub async fn recv_item<T: serde::de::DeserializeOwned>(&mut self) -> Option<Result<T, Error>> {
        let frame = self.recv().await?;
        Some(frame.map(|bytes| bincode::deserialize(&bytes).expect("RPC stream item decode")))
    }

    /// Pass every item on to `tx` until the stream ends, or until `tx`'s
    /// receiver is gone.
    pub async fn forward_items<T: serde::de::DeserializeOwned>(
        mut self,
        tx: &mpsc::Sender<T>,
    ) -> Result<(), Error> {
        while let Some(item) = self.recv_item().await {
            if tx.send(item?).await.is_err() {
                break;
            }
        }
        Ok(())
    }

    pub fn into_async_read(self) -> ByteReader {
        ByteReader {
            receiver: self,
            chunk: Bytes::new(),
        }
    }
}

impl Drop for StreamReceiver {
    fn drop(&mut self) {
        let open = self.outbox.streams.tables.lock().readers.remove(&self.id);
        if open.is_some() {
            let _ = self.outbox.send_high(Message::StreamCancel(self.id));
        }
    }
}

/// A byte stream as an [`tokio::io::AsyncRead`].
pub struct ByteReader {
    receiver: StreamReceiver,
    chunk: Bytes,
}

impl tokio::io::AsyncRead for ByteReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        while self.chunk.is_empty() {
            match ready!(self.receiver.poll_recv(cx)) {
                Some(Ok(chunk)) => self.chunk = chunk,
                Some(Err(e)) => return Poll::Ready(Err(e.into())),
                None => return Poll::Ready(Ok(())),
            }
        }
        let n = self.chunk.len().min(buf.remaining());
        buf.put_slice(&self.chunk[..n]);
        self.chunk.advance(n);
        Poll::Ready(Ok(()))
    }
}

/// The writing end. Dropping it without [`finish`](Self::finish) resets
/// the stream.
pub struct StreamSender {
    id: StreamId,
    outbound: Arc<Outbound>,
    outbox: Outbox,
    ended: bool,
}

impl StreamSender {
    /// Send a frame once the reader has room for it.
    pub async fn send(&mut self, data: Bytes) -> Result<(), Error> {
        match self.outbound.credit.acquire().await {
            Ok(permit) => permit.forget(),
            Err(_) => return Err(self.gone()),
        }
        self.outbox
            .send(Message::StreamData(self.id, data))
            .await
            .map_err(|_| Error::connection())
    }

    /// Send `item` as one frame, for [`StreamReceiver::recv_item`].
    pub async fn send_item<T: serde::Serialize>(&mut self, item: &T) -> Result<(), Error> {
        let bytes = bincode::serialize(item).expect("RPC stream item encode");
        self.send(bytes.into()).await
    }

    /// End the stream after the frames already sent.
    pub async fn finish(mut self) -> Result<(), Error> {
        self.ended = true;
        self.outbox.streams.tables.lock().writers.remove(&self.id);
        self.outbox
            .send(Message::StreamEnd(self.id))
            .await
            .map_err(|_| Error::connection())
    }

    /// Fail the stream: the reader drops what it hasn't taken yet and gets
    /// `error`.
    pub fn reset(mut self, error: Error) {
        self.ended = true;
        self.reset_with(error);
    }

    /// Completes once the reader is gone, or the connection.
    pub async fn cancelled(&self) {
        self.outbound.gone.cancelled().await
    }

    /// Run `work`, sending each item it produces on `items`, then end the
    /// stream. Gives up on `work` if the reader goes away.
    pub async fn forward<T: serde::Serialize, R>(
        mut self,
        mut items: mpsc::Receiver<T>,
        work: impl Future<Output = R>,
    ) -> Result<R, Error> {
        tokio::pin!(work);
        let ret = loop {
            tokio::select! {
                ret = &mut work => break ret,
                item = items.recv() => match item {
                    Some(item) => self.send_item(&item).await?,
                    None => break (&mut work).await,
                },
            }
        };
        // `work` has dropped its sender; pass on what it queued.
        while let Some(item) = items.recv().await {
            self.send_item(&item).await?;
        }
        self.finish().await?;
        Ok(ret)
    }

    fn gone(&self) -> Error {
        if self.outbox.streams.tables.lock().closed {
            Error::connection()
        } else {
            Error::cancelled()
        }
    }

    fn reset_with(&mut self, error: Error) {
        self.outbox.streams.tables.lock().writers.remove(&self.id);
        if !self.outbound.gone.is_cancelled() {
            let _ = self.outbox.send_high(Message::StreamReset(self.id, error));
        }
    }
}

impl Drop for StreamSender {
    fn drop(&mut self) {
        if !self.ended {
            self.reset_with(Error::cancelled());
        }
    }
}

/// Copy `reader` into the stream, ending it at EOF or resetting it with
/// the read error. Stops reading as soon as the reader of the stream goes
/// away.
pub async fn pump(
    reader: &mut (dyn tokio::io::AsyncRead + Send + Unpin),
    mut tx: StreamSender,
) -> Result<(), Error> {
    use tokio::io::AsyncReadExt;
    loop {
        let mut chunk = BytesMut::with_capacity(CHUNK_SIZE);
        let read = tokio::select! {
            biased;
            _ = tx.cancelled() => return Err(tx.gone()),
            read = reader.read_buf(&mut chunk) => read,
        };
        match read {
            Ok(0) => return tx.finish().await,
            Ok(_) => tx.send(chunk.freeze()).await?,
            Err(e) => {
                let e = Error::from(e);
                tx.reset(e.clone());
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
#[path = "streams_tests.rs"]
mod tests;
