//! Uniform push-based decompression: input slice in, plaintext appended to a
//! caller-owned `Vec`, resumable across arbitrarily-sized input chunks.

use super::{Method, Result, ZipError, corrupt, u16_le};

/// Output produced per `decompress` call is bounded by this; the caller
/// loops while it wants more.
const OUT_STEP: usize = 64 * 1024;

/// LZMA/xz dictionaries are bounded by their props; this limit mirrors what
/// other readers accept.
const LZMA_MEMLIMIT: u64 = 512 * 1024 * 1024;

pub(super) struct Decompressor {
    inner: Inner,
    finished: bool,
}

enum Inner {
    Stored,
    Deflate(flate2::Decompress),
    Deflate64(Box<deflate64::InflaterManaged>),
    Bzip2(bzip2::Decompress),
    Zstd(zstd::stream::raw::Decoder<'static>),
    Xz(xz2::stream::Stream),
    /// ZIP method 14 prepends `version(2) ‖ props_len(2) ‖ props` to a raw
    /// LZMA stream; liblzma's lzma_alone decoder wants `props ‖ size(8)`
    /// instead, so the header is rewritten once it has fully arrived. The
    /// size is always declared unknown: streams written with the EOS flag
    /// carry a terminator that a known-size decoder rejects as trailing
    /// garbage, and without the flag the caller stops at the entry size.
    LzmaHeader {
        buf: Vec<u8>,
    },
    Lzma(xz2::stream::Stream),
}

impl Decompressor {
    pub(super) fn new(method: Method) -> Result<Self> {
        let inner = match method {
            Method::Stored => Inner::Stored,
            Method::Deflate => Inner::Deflate(flate2::Decompress::new(false)),
            Method::Deflate64 => Inner::Deflate64(Box::new(deflate64::InflaterManaged::new())),
            Method::Bzip2 => Inner::Bzip2(bzip2::Decompress::new(false)),
            Method::Zstd => Inner::Zstd(
                zstd::stream::raw::Decoder::new()
                    .map_err(|e| corrupt(format!("zstd init: {}", e)))?,
            ),
            Method::Xz => Inner::Xz(
                xz2::stream::Stream::new_stream_decoder(LZMA_MEMLIMIT, 0)
                    .map_err(|e| corrupt(format!("xz init: {}", e)))?,
            ),
            Method::Lzma => Inner::LzmaHeader { buf: Vec::new() },
            Method::Other(_) => {
                return Err(ZipError::Unsupported(method.describe()));
            }
        };
        Ok(Decompressor {
            inner,
            finished: false,
        })
    }

    /// True once the stream reported its own end marker. Callers also stop
    /// at the entry's uncompressed size — stored data has no marker, and an
    /// LZMA stream written with a known size may omit it.
    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    /// Consume some of `input`, appending up to [`OUT_STEP`] plaintext bytes
    /// to `out`. Returns the number of input bytes consumed. Zero consumed
    /// with nothing appended means the stream needs more input.
    pub(super) fn decompress(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<usize> {
        let start = out.len();
        match &mut self.inner {
            Inner::Stored => {
                let n = input.len().min(OUT_STEP);
                out.extend_from_slice(&input[..n]);
                Ok(n)
            }
            Inner::Deflate(d) => {
                out.resize(start + OUT_STEP, 0);
                let (before_in, before_out) = (d.total_in(), d.total_out());
                let status = d
                    .decompress(input, &mut out[start..], flate2::FlushDecompress::None)
                    .map_err(|e| corrupt(format!("deflate: {}", e)))?;
                out.truncate(start + (d.total_out() - before_out) as usize);
                if status == flate2::Status::StreamEnd {
                    self.finished = true;
                }
                Ok((d.total_in() - before_in) as usize)
            }
            Inner::Deflate64(d) => {
                out.resize(start + OUT_STEP, 0);
                let result = d.inflate(input, &mut out[start..]);
                out.truncate(start + result.bytes_written);
                if result.data_error {
                    return Err(corrupt("deflate64: invalid stream"));
                }
                if d.finished() {
                    self.finished = true;
                }
                Ok(result.bytes_consumed)
            }
            Inner::Bzip2(d) => {
                out.resize(start + OUT_STEP, 0);
                let (before_in, before_out) = (d.total_in(), d.total_out());
                let status = d
                    .decompress(input, &mut out[start..])
                    .map_err(|e| corrupt(format!("bzip2: {}", e)))?;
                out.truncate(start + (d.total_out() - before_out) as usize);
                if status == bzip2::Status::StreamEnd {
                    self.finished = true;
                }
                Ok((d.total_in() - before_in) as usize)
            }
            Inner::Zstd(d) => {
                use zstd::stream::raw::Operation;
                out.resize(start + OUT_STEP, 0);
                let mut in_buf = zstd::stream::raw::InBuffer::around(input);
                let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut out[start..]);
                let hint = d
                    .run(&mut in_buf, &mut out_buf)
                    .map_err(|e| corrupt(format!("zstd: {}", e)))?;
                let (consumed, written) = (in_buf.pos(), out_buf.pos());
                out.truncate(start + written);
                if hint == 0 {
                    self.finished = true;
                }
                Ok(consumed)
            }
            Inner::Xz(s) | Inner::Lzma(s) => {
                out.resize(start + OUT_STEP, 0);
                let (before_in, before_out) = (s.total_in(), s.total_out());
                let status = s
                    .process(input, &mut out[start..], xz2::stream::Action::Run)
                    .map_err(|e| corrupt(format!("lzma: {}", e)))?;
                out.truncate(start + (s.total_out() - before_out) as usize);
                if status == xz2::stream::Status::StreamEnd {
                    self.finished = true;
                }
                Ok((s.total_in() - before_in) as usize)
            }
            Inner::LzmaHeader { buf } => {
                // Consume only header bytes; payload stays with the caller
                // until the decoder exists to receive it.
                let needed = if buf.len() >= 4 {
                    4 + u16_le(buf, 2)? as usize
                } else {
                    4
                };
                let take = (needed - buf.len()).min(input.len());
                buf.extend_from_slice(&input[..take]);
                if buf.len() < 4 {
                    return Ok(take);
                }
                let props_len = u16_le(buf, 2)? as usize;
                if !(5..=256).contains(&props_len) {
                    return Err(corrupt("lzma: invalid properties length"));
                }
                if buf.len() < 4 + props_len {
                    return Ok(take);
                }
                let mut header = buf[4..4 + props_len].to_vec();
                header.extend_from_slice(&u64::MAX.to_le_bytes());
                let mut stream = xz2::stream::Stream::new_lzma_decoder(LZMA_MEMLIMIT)
                    .map_err(|e| corrupt(format!("lzma init: {}", e)))?;
                // liblzma consumes no input when handed zero output space,
                // so the header feed needs a (never-written) sink.
                let mut sink = [0u8; 64];
                let mut fed = 0;
                while fed < header.len() {
                    let before = stream.total_in();
                    stream
                        .process(&header[fed..], &mut sink, xz2::stream::Action::Run)
                        .map_err(|e| corrupt(format!("lzma props: {}", e)))?;
                    let n = (stream.total_in() - before) as usize;
                    if n == 0 {
                        return Err(corrupt("lzma: header not consumed"));
                    }
                    fed += n;
                }
                self.inner = Inner::Lzma(stream);
                Ok(take)
            }
        }
    }
}
