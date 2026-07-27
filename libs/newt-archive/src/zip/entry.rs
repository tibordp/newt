//! Opening an entry and reading its content: local-header resolution,
//! password verification, and the resumable decrypt→decompress pipeline.

use std::collections::VecDeque;
use std::ops::Range;

use super::decompress::Decompressor;
use super::decrypt::{self, AES_AUTH_LEN, AES_VERIFIER_LEN, Decryptor, ZIPCRYPTO_HEADER_LEN};
use super::{
    Chunk, Encryption, Method, READ_SLICE, Result, Step, ZipEntry, ZipError, corrupt, u16_le,
    u32_le,
};

pub use super::decrypt::EntryKey;

const LOCAL_HEADER_SIG: u32 = 0x0403_4b50;
const LOCAL_HEADER_LEN: u64 = 30;

/// Output is buffered inside the reader up to this before `step` reports
/// `Output` and stops driving the decompressor.
const OUT_HIGH_WATER: usize = 256 * 1024;

/// Consumed input is compacted out of the buffer past this point.
const IN_COMPACT: usize = 512 * 1024;

// ---------------------------------------------------------------------------
// EntryOpenOp — local-header resolution
// ---------------------------------------------------------------------------

/// Resolves where an entry's payload actually starts. The central directory
/// records the local-header offset, but the local header's own name/extra
/// lengths legitimately differ from the central copy, so one small read is
/// unavoidable. For encrypted entries the crypto prelude (ZipCrypto header /
/// AES salt+verifier) is fetched in the same pass.
pub struct EntryOpenOp {
    header_offset: u64,
    compressed_size: u64,
    encryption: Encryption,
    state: OpenState,
}

enum OpenState {
    Start,
    Header,
    Prelude { data_start: u64 },
    Finished,
}

/// A resolved entry: absolute payload bounds plus the crypto prelude.
#[derive(Debug, Clone)]
pub struct OpenEntry {
    /// Start of the compressed payload, past any crypto prelude.
    pub data_offset: u64,
    /// Payload length, crypto framing excluded.
    pub data_len: u64,
    prelude: Prelude,
}

#[derive(Debug, Clone)]
enum Prelude {
    None,
    ZipCrypto([u8; ZIPCRYPTO_HEADER_LEN]),
    Aes {
        salt: Vec<u8>,
        verifier: [u8; AES_VERIFIER_LEN],
    },
}

/// Crypto framing around the payload: (before, after) byte counts.
fn framing(encryption: &Encryption) -> (u64, u64) {
    match encryption {
        Encryption::None | Encryption::Unsupported => (0, 0),
        Encryption::ZipCrypto { .. } => (ZIPCRYPTO_HEADER_LEN as u64, 0),
        Encryption::Aes { strength, .. } => (
            strength.salt_len() as u64 + AES_VERIFIER_LEN as u64,
            AES_AUTH_LEN as u64,
        ),
    }
}

impl EntryOpenOp {
    pub fn new(entry: &ZipEntry, file_size: u64) -> Result<Self> {
        if let Encryption::Unsupported = entry.encryption {
            return Err(ZipError::Unsupported("strong encryption (SES)".to_string()));
        }
        if entry.header_offset + LOCAL_HEADER_LEN > file_size {
            return Err(corrupt("local header outside the archive"));
        }
        Ok(EntryOpenOp {
            header_offset: entry.header_offset,
            compressed_size: entry.compressed_size,
            encryption: entry.encryption,
            state: OpenState::Start,
        })
    }

