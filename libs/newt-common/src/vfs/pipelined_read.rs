//! `AsyncRead` over pipelined positioned reads on one held-open upstream
//! handle — the shared skeleton behind the zip and disc streaming readers.
//!
//! A sans-IO [`ChunkDriver`] decides what to fetch and turns fetched bytes
//! into output; [`PipelinedReader`] owns the IO. The in-flight future owns
//! the upstream handle and hands it back with the result — exactly one of
//! `upstream`/`inflight` holds it at any time. Dropping the reader drops
//! any in-flight read (and with it the handle) — cancellation propagates
//! naturally.

use std::future::Future;
use std::io::Read;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::AsyncRead;

use crate::Error;

use super::VfsRandomReader;

/// What a [`ChunkDriver::step`] produced.
pub(crate) enum DriveStep {
    /// Fetch `len` bytes at `offset` from the upstream; the data comes back
    /// through the next `step(Some(…))` call. A short read is an error —
    /// the formats know their exact extents.
    Need {
        offset: u64,
        len: u64,
    },
    /// Bytes ready for the consumer.
    Output(Vec<u8>),
    Done,
}

/// Sans-IO byte producer: turns fetched upstream chunks into output bytes.
pub(crate) trait ChunkDriver: Send {
    /// Advance. `fetched` is the completed [`DriveStep::Need`] read tagged
    /// with its offset, or `None` when nothing was outstanding.
    fn step(&mut self, fetched: Option<(u64, Vec<u8>)>) -> Result<DriveStep, Error>;

    /// Output accumulated inside the driver independent of `step` — lets
    /// the adapter hand bytes to the consumer while a fetch is still in
    /// flight (a decompressor can buffer output beside a pending `Need`).
    /// Drivers with no internal buffer keep the default.
    fn take_buffered(&mut self) -> Vec<u8> {
        Vec::new()
    }
}

type ChunkFuture =
    Pin<Box<dyn Future<Output = (Box<dyn VfsRandomReader>, u64, Result<Vec<u8>, Error>)> + Send>>;

pub(crate) struct PipelinedReader<D> {
    driver: D,
    /// `None` while a read is in flight — and for drivers that never fetch
    /// (an inline-only disc entry opens with no handle at all).
    upstream: Option<Box<dyn VfsRandomReader>>,
    inflight: Option<ChunkFuture>,
    leftover: std::io::Cursor<Vec<u8>>,
    done: bool,
    /// Names the byte source in truncation errors ("ZIP archive", …).
    label: &'static str,
}

impl<D: ChunkDriver> PipelinedReader<D> {
    pub(crate) fn new(
        driver: D,
        upstream: Option<Box<dyn VfsRandomReader>>,
        label: &'static str,
    ) -> Self {
        Self {
            driver,
            upstream,
            inflight: None,
            leftover: std::io::Cursor::new(Vec::new()),
            done: false,
            label,
        }
    }
}

impl<D: ChunkDriver + Unpin> AsyncRead for PipelinedReader<D> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            let n = self
                .leftover
                .read(out.initialize_unfilled())
                .expect("cursor read");
            if n > 0 {
                out.advance(n);
                return Poll::Ready(Ok(()));
            }

            let buffered = self.driver.take_buffered();
            if !buffered.is_empty() {
                self.leftover = std::io::Cursor::new(buffered);
                continue;
            }

            if self.done {
                return Poll::Ready(Ok(())); // EOF
            }

            let fetched = if let Some(mut fut) = self.inflight.take() {
                match fut.as_mut().poll(cx) {
                    Poll::Pending => {
                        self.inflight = Some(fut);
                        return Poll::Pending;
                    }
                    Poll::Ready((handle, offset, result)) => {
                        self.upstream = Some(handle);
                        match result {
                            Ok(data) => Some((offset, data)),
                            Err(e) => {
                                return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                            }
                        }
                    }
                }
            } else {
                None
            };

            match self.driver.step(fetched) {
                Ok(DriveStep::Need { offset, len }) => {
                    let mut handle = self.upstream.take().expect(
                        "pipelined reader: driver requested a read without an upstream handle",
                    );
                    let label = self.label;
                    self.inflight = Some(Box::pin(async move {
                        let result = match handle.read_at(offset, len).await {
                            Ok(data) if (data.len() as u64) < len => Err(Error::custom(format!(
                                "{label} truncated: read came up short"
                            ))),
                            Ok(data) => Ok(data),
                            Err(e) => Err(e),
                        };
                        (handle, offset, result)
                    }));
                }
                Ok(DriveStep::Output(data)) => {
                    self.leftover = std::io::Cursor::new(data);
                }
                Ok(DriveStep::Done) => {
                    self.done = true;
                }
                Err(e) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
            }
        }
    }
}
