//! Sans-IO streaming archive writers.
//!
//! Every writer here is a pure state machine: entry metadata and file data go
//! in, archive bytes come out through a caller-provided `Vec<u8>`. There is no
//! IO trait anywhere — the caller owns both ends of the pipe, which is what
//! makes the writers usable against append-only asynchronous sinks (multipart
//! uploads, RPC streams) without temp files or whole-archive buffering. Memory
//! use is O(chunk) for data plus, for zip, O(entries) central-directory
//! bookkeeping.
//!
//! Because all archive bytes flow through the writer, it tracks output offsets
//! itself; nothing ever needs to seek.

mod compress;
mod crypto;
mod tar;
mod zip;

pub use compress::{Compression, Compressor};
pub use tar::TarWriter;
pub use zip::ZipWriter;

/// Per-entry metadata, as much of it as the source filesystem exposes.
/// Writers substitute conventional defaults (0644/0755, uid/gid 0, archive
/// creation time) for absent fields.
#[derive(Debug, Clone, Default)]
pub struct EntryMeta {
    /// Unix permission bits; type bits are ignored.
    pub mode: Option<u32>,
    pub uid: Option<u64>,
    pub gid: Option<u64>,
    pub uname: Option<String>,
    pub gname: Option<String>,
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime_ms: Option<i64>,
}

pub(crate) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
pub(crate) fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
