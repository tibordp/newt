//! Sans-IO 7z reading. Same exchange model as [`crate::zip`]: the probe hands
//! the caller byte ranges and consumes the fetched bytes on the next `step`.
//!
//! 7z keeps its whole index in a header at the end of the file, so probing
//! yields the complete entry table in a few bounded reads. Content lives in
//! **folders**: one compressed stream per folder holding one or many entries
//! back to back (the "solid" case). A folder's decoding is described by a
//! [`iluvatar::CodecSpec`] chain, and random access inside it is iluvatar's
//! business (`StreamIndexer`/`StreamReader`); this module only tells the
//! caller where each folder's packed bytes are, how to decode them, and where
//! each entry lives in the unpacked stream.
//!
//! BCJ2 folders have four packed streams: the main one flows through the
//! chain as usual, and the three side streams ([`Bcj2Sides`]) are decoded
//! whole by the caller and handed to the chain's BCJ2 stage.
//!
//! Malformed archives are user data, never a programming error: everything is
//! a [`SevenZError`], nothing panics. Folders using coders outside the
//! supported set (PPMd, Deflate64, …) still list with full metadata; the
//! error surfaces when a folder is decoded.

// `ProbeStep::Need` legitimately carries single-range batches.
#![allow(clippy::single_range_in_vec_init)]

mod folder;
mod header;
mod probe;
mod writer;

#[cfg(test)]
mod tests;

use std::ops::Range;

pub use crate::sansio::{Chunk, Step};
pub use folder::{AesParams, Bcj2Sides, FolderInfo, MAX_BCJ2_SIDE, SideStream, derive_key};
pub use probe::{ProbeProgress, ProbeStep, SevenZProbeOp};
pub use writer::{SOLID_BLOCK, SevenZWriter};

pub(super) const SIGNATURE: [u8; 6] = [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
pub(super) const SIGNATURE_HEADER_LEN: u64 = 32;

/// Header and packed-header slices are requested in pieces of at most this.
pub const READ_SLICE: u64 = 1024 * 1024;

/// Larger next-headers are refused: a real one is a few bytes per entry.
const MAX_HEADER: u64 = 1 << 30;

/// Upper bound on `step` rounds for a single operation.
const MAX_ROUNDS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SevenZError {
    /// No signature — this file is not a 7z archive.
    NotA7z,
    /// Recognized but outside supported scope (PPMd, split volumes, …).
    Unsupported(String),
    /// Structurally invalid archive data.
    Corrupt(String),
    /// A folder (or the header) is encrypted and no key was provided.
    PasswordRequired,
    /// The provided password does not decrypt the header.
    WrongPassword,
}

impl std::fmt::Display for SevenZError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SevenZError::NotA7z => write!(f, "not a 7z archive"),
            SevenZError::Unsupported(m) => write!(f, "unsupported 7z feature: {}", m),
            SevenZError::Corrupt(m) => write!(f, "corrupt 7z archive: {}", m),
            SevenZError::PasswordRequired => write!(f, "7z archive is encrypted"),
            SevenZError::WrongPassword => write!(f, "incorrect password for 7z archive"),
        }
    }
}

impl std::error::Error for SevenZError {}

pub type Result<T> = std::result::Result<T, SevenZError>;

fn corrupt(msg: impl Into<String>) -> SevenZError {
    SevenZError::Corrupt(msg.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// Where an entry's bytes sit: `offset` into folder `folder`'s unpacked
/// stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub folder: usize,
    pub offset: u64,
}

/// One archive entry, fully decoded.
#[derive(Debug, Clone)]
pub struct SevenZEntry {
    /// Decoded path, `/`-separated, no leading or trailing separators.
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub crc32: Option<u32>,
    /// `None` for directories and empty files.
    pub location: Option<Location>,
    /// Unix permission bits (no file-type bits), when the archiver stored a
    /// unix mode in the attributes' high half.
    pub mode: Option<u32>,
    /// Milliseconds since the Unix epoch.
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
    /// DOS hidden attribute.
    pub hidden: bool,
    /// DOS read-only attribute.
    pub readonly: bool,
}

/// A parsed archive: the entry table plus one [`FolderInfo`] per folder.
#[derive(Debug)]
pub struct SevenZFs {
    pub entries: Vec<SevenZEntry>,
    pub folders: Vec<FolderInfo>,
    pub file_size: u64,
}

// ---------------------------------------------------------------------------
// Fetch bookkeeping
// ---------------------------------------------------------------------------

/// Requested ranges and their fetched bytes.
#[derive(Debug, Default)]
struct Store {
    chunks: std::collections::HashMap<u64, Vec<u8>>,
}

impl Store {
    fn supply(&mut self, fetched: Vec<Chunk>) {
        for c in fetched {
            self.chunks.insert(c.offset, c.data);
        }
    }

    fn take(&mut self, range: &Range<u64>) -> Result<Vec<u8>> {
        let mut data = self
            .chunks
            .remove(&range.start)
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
        data.truncate(len);
        Ok(data)
    }
}

/// NTFS FILETIME (100 ns intervals since 1601-01-01 UTC) → epoch ms.
fn filetime_ms(ft: u64) -> Option<i64> {
    const EPOCH_DELTA_100NS: i64 = 116_444_736_000_000_000;
    if ft == 0 {
        return None;
    }
    Some((ft as i64 - EPOCH_DELTA_100NS) / 10_000)
}
