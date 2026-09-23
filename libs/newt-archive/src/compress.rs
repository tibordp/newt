use std::io::{self, Write};

/// Outer stream compression (tar.gz / tar.xz / tar.zst).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Zstd,
}

impl Compression {
    /// Valid level range, or `None` for uncompressed.
    pub fn level_range(self) -> Option<(i32, i32)> {
        match self {
            Compression::None => None,
            Compression::Gzip | Compression::Xz => Some((0, 9)),
            Compression::Zstd => Some((1, 22)),
        }
    }

    pub fn default_level(self) -> i32 {
        match self {
            Compression::None => 0,
            Compression::Gzip | Compression::Xz => 6,
            Compression::Zstd => 3,
        }
    }

    pub fn clamp_level(self, level: Option<i32>) -> i32 {
        let level = level.unwrap_or_else(|| self.default_level());
        match self.level_range() {
            Some((min, max)) => level.clamp(min, max),
            None => 0,
        }
    }
}

/// A chunk-at-a-time compressor. Encoders write into an internal `Vec` that is
/// drained into `out` after every chunk, so buffered memory stays O(chunk).
pub struct Compressor(Inner);

enum Inner {
    None,
    Gzip(flate2::write::GzEncoder<Vec<u8>>),
    Xz(xz2::write::XzEncoder<Vec<u8>>),
    Zstd(zstd::stream::write::Encoder<'static, Vec<u8>>),
}

impl Compressor {
    pub fn new(compression: Compression, level: Option<i32>) -> io::Result<Self> {
        let level = compression.clamp_level(level);
        Ok(Compressor(match compression {
            Compression::None => Inner::None,
            Compression::Gzip => Inner::Gzip(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::new(level as u32),
            )),
            Compression::Xz => Inner::Xz(xz2::write::XzEncoder::new(Vec::new(), level as u32)),
            Compression::Zstd => Inner::Zstd(zstd::stream::write::Encoder::new(Vec::new(), level)?),
        }))
    }

    pub fn write(&mut self, input: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        match &mut self.0 {
            Inner::None => out.extend_from_slice(input),
            Inner::Gzip(enc) => {
                enc.write_all(input)?;
                out.append(enc.get_mut());
            }
            Inner::Xz(enc) => {
                enc.write_all(input)?;
                out.append(enc.get_mut());
            }
            Inner::Zstd(enc) => {
                enc.write_all(input)?;
                out.append(enc.get_mut());
            }
        }
        Ok(())
    }

    pub fn finish(self, out: &mut Vec<u8>) -> io::Result<()> {
        match self.0 {
            Inner::None => {}
            Inner::Gzip(enc) => out.append(&mut enc.finish()?),
            Inner::Xz(enc) => out.append(&mut enc.finish()?),
            Inner::Zstd(enc) => out.append(&mut enc.finish()?),
        }
        Ok(())
    }
}

/// Raw LZMA2 over the bundled liblzma, the stream a 7z LZMA2 coder holds.
/// Pushes input and drains output per call; nothing is buffered across
/// calls beyond the encoder's own state.
pub(crate) struct RawLzma2Encoder {
    strm: lzma_sys::lzma_stream,
}

// The stream's pointers only ever refer to buffers passed into a call.
unsafe impl Send for RawLzma2Encoder {}

