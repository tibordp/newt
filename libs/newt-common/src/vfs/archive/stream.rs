//! Shared plumbing for archive VFSes that decode entries out of one
//! compressed stream with iluvatar's stream layer: a pool of parked
//! [`StreamReader`]s so sequential reads pick up where the last one
//! stopped, the packed-range read helpers, and a [`ChunkDriver`] that
//! streams one entry and hands its reader back when it is done.

use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use iluvatar::{EngineRequest, StreamIndex, StreamReader};
use tokio::io::AsyncRead;

use crate::Error;

use super::super::pipelined_read::{ChunkDriver, DriveStep, PipelinedReader};
use super::super::{ConsumerGuard, VfsRandomReader};

/// Parked readers kept per stream, LRU at the back. Each holds a live
/// decoder with the stream's whole dictionary, so large-window streams
/// keep fewer.
pub(super) const MAX_READERS: usize = 4;

/// Reader state a stream may keep parked.
pub(super) const READERS_BUDGET: u64 = 128 << 20;

/// Compressed input is requested in slices of at most this.
pub(super) const PACKED_SLICE: u64 = 256 << 10;

/// Parked readers for one compressed stream.
pub(super) struct ReaderPool {
    readers: parking_lot::Mutex<Vec<StreamReader>>,
    max: usize,
}

impl ReaderPool {
    pub(super) fn new(max: usize) -> Self {
        Self {
            readers: parking_lot::Mutex::new(Vec::new()),
            max,
        }
    }

    /// Readers to keep for a stream whose decoder state costs `cost`
    /// bytes per reader.
    pub(super) fn max_for_cost(cost: u64) -> usize {
        (READERS_BUDGET / cost.max(1)).clamp(1, MAX_READERS as u64) as usize
    }

    /// The parked reader best placed to serve `offset..offset + len`,
    /// retargeted at that range: the closest one at or before the offset
    /// that stands at or past the checkpoint a fresh reader would restore,
    /// so resuming it never decodes more than starting over would.
    pub(super) fn take(&self, index: &StreamIndex, offset: u64, len: u64) -> Option<StreamReader> {
        let floor = index
            .best_checkpoint_for_offset(offset)
            .1
            .uncompressed_offset;
        let mut readers = self.readers.lock();
        let i = readers
            .iter()
            .enumerate()
            .filter(|(_, r)| (floor..=offset).contains(&r.position()))
            .max_by_key(|(_, r)| r.position())
            .map(|(i, _)| i)?;
        let mut reader = readers.remove(i);
        reader.seek_forward(offset, len).ok()?;
        Some(reader)
    }

    #[cfg(test)]
    pub(super) fn parked(&self) -> usize {
        self.readers.lock().len()
    }

    pub(super) fn park(&self, reader: StreamReader) {
        let mut readers = self.readers.lock();
        readers.push(reader);
        while readers.len() > self.max {
            readers.remove(0);
        }
    }
}

/// Packed bytes at `at` (relative to the stream's packed range), at most
/// `len`; `None` at the end of the stream.
pub(super) async fn read_packed(
    upstream: &mut dyn VfsRandomReader,
    packed: &Range<u64>,
    at: u64,
    len: u64,
) -> Result<Option<Vec<u8>>, Error> {
    let start = packed.start.saturating_add(at);
    if start >= packed.end {
        return Ok(None);
    }
    let want = len.min(packed.end - start);
    let data = upstream.read_at(start, want).await?;
    if (data.len() as u64) < want {
        return Err(Error::custom("archive truncated: read came up short"));
    }
    Ok(Some(data))
}

/// Pull up to `want` bytes from the reader's current range, returning the
/// reader for the caller to park.
pub(super) async fn drive_reader(
    upstream: &mut dyn VfsRandomReader,
    packed: &Range<u64>,
    mut reader: StreamReader,
    want: u64,
) -> Result<(Vec<u8>, StreamReader), Error> {
    let mut out: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.step() {
            EngineRequest::NeedInput => {
                let at = reader.compressed_position();
                match read_packed(upstream, packed, at, PACKED_SLICE).await? {
                    Some(data) => reader.provide_data(&data),
                    None => reader.signal_eof(),
                }
            }
            EngineRequest::SeekAndRead { offset, len } => {
                match read_packed(upstream, packed, offset, len as u64).await? {
                    Some(data) => reader.provide_data(&data),
                    None => reader.signal_eof(),
                }
            }
            EngineRequest::OutputReady => loop {
                let n = reader.read_output(&mut buf);
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
                if out.len() as u64 >= want {
                    return Ok((out, reader));
                }
            },
            EngineRequest::Done => return Ok((out, reader)),
            EngineRequest::Error(e) => {
                return Err(Error::custom(format!("archive stream: {}", e)));
            }
        }
    }
}