    pub fn step(&mut self, fetched: Vec<Chunk>) -> Result<Step<OpenEntry>> {
        match std::mem::replace(&mut self.state, OpenState::Finished) {
            OpenState::Start => {
                self.state = OpenState::Header;
                Ok(Step::Need(vec![
                    self.header_offset..self.header_offset + LOCAL_HEADER_LEN,
                ]))
            }
            OpenState::Header => {
                let chunk = fetched
                    .into_iter()
                    .find(|c| c.offset == self.header_offset)
                    .ok_or_else(|| corrupt("local header range was not supplied"))?;
                let buf = &chunk.data;
                if buf.len() < LOCAL_HEADER_LEN as usize {
                    return Err(corrupt("short read at local header"));
                }
                if u32_le(buf, 0)? != LOCAL_HEADER_SIG {
                    return Err(corrupt("bad local header signature"));
                }
                let name_len = u64::from(u16_le(buf, 26)?);
                let extra_len = u64::from(u16_le(buf, 28)?);
                let data_start = self.header_offset + LOCAL_HEADER_LEN + name_len + extra_len;

                let (head, tail) = framing(&self.encryption);
                if self.compressed_size < head + tail {
                    return Err(corrupt("compressed size smaller than crypto framing"));
                }
                if head == 0 {
                    return Ok(Step::Done(OpenEntry {
                        data_offset: data_start,
                        data_len: self.compressed_size,
                        prelude: Prelude::None,
                    }));
                }
                self.state = OpenState::Prelude { data_start };
                Ok(Step::Need(vec![data_start..data_start + head]))
            }
            OpenState::Prelude { data_start } => {
                let chunk = fetched
                    .into_iter()
                    .find(|c| c.offset == data_start)
                    .ok_or_else(|| corrupt("crypto prelude range was not supplied"))?;
                let (head, tail) = framing(&self.encryption);
                let buf = &chunk.data;
                if (buf.len() as u64) < head {
                    return Err(corrupt("short read at crypto prelude"));
                }
                let prelude = match &self.encryption {
                    Encryption::ZipCrypto { .. } => {
                        Prelude::ZipCrypto(buf[..ZIPCRYPTO_HEADER_LEN].try_into().unwrap())
                    }
                    Encryption::Aes { strength, .. } => {
                        let salt_len = strength.salt_len();
                        Prelude::Aes {
                            salt: buf[..salt_len].to_vec(),
                            verifier: buf[salt_len..salt_len + AES_VERIFIER_LEN]
                                .try_into()
                                .unwrap(),
                        }
                    }
                    _ => unreachable!("prelude requested without encryption"),
                };
                Ok(Step::Done(OpenEntry {
                    data_offset: data_start + head,
                    data_len: self.compressed_size - head - tail,
                    prelude,
                }))
            }
            OpenState::Finished => Err(corrupt("open stepped after completion")),
        }
    }
}

impl OpenEntry {
    pub fn needs_password(&self) -> bool {
        !matches!(self.prelude, Prelude::None)
    }

    /// Check `password` against the entry's cheap verifier and derive the
    /// stream key. `WrongPassword` on mismatch.
    pub fn verify_password(&self, entry: &ZipEntry, password: &[u8]) -> Result<EntryKey> {
        match (&self.prelude, &entry.encryption) {
            (Prelude::ZipCrypto(header), Encryption::ZipCrypto { check_byte }) => {
                decrypt::verify_zipcrypto(password, header, *check_byte)
                    .ok_or(ZipError::WrongPassword)
            }
            (Prelude::Aes { salt, verifier }, Encryption::Aes { strength, .. }) => {
                decrypt::derive_aes_key(password, salt, verifier, *strength)
                    .ok_or(ZipError::WrongPassword)
            }
            _ => Err(corrupt("password check on an unencrypted entry")),
        }
    }

    /// For stored, unencrypted entries the payload *is* the plaintext:
    /// callers can read the returned absolute range directly and skip the
    /// reader entirely.
    pub fn plain_extent(&self, entry: &ZipEntry) -> Option<Range<u64>> {
        (entry.method == Method::Stored && matches!(self.prelude, Prelude::None))
            .then(|| self.data_offset..self.data_offset + self.data_len)
    }
}

// ---------------------------------------------------------------------------
// EntryReader — resumable decrypt→decompress pipeline
// ---------------------------------------------------------------------------

/// What a `step` produced. `Output` means bytes are waiting in the internal
/// buffer (drain with [`EntryReader::take_output`]); the reader stops
/// driving decompression while more than [`OUT_HIGH_WATER`] is buffered.
#[derive(Debug)]
pub enum ReadStep {
    Need(Range<u64>),
    Output,
    Done,
}

/// Streams one entry's plaintext from a starting offset, requesting
/// sequential compressed slices. The reader owns live decompressor and
/// decryptor state; callers park it between uses and resume with
/// [`EntryReader::seek_forward`] — sequential range reads then cost only the
/// new compressed bytes, not a restart.
///
/// Integrity: when the stream runs from offset 0 the CRC32 is checked at the
/// end (AE-1 included), and a WinZip AES stream additionally fetches and
/// checks the trailing HMAC (AE-2's only integrity signal). Seeking forfeits
/// verification — a range read can't see the whole stream by construction.
pub struct EntryReader {
    // Entry facts.
    data: Range<u64>,
    size: u64,
    crc32: u32,
    verify_crc: bool,
    // Pipeline.
    decompressor: Decompressor,
    decryptor: Option<Decryptor>,
    // Input side.
    comp_next: u64,
    pending: Option<Range<u64>>,
    in_buf: Vec<u8>,
    in_pos: usize,
    // Output side.
    out_buf: VecDeque<u8>,
    /// Plaintext position of the front of `out_buf`.
    take_pos: u64,
    /// Total plaintext produced by the decompressor.
    produced: u64,
    /// Output below this offset is discarded instead of buffered.
    skip_until: u64,
    crc: Option<crc32fast::Hasher>,
    state: ReadState,
}