impl RawLzma2Encoder {
    /// Returns the encoder and the dictionary size its preset uses.
    pub(crate) fn new(preset: u32) -> io::Result<(Self, u32)> {
        // SAFETY: plain FFI; the option and filter structs are only read
        // during `lzma_raw_encoder`, which copies what it keeps.
        unsafe {
            let mut opts: lzma_sys::lzma_options_lzma = std::mem::zeroed();
            if lzma_sys::lzma_lzma_preset(&mut opts, preset) != 0 {
                return Err(io::Error::other(format!("invalid LZMA2 preset {}", preset)));
            }
            let filters = [
                lzma_sys::lzma_filter {
                    id: lzma_sys::LZMA_FILTER_LZMA2,
                    options: &mut opts as *mut _ as *mut std::ffi::c_void,
                },
                lzma_sys::lzma_filter {
                    id: lzma_sys::LZMA_VLI_UNKNOWN,
                    options: std::ptr::null_mut(),
                },
            ];
            let mut strm: lzma_sys::lzma_stream = std::mem::zeroed();
            if lzma_sys::lzma_raw_encoder(&mut strm, filters.as_ptr()) != lzma_sys::LZMA_OK {
                return Err(io::Error::other("failed to create LZMA2 encoder"));
            }
            Ok((RawLzma2Encoder { strm }, opts.dict_size))
        }
    }

    fn run(
        &mut self,
        mut input: &[u8],
        action: lzma_sys::lzma_action,
        out: &mut Vec<u8>,
    ) -> io::Result<bool> {
        let mut buf = [0u8; 64 * 1024];
        loop {
            self.strm.next_in = input.as_ptr();
            self.strm.avail_in = input.len();
            self.strm.next_out = buf.as_mut_ptr();
            self.strm.avail_out = buf.len();
            // SAFETY: the stream is initialized and both buffers outlive
            // the call.
            let ret = unsafe { lzma_sys::lzma_code(&mut self.strm, action) };
            let consumed = input.len() - self.strm.avail_in;
            let produced = buf.len() - self.strm.avail_out;
            out.extend_from_slice(&buf[..produced]);
            input = &input[consumed..];
            match ret {
                lzma_sys::LZMA_OK => {}
                lzma_sys::LZMA_STREAM_END => return Ok(true),
                code => return Err(io::Error::other(format!("LZMA2 encoder error {}", code))),
            }
            if input.is_empty() && produced < buf.len() {
                return Ok(false);
            }
        }
    }

    pub(crate) fn write(&mut self, input: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        self.run(input, lzma_sys::LZMA_RUN, out).map(|_| ())
    }

    pub(crate) fn finish(mut self, out: &mut Vec<u8>) -> io::Result<()> {
        while !self.run(&[], lzma_sys::LZMA_FINISH, out)? {}
        Ok(())
    }
}

impl Drop for RawLzma2Encoder {
    fn drop(&mut self) {
        // SAFETY: `lzma_end` accepts an initialized stream any number of
        // times.
        unsafe { lzma_sys::lzma_end(&mut self.strm) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn round_trip(compression: Compression, level: Option<i32>) {
        let payload: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let mut compressed = Vec::new();
        let mut enc = Compressor::new(compression, level).unwrap();
        for chunk in payload.chunks(64 * 1024) {
            enc.write(chunk, &mut compressed).unwrap();
        }
        enc.finish(&mut compressed).unwrap();

        let mut decompressed = Vec::new();
        match compression {
            Compression::None => decompressed = compressed.clone(),
            Compression::Gzip => {
                flate2::read::GzDecoder::new(&compressed[..])
                    .read_to_end(&mut decompressed)
                    .unwrap();
            }
            Compression::Xz => {
                xz2::read::XzDecoder::new(&compressed[..])
                    .read_to_end(&mut decompressed)
                    .unwrap();
            }
            Compression::Zstd => {
                decompressed = zstd::stream::decode_all(&compressed[..]).unwrap();
            }
        }
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn round_trips() {
        round_trip(Compression::None, None);
        round_trip(Compression::Gzip, None);
        round_trip(Compression::Xz, Some(1));
        round_trip(Compression::Zstd, Some(19));
    }

    #[test]
    fn level_clamping() {
        assert_eq!(Compression::Gzip.clamp_level(Some(42)), 9);
        assert_eq!(Compression::Zstd.clamp_level(Some(0)), 1);
        assert_eq!(Compression::Zstd.clamp_level(None), 3);
        assert_eq!(Compression::None.clamp_level(Some(5)), 0);
    }
}
