//! Sans-IO ZIP reading; the streaming writer lives in [`writer`].
//!
//! Same exchange model as `newt-disc`: no IO trait anywhere — each multi-read
//! operation is a state machine that hands the caller absolute byte ranges of
//! the archive and consumes the fetched bytes on the next `step`. The central
//! directory at EOF is a complete index, so probing yields the entire entry
//! table in a handful of bounded reads; entry content is then either a pure
//! extent (stored, unencrypted) or a resumable decrypt→decompress pipeline
//! ([`EntryReader`]) whose state a caller can park and resume for cheap
//! sequential range reads.
//!
//! Malformed archives are user data, never a programming error: everything is
//! a `ZipError`, nothing panics. Entries using features outside the supported
//! scope (PPMd, pre-1993 methods, SES strong encryption, split archives) are
//! still listed with full metadata; the error surfaces only when their
//! content is opened.

// `Step::Need` legitimately carries single-range batches.
#![allow(clippy::single_range_in_vec_init)]

mod cdir;
mod cp437;
mod decompress;
mod decrypt;
mod entry;
mod probe;
pub mod writer;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ops::Range;

pub use entry::{EntryKey, EntryOpenOp, EntryReader, OpenEntry, ReadStep};
pub use probe::{ProbeProgress, ZipProbeOp};
pub use writer::ZipWriter;

/// Compressed input is requested in slices of at most this size; a probe
/// fetches the central directory in slices of the same size.
pub const READ_SLICE: u64 = 1024 * 1024;

/// EOCD record (22 bytes) plus the maximum comment length.
const EOCD_SCAN_WINDOW: u64 = 22 + 65_535;

/// Entry names longer than this are corrupt (the format caps them at u16).
const MAX_NAME_BYTES: usize = 65_535;

/// Upper bound on `step` rounds for a single operation — a backstop against
/// crafted archives that chain structures forever.
const MAX_ROUNDS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// No end-of-central-directory record — this file is not a ZIP archive.
    NotAZip,
    /// Recognized but outside supported scope (PPMd, split archives, …).
    Unsupported(String),
    /// Structurally invalid archive data.
    Corrupt(String),
    /// The entry is encrypted and no password (or key) was provided.
    PasswordRequired,
    /// The provided password failed verification.
    WrongPassword,
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZipError::NotAZip => write!(f, "not a ZIP archive"),
            ZipError::Unsupported(m) => write!(f, "unsupported ZIP feature: {}", m),
            ZipError::Corrupt(m) => write!(f, "corrupt ZIP archive: {}", m),
            ZipError::PasswordRequired => write!(f, "ZIP entry is encrypted"),
            ZipError::WrongPassword => write!(f, "incorrect password for ZIP entry"),
        }
    }
}

impl std::error::Error for ZipError {}

pub type Result<T> = std::result::Result<T, ZipError>;

fn corrupt(msg: impl Into<String>) -> ZipError {
    ZipError::Corrupt(msg.into())
}

/// One fetched byte range, exactly as requested.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub offset: u64,
    pub data: Vec<u8>,
}

/// Progress of a sans-IO operation. `Need` lists absolute archive byte ranges
/// the caller must fetch (they may be fetched concurrently) and feed to the
/// next `step` call; ranges are already validated to lie within the archive.
#[derive(Debug)]
pub enum Step<T> {
    Need(Vec<Range<u64>>),
    Done(T),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// Compression method, with the WinZip AES wrapper (method 99) already
/// unwrapped to the real method it conceals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Stored,
    Deflate,
    Deflate64,
    Bzip2,
    Lzma,
    Zstd,
    Xz,
    /// Anything we don't decode (PPMd, shrink/implode/reduce, …). Listing
    /// works; opening reports the method number.
    Other(u16),
}

impl Method {
    fn from_raw(raw: u16) -> Method {
        match raw {
            0 => Method::Stored,
            8 => Method::Deflate,
            9 => Method::Deflate64,
            12 => Method::Bzip2,
            14 => Method::Lzma,
            93 => Method::Zstd,
            95 => Method::Xz,
            other => Method::Other(other),
        }
    }