/// [`ChunkDriver`] over a [`StreamReader`] aimed at one entry. Streams the
/// entry from its start, so a CRC is verified once the whole entry has
/// been read; failures surface as read errors. The reader goes back to
/// the pool when the entry ends or the driver is dropped, except after
/// a decoding or verification failure.
pub(super) struct StreamDriver {
    reader: Option<StreamReader>,
    packed: Range<u64>,
    pool: Arc<ReaderPool>,
    crc: Option<(crc32fast::Hasher, u32)>,
    produced: u64,
    size: u64,
}

impl StreamDriver {
    pub(super) fn new(
        reader: StreamReader,
        packed: Range<u64>,
        pool: Arc<ReaderPool>,
        size: u64,
        expect_crc: Option<u32>,
    ) -> Self {
        Self {
            reader: Some(reader),
            packed,
            pool,
            crc: expect_crc.map(|expected| (crc32fast::Hasher::new(), expected)),
            produced: 0,
            size,
        }
    }

    fn fail(&mut self, message: String) -> Error {
        self.reader = None;
        Error::custom(message)
    }
}

impl ChunkDriver for StreamDriver {
    fn step(&mut self, fetched: Option<(u64, Vec<u8>)>) -> Result<DriveStep, Error> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(DriveStep::Done);
        };
        if let Some((_, data)) = fetched {
            if data.is_empty() {
                reader.signal_eof();
            } else {
                reader.provide_data(&data);
            }
        }
        loop {
            match reader.step() {
                EngineRequest::NeedInput => {
                    let at = self.packed.start + reader.compressed_position();
                    if at >= self.packed.end {
                        reader.signal_eof();
                        continue;
                    }
                    return Ok(DriveStep::Need {
                        offset: at,
                        len: PACKED_SLICE.min(self.packed.end - at),
                    });
                }
                EngineRequest::SeekAndRead { offset, len } => {
                    let at = self.packed.start + offset;
                    if at >= self.packed.end {
                        reader.signal_eof();
                        continue;
                    }
                    return Ok(DriveStep::Need {
                        offset: at,
                        len: (len as u64).min(self.packed.end - at),
                    });
                }
                EngineRequest::OutputReady => {
                    let mut out = Vec::new();
                    let mut buf = [0u8; 64 * 1024];
                    loop {
                        let n = reader.read_output(&mut buf);
                        if n == 0 {
                            break;
                        }
                        out.extend_from_slice(&buf[..n]);
                    }
                    if let Some((hasher, _)) = &mut self.crc {
                        hasher.update(&out);
                    }
                    self.produced += out.len() as u64;
                    return Ok(DriveStep::Output(out));
                }
                EngineRequest::Done => {
                    if self.produced < self.size {
                        let message = format!(
                            "archive entry truncated: {} of {} bytes",
                            self.produced, self.size
                        );
                        return Err(self.fail(message));
                    }
                    if let Some((hasher, expected)) = self.crc.take() {
                        let actual = hasher.finalize();
                        if actual != expected {
                            let message =
                                format!("CRC mismatch: {:08x} != {:08x}", actual, expected);
                            return Err(self.fail(message));
                        }
                    }
                    if let Some(reader) = self.reader.take() {
                        self.pool.park(reader);
                    }
                    return Ok(DriveStep::Done);
                }
                EngineRequest::Error(e) => {
                    return Err(self.fail(format!("archive stream: {}", e)));
                }
            }
        }
    }
}

impl Drop for StreamDriver {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            self.pool.park(reader);
        }
    }
}

/// A streaming read over a background-indexed stream keeps its indexer
/// consumer slot for its whole lifetime, so the indexer outlives the
/// navigation that started it.
pub(super) struct GuardedRead {
    pub(super) inner: PipelinedReader<StreamDriver>,
    pub(super) _guard: ConsumerGuard,
}

impl AsyncRead for GuardedRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