enum ReadState {
    Streaming,
    /// Payload finished; waiting for the AES auth tail.
    Auth {
        range: Range<u64>,
    },
    Done,
}

impl EntryReader {
    /// `key` is required iff the entry is encrypted (obtain it via
    /// [`OpenEntry::verify_password`]).
    pub fn new(
        entry: &ZipEntry,
        open: &OpenEntry,
        key: Option<&EntryKey>,
        target: u64,
    ) -> Result<EntryReader> {
        let mut decryptor = match (&open.prelude, key) {
            (Prelude::None, _) => None,
            (_, None) => return Err(ZipError::PasswordRequired),
            (_, Some(key)) => Some(Decryptor::new(key)),
        };
        let decompressor = Decompressor::new(entry.method)?;

        let mut comp_next = open.data_offset;
        let mut produced = 0;
        // Stored data is byte-addressable, so a non-zero target can skip
        // compressed bytes outright — except under ZipCrypto, whose cipher
        // state depends on every preceding byte. AES-CTR seeks in O(1).
        if entry.method == Method::Stored && target > 0 {
            let jump = target.min(open.data_len);
            match &mut decryptor {
                None => {
                    comp_next += jump;
                    produced = jump;
                }
                Some(Decryptor::Aes(ctr)) => {
                    ctr.seek_to(jump);
                    comp_next += jump;
                    produced = jump;
                }
                Some(Decryptor::ZipCrypto(_)) => {}
            }
        }

        let verify = produced == 0 && target == 0;
        Ok(EntryReader {
            data: open.data_offset..open.data_offset + open.data_len,
            size: entry.size,
            crc32: entry.crc32,
            verify_crc: verify && !matches!(entry.encryption, Encryption::Aes { ae2: true, .. }),
            decompressor,
            decryptor,
            comp_next,
            pending: None,
            in_buf: Vec::new(),
            in_pos: 0,
            out_buf: VecDeque::new(),
            take_pos: target,
            produced,
            skip_until: target,
            crc: verify.then(crc32fast::Hasher::new),
            state: ReadState::Streaming,
        })
    }

    /// The plaintext offset the next `take_output` byte corresponds to.
    pub fn position(&self) -> u64 {
        self.take_pos
    }

    /// Bytes immediately available to `take_output`.
    pub fn buffered(&self) -> usize {
        self.out_buf.len()
    }

    /// Advance to `target`, discarding buffered or yet-to-be-produced output
    /// before it. `target` must not be behind [`Self::position`].
    pub fn seek_forward(&mut self, target: u64) -> Result<()> {
        if target < self.take_pos {
            return Err(corrupt("seek_forward went backward"));
        }
        let in_buffer = (target - self.take_pos).min(self.out_buf.len() as u64);
        self.out_buf.drain(..in_buffer as usize);
        self.take_pos += in_buffer;
        if target > self.take_pos {
            self.out_buf.clear();
            self.skip_until = target;
            self.take_pos = target;
            self.crc = None;
            self.verify_crc = false;
        }
        Ok(())
    }

    pub fn take_output(&mut self, max: usize) -> Vec<u8> {
        let n = max.min(self.out_buf.len());
        let out: Vec<u8> = self.out_buf.drain(..n).collect();
        self.take_pos += out.len() as u64;
        out
    }

