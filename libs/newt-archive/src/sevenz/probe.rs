//! Archive recognition: the signature header, the next header (possibly
//! packed and encrypted), and the entry table.

use std::ops::Range;

use super::folder::{FolderInfo, decode_all};
use super::header::{self, FileRecord, Reader, StreamsInfo};
use super::{
    Chunk, EntryKind, Location, MAX_HEADER, MAX_ROUNDS, READ_SLICE, Result, SIGNATURE,
    SIGNATURE_HEADER_LEN, SevenZEntry, SevenZError, SevenZFs, Store, corrupt, filetime_ms,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ProbeProgress {
    pub header_bytes_done: u64,
    pub header_bytes_total: u64,
}

/// Progress of a probe. `NeedPassword` means the header is encrypted:
/// derive a key with [`super::derive_key`] from the parameters in
/// [`SevenZProbeOp::aes_params`], hand it to [`SevenZProbeOp::set_key`] and
/// step again with no chunks.
#[derive(Debug)]
pub enum ProbeStep {
    Need(Vec<Range<u64>>),
    NeedPassword,
    Done(SevenZFs),
}

enum State {
    Start,
    Signature,
    /// Reading a header (top-level or packed) in slices into `buf`.
    Read {
        range: Range<u64>,
        pending: Option<Range<u64>>,
        buf: Vec<u8>,
        /// The streams info describing `buf` when it is a packed header.
        packed: Option<Box<StreamsInfo>>,
    },
    /// A packed header whose folder is encrypted, waiting for a key.
    Locked {
        buf: Vec<u8>,
        streams: Box<StreamsInfo>,
    },
    Finished,
}

pub struct SevenZProbeOp {
    file_size: u64,
    state: State,
    store: Store,
    rounds: usize,
    key: Option<[u8; 32]>,
    aes: Option<super::AesParams>,
    /// CRC the signature header declares for the next header.
    expect_crc: Option<u32>,
    progress: ProbeProgress,
}

impl SevenZProbeOp {
    pub fn new(file_size: u64) -> Self {
        SevenZProbeOp {
            file_size,
            state: State::Start,
            store: Store::default(),
            rounds: 0,
            key: None,
            aes: None,
            expect_crc: None,
            progress: ProbeProgress::default(),
        }
    }

    pub fn progress(&self) -> ProbeProgress {
        self.progress
    }

    /// The header's encryption parameters, once `NeedPassword` was returned.
    pub fn aes_params(&self) -> Option<&super::AesParams> {
        self.aes.as_ref()
    }

    pub fn set_key(&mut self, key: [u8; 32]) {
        self.key = Some(key);
    }

    pub fn step(&mut self, fetched: Vec<Chunk>) -> Result<ProbeStep> {
        self.rounds += 1;
        if self.rounds > MAX_ROUNDS {
            return Err(corrupt("operation exceeded its round budget"));
        }
        self.store.supply(fetched);

        loop {
            match std::mem::replace(&mut self.state, State::Finished) {
                State::Start => {
                    if self.file_size < SIGNATURE_HEADER_LEN {
                        return Err(SevenZError::NotA7z);
                    }
                    self.state = State::Signature;
                    return Ok(ProbeStep::Need(vec![0..SIGNATURE_HEADER_LEN]));
                }
                State::Signature => {
                    let buf = self.store.take(&(0..SIGNATURE_HEADER_LEN))?;
                    if buf[..6] != SIGNATURE {
                        return Err(SevenZError::NotA7z);
                    }
                    if crc32fast::hash(&buf[12..32]) != u32_at(&buf, 8) {
                        return Err(corrupt("start header CRC mismatch"));
                    }
                    let offset = u64_at(&buf, 12);
                    let size = u64_at(&buf, 20);
                    if size == 0 {
                        return Ok(ProbeStep::Done(SevenZFs {
                            entries: Vec::new(),
                            folders: Vec::new(),
                            file_size: self.file_size,
                        }));
                    }
                    let start = SIGNATURE_HEADER_LEN
                        .checked_add(offset)
                        .ok_or_else(|| corrupt("header offset overflow"))?;
                    let end = start
                        .checked_add(size)
                        .ok_or_else(|| corrupt("header size overflow"))?;
                    if size > MAX_HEADER || end > self.file_size {
                        return Err(corrupt("next header outside the archive"));
                    }
                    self.expect_crc = Some(u32_at(&buf, 28));
                    self.begin_read(start..end, None);
                }
                State::Read {
                    range,
                    pending,
                    mut buf,
                    packed,
                } => {
                    if let Some(p) = pending {
                        buf.extend_from_slice(&self.store.take(&p)?);
                    }
                    let next = range.start + buf.len() as u64;
                    self.progress = ProbeProgress {
                        header_bytes_done: buf.len() as u64,
                        header_bytes_total: range.end - range.start,
                    };
                    if next < range.end {
                        let slice = next..(next + READ_SLICE).min(range.end);
                        self.state = State::Read {
                            range,
                            pending: Some(slice.clone()),
                            buf,
                            packed,
                        };
                        return Ok(ProbeStep::Need(vec![slice]));
                    }
                    match packed {
                        None => {
                            if let Some(expected) = self.expect_crc.take()
                                && crc32fast::hash(&buf) != expected
                            {
                                return Err(corrupt("header CRC mismatch"));
                            }
                            if let Some(step) = self.parse(&buf)? {
                                return Ok(step);
                            }
                        }
                        Some(streams) => {
                            if let Some(step) = self.unpack_header(buf, streams)? {
                                return Ok(step);
                            }
                        }
                    }
                }
                State::Locked { buf, streams } => {
                    if self.key.is_none() {
                        self.state = State::Locked { buf, streams };
                        return Ok(ProbeStep::NeedPassword);
                    }
                    if let Some(step) = self.unpack_header(buf, streams)? {
                        return Ok(step);
                    }
                }
                State::Finished => return Err(corrupt("probe stepped after completion")),
            }
        }
    }

    fn begin_read(&mut self, range: Range<u64>, packed: Option<Box<StreamsInfo>>) {
        self.state = State::Read {
            range,
            pending: None,
            buf: Vec::new(),
            packed,
        };
    }

    /// Parse a plaintext header buffer. `None` means the state machine
    /// continues (a packed header must be fetched first).
    fn parse(&mut self, buf: &[u8]) -> Result<Option<ProbeStep>> {
        let mut r = Reader::new(buf);
        match r.byte()? {
            header::K_HEADER => {
                let (streams, files) = header::parse_header(&mut r)?;
                Ok(Some(ProbeStep::Done(self.assemble(streams, files)?)))
            }
            header::K_ENCODED_HEADER => {
                let streams = header::parse_streams_info(&mut r)?;
                if streams.folders.len() != 1 || streams.pack_sizes.len() != 1 {
                    return Err(corrupt("packed header must be a single folder"));
                }
                let start = SIGNATURE_HEADER_LEN
                    .checked_add(streams.pack_pos)
                    .ok_or_else(|| corrupt("pack position overflow"))?;
                let end = start
                    .checked_add(streams.pack_sizes[0])
                    .ok_or_else(|| corrupt("pack size overflow"))?;
                if end > self.file_size || streams.pack_sizes[0] > MAX_HEADER {
                    return Err(corrupt("packed header outside the archive"));
                }
                self.begin_read(start..end, Some(Box::new(streams)));
                Ok(None)
            }
            other => Err(corrupt(format!("header starts with {:#x}", other))),
        }
    }

    /// Decode a packed header and parse what comes out.
    fn unpack_header(
        &mut self,
        buf: Vec<u8>,
        streams: Box<StreamsInfo>,
    ) -> Result<Option<ProbeStep>> {
        let raw = &streams.folders[0];
        let info = FolderInfo::new(raw, &[0..0], 1)?;
        if info.unpacked_len > MAX_HEADER {
            return Err(corrupt("packed header unpacks too large"));
        }
        if info.bcj2.is_some() {
            return Err(corrupt("packed header uses BCJ2"));
        }
        if let Some(params) = &info.aes {
            self.aes = Some(params.clone());
            if self.key.is_none() {
                self.state = State::Locked { buf, streams };
                return Ok(Some(ProbeStep::NeedPassword));
            }
        }
        let spec = info.codec_spec(self.key, None)?;
        let encrypted = info.aes.is_some();
        let decoded = match decode_all(&spec, &buf, info.unpacked_len) {
            Ok(d) => d,
            Err(e) if encrypted => {
                self.state = State::Locked { buf, streams };
                let _ = e;
                return Err(SevenZError::WrongPassword);
            }
            Err(e) => return Err(e),
        };
        if let Some(expected) = info.crc32
            && crc32fast::hash(&decoded) != expected
        {
            if encrypted {
                self.state = State::Locked { buf, streams };
                return Err(SevenZError::WrongPassword);
            }
            return Err(corrupt("packed header CRC mismatch"));
        }
        // Without a folder CRC, a wrong key shows up as a header that
        // does not parse.
        match self.parse(&decoded) {
            Err(SevenZError::Corrupt(_)) if encrypted => {
                self.state = State::Locked { buf, streams };
                Err(SevenZError::WrongPassword)
            }
            other => other,
        }
    }

    fn assemble(&self, streams: StreamsInfo, files: Vec<FileRecord>) -> Result<SevenZFs> {
        // Packed streams sit back to back from pack_pos, each folder
        // taking as many consecutive ones as its graph has inputs.
        let base = SIGNATURE_HEADER_LEN
            .checked_add(streams.pack_pos)
            .ok_or_else(|| corrupt("pack position overflow"))?;
        let mut pack_offsets = Vec::with_capacity(streams.pack_sizes.len());
        let mut pos = base;
        for &size in &streams.pack_sizes {
            pack_offsets.push(pos);
            pos = pos
                .checked_add(size)
                .ok_or_else(|| corrupt("packed streams overflow"))?;
        }
        if pos > self.file_size {
            return Err(corrupt("packed streams outside the archive"));
        }
        let mut folders = Vec::with_capacity(streams.folders.len());
        let mut pack_index = 0;
        for (raw, subs) in streams.folders.iter().zip(&streams.substreams) {
            let count = raw.packed_streams.len();
            let packed = (pack_index..pack_index + count)
                .map(|i| match (pack_offsets.get(i), streams.pack_sizes.get(i)) {
                    (Some(&off), Some(&size)) => Ok(off..off + size),
                    _ => Err(corrupt("folder refers to a missing packed stream")),
                })
                .collect::<Result<Vec<_>>>()?;
            pack_index += count;
            folders.push(FolderInfo::new(raw, &packed, subs.len())?);
        }

        // Entries with data take substreams in order across folders.
        let mut subs = streams
            .substreams
            .iter()
            .enumerate()
            .flat_map(|(folder, list)| {
                let mut offset = 0u64;
                list.iter()
                    .map(move |s| {
                        let loc = (folder, offset, s.size, s.crc32);
                        offset += s.size;
                        loc
                    })
                    .collect::<Vec<_>>()
            });
        let mut entries = Vec::with_capacity(files.len());
        for f in files {
            if f.is_anti {
                continue;
            }
            let (location, size, crc32) = if f.has_stream {
                let (folder, offset, size, crc32) = subs
                    .next()
                    .ok_or_else(|| corrupt("more files than streams"))?;
                // A zero-length stream (some writers store empty files
                // that way) has nothing to locate.
                (
                    (size > 0).then_some(Location { folder, offset }),
                    size,
                    crc32,
                )
            } else {
                (None, 0, None)
            };
            let attrs = f.attributes.unwrap_or(0);
            let unix_mode = (attrs & 0x8000 != 0).then_some(attrs >> 16);
            let is_link = unix_mode.is_some_and(|m| m & 0o170000 == 0o120000);
            let is_dir = f.is_dir
                || attrs & 0x10 != 0
                || unix_mode.is_some_and(|m| m & 0o170000 == 0o040000);
            let kind = if is_link {
                EntryKind::Symlink
            } else if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            let name = normalize_name(&f.name);
            if name.is_empty() {
                continue;
            }
            entries.push(SevenZEntry {
                name,
                kind,
                size,
                crc32,
                location,
                mode: unix_mode.map(|m| m & 0o7777),
                modified: f.mtime.and_then(filetime_ms),
                accessed: f.atime.and_then(filetime_ms),
                created: f.ctime.and_then(filetime_ms),
                hidden: attrs & 0x02 != 0,
                readonly: attrs & 0x01 != 0,
            });
        }
        if subs.next().is_some() {
            return Err(corrupt("more streams than files"));
        }
        Ok(SevenZFs {
            entries,
            folders,
            file_size: self.file_size,
        })
    }
}

/// Both separators are normalized; leading `./` and `/` are stripped.
fn normalize_name(raw: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in raw.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// Decode a small whole stream (a packed header) in memory.
fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn u64_at(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}