    fn describe(&self) -> String {
        match self {
            Method::Other(n) => format!("compression method {}", n),
            m => format!("{:?}", m).to_lowercase(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AesStrength {
    Aes128,
    Aes192,
    Aes256,
}

impl AesStrength {
    pub(crate) fn salt_len(self) -> usize {
        match self {
            AesStrength::Aes128 => 8,
            AesStrength::Aes192 => 12,
            AesStrength::Aes256 => 16,
        }
    }

    pub(crate) fn key_len(self) -> usize {
        match self {
            AesStrength::Aes128 => 16,
            AesStrength::Aes192 => 24,
            AesStrength::Aes256 => 32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    None,
    /// Traditional PKWARE encryption. `check_byte` is the expected value of
    /// the last decrypted header byte: CRC high byte, or the DOS-time high
    /// byte for entries written with a data descriptor (APPNOTE 6.1.6).
    ZipCrypto {
        check_byte: u8,
    },
    /// WinZip AES (AE-1 verifies CRC after decode, AE-2 zeroes the CRC and
    /// relies on the HMAC alone).
    Aes {
        strength: AesStrength,
        ae2: bool,
    },
    /// PKWARE SES ("strong encryption", flag bit 6) and other schemes we
    /// don't decrypt. Listing works; opening fails.
    Unsupported,
}

/// One central-directory entry, fully decoded.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    /// Decoded path, `/`-separated, no leading or trailing separators.
    pub name: String,
    pub kind: EntryKind,
    /// Uncompressed size.
    pub size: u64,
    /// Compressed stream size as recorded — includes crypto framing
    /// (ZipCrypto header / AES salt+verifier+HMAC), which entry opening
    /// subtracts.
    pub compressed_size: u64,
    pub method: Method,
    pub encryption: Encryption,
    pub crc32: u32,
    /// Absolute local-header offset with the base-offset skew applied.
    pub header_offset: u64,
    /// Unix permission bits (no file-type bits), when the entry was made on
    /// a Unix-like host.
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Milliseconds since the Unix epoch.
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
    /// DOS hidden attribute.
    pub hidden: bool,
}

/// A parsed archive: the complete entry table, ready for listing and reads
/// with no further metadata IO.
#[derive(Debug)]
pub struct ZipFs {
    pub entries: Vec<ZipEntry>,
    pub comment: Option<String>,
    /// Bytes prepended before the archive proper (SFX stub, …), already
    /// folded into every `header_offset`.
    pub base_offset: u64,
    pub file_size: u64,
}

// ---------------------------------------------------------------------------
// Fetch bookkeeping
// ---------------------------------------------------------------------------

/// Requested ranges and their fetched bytes. Operations request ranges,
/// remember them, and read them back once supplied; `get` fails if the
/// caller didn't honor the contract.
#[derive(Debug, Default)]
struct Store {
    chunks: HashMap<u64, Vec<u8>>,
}

impl Store {
    fn supply(&mut self, fetched: Vec<Chunk>) {
        for c in fetched {
            self.chunks.insert(c.offset, c.data);
        }
    }

    fn get(&self, range: &Range<u64>) -> Result<&[u8]> {
        let data = self
            .chunks
            .get(&range.start)
            .ok_or_else(|| corrupt(format!("range at {} was not supplied", range.start)))?;
        let len = (range.end - range.start) as usize;
        if data.len() < len {
            return Err(corrupt(format!(
                "short read at {}: got {} of {} bytes",
                range.start,
                data.len(),
                len
            )));
        }
        Ok(&data[..len])
    }

    fn clear(&mut self) {
        self.chunks.clear();
    }
}

// ---------------------------------------------------------------------------
// Byte-level helpers (checked slicing — malformed input must never panic)
// ---------------------------------------------------------------------------

fn slice(buf: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    buf.get(
        off..off
            .checked_add(len)
            .ok_or_else(|| corrupt("offset overflow"))?,
    )
    .ok_or_else(|| corrupt(format!("structure truncated at offset {}", off)))
}

fn u16_le(buf: &[u8], off: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(slice(buf, off, 2)?.try_into().unwrap()))
}

fn u32_le(buf: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(slice(buf, off, 4)?.try_into().unwrap()))
}

fn u64_le(buf: &[u8], off: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(slice(buf, off, 8)?.try_into().unwrap()))
}

/// DOS date/time → Unix epoch milliseconds. DOS stamps carry no timezone;
/// they are interpreted as UTC, and the extended-timestamp / NTFS extra
/// fields override with real UTC when present.
fn dos_datetime_ms(dos_date: u16, dos_time: u16) -> Option<i64> {
    let year = 1980 + i32::from(dos_date >> 9);
    let month = u32::from((dos_date >> 5) & 0x0F);
    let day = u32::from(dos_date & 0x1F);
    let hour = u32::from(dos_time >> 11);
    let min = u32::from((dos_time >> 5) & 0x3F);
    let sec = u32::from(dos_time & 0x1F) * 2;
    epoch_ms(year, month, day, hour, min, sec)
}

/// NTFS FILETIME (100 ns intervals since 1601-01-01 UTC) → epoch ms.
/// Zero means "not set".
fn filetime_ms(ft: u64) -> Option<i64> {
    const EPOCH_DELTA_100NS: i64 = 116_444_736_000_000_000;
    if ft == 0 {
        return None;
    }
    Some((ft as i64 - EPOCH_DELTA_100NS) / 10_000)
}

/// Civil UTC date/time → Unix epoch milliseconds. Returns `None` for
/// out-of-range field values (DOS stamps can encode month 0 or day 0).
fn epoch_ms(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = i64::from(year) - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + i64::from(hour) * 3_600 + i64::from(min) * 60 + i64::from(sec);
    Some(secs * 1_000)
}