    /// Drive the pipeline. Pass the chunk for the previously returned
    /// `Need` range (or `None` on the first call / after draining output).
    pub fn step(&mut self, fetched: Option<Chunk>) -> Result<ReadStep> {
        if let Some(chunk) = fetched {
            self.accept(chunk)?;
        }
        loop {
            match &self.state {
                // Buffered output survives completion: `Done` is only
                // reported once the caller has drained it.
                ReadState::Done => {
                    return Ok(if self.out_buf.is_empty() {
                        ReadStep::Done
                    } else {
                        ReadStep::Output
                    });
                }
                ReadState::Auth { range } => {
                    let range = range.clone();
                    if let Some(auth) = self.try_finish_auth(&range)? {
                        return Ok(auth);
                    }
                    self.pending = Some(range.clone());
                    return Ok(ReadStep::Need(range));
                }
                ReadState::Streaming => {}
            }
            if self.out_buf.len() >= OUT_HIGH_WATER {
                return Ok(ReadStep::Output);
            }

            let mut fresh = Vec::new();
            let consumed = self
                .decompressor
                .decompress(&self.in_buf[self.in_pos..], &mut fresh)?;
            self.in_pos += consumed;
            if self.in_pos >= IN_COMPACT {
                self.in_buf.drain(..self.in_pos);
                self.in_pos = 0;
            }

            if !fresh.is_empty() {
                // Cap at the declared size — padding beyond it is not data.
                let room = (self.size - self.produced).min(fresh.len() as u64) as usize;
                fresh.truncate(room);
                if let Some(crc) = &mut self.crc {
                    crc.update(&fresh);
                }
                let fresh_start = self.produced;
                self.produced += fresh.len() as u64;
                let keep_from = self
                    .skip_until
                    .saturating_sub(fresh_start)
                    .min(fresh.len() as u64);
                self.out_buf
                    .extend(fresh[keep_from as usize..].iter().copied());
            }

            if self.produced >= self.size || self.decompressor.finished() {
                if self.produced < self.size {
                    return Err(corrupt(format!(
                        "entry truncated: {} of {} bytes",
                        self.produced, self.size
                    )));
                }
                self.finish_streaming()?;
                continue;
            }

            if !fresh.is_empty() {
                continue;
            }
            if consumed == 0 && self.in_pos == self.in_buf.len() {
                if self.comp_next >= self.data.end {
                    return Err(corrupt("compressed stream ended prematurely"));
                }
                let range = self.comp_next..(self.comp_next + READ_SLICE).min(self.data.end);
                self.pending = Some(range.clone());
                return Ok(ReadStep::Need(range));
            }
        }
    }

    fn accept(&mut self, chunk: Chunk) -> Result<()> {
        let Some(range) = self.pending.take() else {
            return Err(corrupt("chunk supplied without a pending request"));
        };
        if chunk.offset != range.start || (chunk.data.len() as u64) < range.end - range.start {
            return Err(corrupt(format!(
                "range at {} was not supplied",
                range.start
            )));
        }
        let mut data = chunk.data;
        data.truncate((range.end - range.start) as usize);
        if let ReadState::Auth { .. } = self.state {
            // The auth tail is raw; stash it via the input buffer.
            self.in_buf = data;
            self.in_pos = 0;
            return Ok(());
        }
        if let Some(d) = &mut self.decryptor {
            d.decrypt(&mut data);
        }
        self.comp_next = range.end;
        self.in_buf.extend_from_slice(&data);
        Ok(())
    }

    /// Payload complete: run CRC verification and decide whether an AES auth
    /// read is still owed.
    fn finish_streaming(&mut self) -> Result<()> {
        if self.verify_crc
            && let Some(crc) = self.crc.take()
        {
            let actual = crc.finalize();
            if actual != self.crc32 {
                return Err(corrupt(format!(
                    "CRC mismatch: {:08x} != {:08x}",
                    actual, self.crc32
                )));
            }
        }
        let wants_auth = matches!(
            self.decryptor,
            Some(Decryptor::Aes(ref ctr)) if ctr.has_auth()
        );
        if wants_auth {
            self.in_buf.clear();
            self.in_pos = 0;
            self.state = ReadState::Auth {
                range: self.data.end..self.data.end + AES_AUTH_LEN as u64,
            };
        } else {
            self.state = ReadState::Done;
        }
        Ok(())
    }

    /// `None` when the tail hasn't been supplied yet.
    fn try_finish_auth(&mut self, range: &Range<u64>) -> Result<Option<ReadStep>> {
        if self.in_buf.len() < (range.end - range.start) as usize {
            return Ok(None);
        }
        let Some(Decryptor::Aes(ctr)) = &mut self.decryptor else {
            return Err(corrupt("auth state without an AES stream"));
        };
        let expected = ctr.take_auth().ok_or_else(|| corrupt("auth taken twice"))?;
        if self.in_buf[..AES_AUTH_LEN] != expected {
            return Err(corrupt("AES authentication failed"));
        }
        self.state = ReadState::Done;
        Ok(Some(ReadStep::Done))
    }
}
