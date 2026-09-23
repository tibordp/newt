//! The 7z header grammar: property records, streams info (pack streams,
//! folders, substreams) and files info.

use super::folder::{Coder, RawFolder};
use super::{Result, corrupt};

pub(super) const K_END: u8 = 0x00;
pub(super) const K_HEADER: u8 = 0x01;
const K_ARCHIVE_PROPERTIES: u8 = 0x02;
const K_ADDITIONAL_STREAMS_INFO: u8 = 0x03;
pub(super) const K_MAIN_STREAMS_INFO: u8 = 0x04;
pub(super) const K_FILES_INFO: u8 = 0x05;
pub(super) const K_PACK_INFO: u8 = 0x06;
pub(super) const K_UNPACK_INFO: u8 = 0x07;
pub(super) const K_SUBSTREAMS_INFO: u8 = 0x08;
pub(super) const K_SIZE: u8 = 0x09;
pub(super) const K_CRC: u8 = 0x0A;
pub(super) const K_FOLDER: u8 = 0x0B;
pub(super) const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
pub(super) const K_NUM_UNPACK_STREAM: u8 = 0x0D;
pub(super) const K_EMPTY_STREAM: u8 = 0x0E;
pub(super) const K_EMPTY_FILE: u8 = 0x0F;
const K_ANTI: u8 = 0x10;
pub(super) const K_NAME: u8 = 0x11;
const K_CTIME: u8 = 0x12;
const K_ATIME: u8 = 0x13;
pub(super) const K_MTIME: u8 = 0x14;
pub(super) const K_WIN_ATTRIBUTES: u8 = 0x15;
pub(super) const K_ENCODED_HEADER: u8 = 0x17;
const K_DUMMY: u8 = 0x19;

/// Counts above this are not plausible in a header we would read.
const MAX_COUNT: u64 = 1 << 26;

/// A packed stream or substream size.
#[derive(Debug, Clone)]
pub(super) struct SubStream {
    pub size: u64,
    pub crc32: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct StreamsInfo {
    /// Offset of the first packed byte, relative to the end of the
    /// signature header.
    pub pack_pos: u64,
    pub pack_sizes: Vec<u64>,
    pub folders: Vec<RawFolder>,
    /// Per folder, its substreams in order.
    pub substreams: Vec<Vec<SubStream>>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct FileRecord {
    pub name: String,
    pub has_stream: bool,
    pub is_dir: bool,
    pub is_anti: bool,
    pub attributes: Option<u32>,
    pub ctime: Option<u64>,
    pub atime: Option<u64>,
    pub mtime: Option<u64>,
}

pub(super) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(super) fn byte(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| corrupt("header truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| corrupt("header length overflow"))?;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| corrupt("header truncated"))?;
        self.pos = end;
        Ok(s)
    }

    fn u32_le(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn u64_le(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    /// 7z's variable-length integer: the count of leading one bits in the
    /// first byte says how many little-endian bytes follow; the rest of the
    /// first byte is the high part.
    pub(super) fn number(&mut self) -> Result<u64> {
        let first = self.byte()?;
        let mut mask = 0x80u8;
        let mut value = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                let high = u64::from(first & (mask - 1));
                value |= high << (8 * i);
                return Ok(value);
            }
            value |= u64::from(self.byte()?) << (8 * i);
            mask >>= 1;
        }
        Ok(value)
    }

    fn count(&mut self) -> Result<usize> {
        let n = self.number()?;
        if n > MAX_COUNT {
            return Err(corrupt(format!("implausible count {}", n)));
        }
        Ok(n as usize)
    }

    /// `n` bits, most significant bit of each byte first.
    fn bit_vector(&mut self, n: usize) -> Result<Vec<bool>> {
        let bytes = self.bytes(n.div_ceil(8))?;
        Ok((0..n)
            .map(|i| bytes[i / 8] & (0x80 >> (i % 8)) != 0)
            .collect())
    }

    /// An "all defined" byte, else a bit vector.
    fn defined_vector(&mut self, n: usize) -> Result<Vec<bool>> {
        if self.byte()? != 0 {
            Ok(vec![true; n])
        } else {
            self.bit_vector(n)
        }
    }

    fn digests(&mut self, n: usize) -> Result<Vec<Option<u32>>> {
        let defined = self.defined_vector(n)?;
        let mut out = Vec::with_capacity(n);
        for d in defined {
            out.push(if d { Some(self.u32_le()?) } else { None });
        }
        Ok(out)
    }

    fn skip(&mut self, n: u64) -> Result<()> {
        let n = usize::try_from(n).map_err(|_| corrupt("skip length overflow"))?;
        self.bytes(n).map(|_| ())
    }
}

/// Parse a `kHeader` body (the id byte already consumed). Returns the main
/// streams info and the file records.
pub(super) fn parse_header(r: &mut Reader<'_>) -> Result<(StreamsInfo, Vec<FileRecord>)> {
    let mut id = r.byte()?;
    if id == K_ARCHIVE_PROPERTIES {
        loop {
            let ty = r.byte()?;
            if ty == K_END {
                break;
            }
            let size = r.number()?;
            r.skip(size)?;
        }
        id = r.byte()?;
    }
    if id == K_ADDITIONAL_STREAMS_INFO {
        // Additional streams hold external data 7-Zip never writes.
        return Err(super::SevenZError::Unsupported(
            "external (additional) streams".to_string(),
        ));
    }
    let mut streams = StreamsInfo::default();
    if id == K_MAIN_STREAMS_INFO {
        streams = parse_streams_info(r)?;
        id = r.byte()?;
    }
    let mut files = Vec::new();
    if id == K_FILES_INFO {
        files = parse_files_info(r)?;
        id = r.byte()?;
    }
    if id != K_END {
        return Err(corrupt(format!("unexpected property {:#x} in header", id)));
    }
    Ok((streams, files))
}

pub(super) fn parse_streams_info(r: &mut Reader<'_>) -> Result<StreamsInfo> {
    let mut info = StreamsInfo::default();
    let mut id = r.byte()?;
    if id == K_PACK_INFO {
        info.pack_pos = r.number()?;
        let n = r.count()?;
        loop {
            let sub = r.byte()?;
            match sub {
                K_END => break,
                K_SIZE => {
                    info.pack_sizes = (0..n).map(|_| r.number()).collect::<Result<_>>()?;
                }
                K_CRC => {
                    r.digests(n)?;
                }
                other => return Err(corrupt(format!("unexpected {:#x} in pack info", other))),
            }
        }
        if info.pack_sizes.len() != n {
            return Err(corrupt("pack info without sizes"));
        }
        id = r.byte()?;
    }
    if id == K_UNPACK_INFO {
        if r.byte()? != K_FOLDER {
            return Err(corrupt("unpack info without folders"));
        }
        let n = r.count()?;
        if r.byte()? != 0 {
            return Err(super::SevenZError::Unsupported(
                "external folder definitions".to_string(),
            ));
        }
        for _ in 0..n {
            info.folders.push(parse_folder(r)?);
        }
        if r.byte()? != K_CODERS_UNPACK_SIZE {
            return Err(corrupt("folders without unpack sizes"));
        }
        for folder in &mut info.folders {
            folder.unpack_sizes = (0..folder.num_out_streams())
                .map(|_| r.number())
                .collect::<Result<_>>()?;
        }
        loop {
            let sub = r.byte()?;
            match sub {
                K_END => break,
                K_CRC => {
                    let crcs = r.digests(n)?;
                    for (folder, crc) in info.folders.iter_mut().zip(crcs) {
                        folder.crc32 = crc;
                    }
                }
                other => return Err(corrupt(format!("unexpected {:#x} in unpack info", other))),
            }
        }
        id = r.byte()?;
    }
    // Default: one substream per folder covering its whole output.
    let mut counts: Vec<usize> = vec![1; info.folders.len()];
    let mut sizes: Vec<Vec<u64>> = Vec::new();
    let mut explicit_sizes = false;
    let mut digests: Option<Vec<Option<u32>>> = None;
    if id == K_SUBSTREAMS_INFO {
        loop {
            let sub = r.byte()?;
            match sub {
                K_END => break,
                K_NUM_UNPACK_STREAM => {
                    counts = (0..info.folders.len())
                        .map(|_| r.count())
                        .collect::<Result<_>>()?;
                }
                K_SIZE => {
                    explicit_sizes = true;
                    for (folder, &count) in info.folders.iter().zip(&counts) {
                        let mut v = Vec::with_capacity(count);
                        let mut sum = 0u64;
                        for _ in 1..count {
                            let s = r.number()?;
                            sum = sum
                                .checked_add(s)
                                .ok_or_else(|| corrupt("substream size overflow"))?;
                            v.push(s);
                        }
                        if count > 0 {
                            let total = folder.unpack_size()?;
                            v.push(
                                total
                                    .checked_sub(sum)
                                    .ok_or_else(|| corrupt("substreams exceed folder size"))?,
                            );
                        }
                        sizes.push(v);
                    }
                }
                K_CRC => {
                    // Digests only for streams whose CRC the folder does
                    // not already carry.
                    let wanted: usize = info
                        .folders
                        .iter()
                        .zip(&counts)
                        .map(|(f, &c)| if c == 1 && f.crc32.is_some() { 0 } else { c })
                        .sum();
                    digests = Some(r.digests(wanted)?);
                }
                other => {
                    return Err(corrupt(format!(
                        "unexpected {:#x} in substreams info",
                        other
                    )));
                }
            }
        }
        id = r.byte()?;
    }
    if !explicit_sizes {
        for (folder, &count) in info.folders.iter().zip(&counts) {
            sizes.push(match count {
                0 => Vec::new(),
                1 => vec![folder.unpack_size()?],
                _ => return Err(corrupt("multiple substreams without sizes")),
            });
        }
    }
    let mut digest_iter = digests.unwrap_or_default().into_iter();
    for (folder, folder_sizes) in info.folders.iter().zip(sizes) {
        let mut subs = Vec::with_capacity(folder_sizes.len());
        let single = folder_sizes.len() == 1 && folder.crc32.is_some();
        for size in folder_sizes {
            let crc32 = if single {
                folder.crc32
            } else {
                digest_iter.next().flatten()
            };
            subs.push(SubStream { size, crc32 });
        }
        info.substreams.push(subs);
    }
    if id != K_END {
        return Err(corrupt(format!("unexpected {:#x} in streams info", id)));
    }
    Ok(info)
}

fn parse_folder(r: &mut Reader<'_>) -> Result<RawFolder> {
    let num_coders = r.count()?;
    if num_coders == 0 || num_coders > 64 {
        return Err(corrupt("implausible coder count"));
    }
    let mut coders = Vec::with_capacity(num_coders);
    for _ in 0..num_coders {
        let flags = r.byte()?;
        let id_len = usize::from(flags & 0x0F);
        if flags & 0x80 != 0 {
            return Err(corrupt("alternative coder methods are reserved"));
        }
        let id = r.bytes(id_len)?.to_vec();
        let (num_in, num_out) = if flags & 0x10 != 0 {
            (r.count()?, r.count()?)
        } else {
            (1, 1)
        };
        let props = if flags & 0x20 != 0 {
            let n = r.count()?;
            r.bytes(n)?.to_vec()
        } else {
            Vec::new()
        };
        coders.push(Coder {
            id,
            num_in,
            num_out,
            props,
        });
    }
    let total_in: usize = coders.iter().map(|c| c.num_in).sum();
    let total_out: usize = coders.iter().map(|c| c.num_out).sum();
    if total_out == 0 {
        return Err(corrupt("folder without output streams"));
    }
    let num_bind_pairs = total_out - 1;
    let mut bind_pairs = Vec::with_capacity(num_bind_pairs);
    for _ in 0..num_bind_pairs {
        let in_index = r.count()?;
        let out_index = r.count()?;
        if in_index >= total_in || out_index >= total_out {
            return Err(corrupt("bind pair out of range"));
        }
        bind_pairs.push((in_index, out_index));
    }
    let num_packed = total_in
        .checked_sub(num_bind_pairs)
        .ok_or_else(|| corrupt("more bind pairs than input streams"))?;
    let packed_streams = if num_packed == 1 {
        // The one input stream no bind pair feeds.
        let bound: Vec<usize> = bind_pairs.iter().map(|&(i, _)| i).collect();
        vec![
            (0..total_in)
                .find(|i| !bound.contains(i))
                .ok_or_else(|| corrupt("no unbound input stream"))?,
        ]
    } else {
        let mut v = Vec::with_capacity(num_packed);
        for _ in 0..num_packed {
            let i = r.count()?;
            if i >= total_in {
                return Err(corrupt("packed stream index out of range"));
            }
            v.push(i);
        }
        v
    };
    Ok(RawFolder {
        coders,
        bind_pairs,
        packed_streams,
        unpack_sizes: Vec::new(),
        crc32: None,
    })
}

fn parse_files_info(r: &mut Reader<'_>) -> Result<Vec<FileRecord>> {
    let n = r.count()?;
    let mut files = vec![FileRecord::default(); n];
    for f in &mut files {
        f.has_stream = true;
    }
    let mut empty_stream: Vec<bool> = vec![false; n];
    let mut empty_file: Option<Vec<bool>> = None;
    let mut anti: Option<Vec<bool>> = None;
    loop {
        let ty = r.byte()?;
        if ty == K_END {
            break;
        }
        let size = r.number()?;
        match ty {
            K_EMPTY_STREAM => {
                empty_stream = r.bit_vector(n)?;
                for (f, &e) in files.iter_mut().zip(&empty_stream) {
                    f.has_stream = !e;
                }
            }
            K_EMPTY_FILE => {
                let count = empty_stream.iter().filter(|&&e| e).count();
                empty_file = Some(r.bit_vector(count)?);
            }
            K_ANTI => {
                let count = empty_stream.iter().filter(|&&e| e).count();
                anti = Some(r.bit_vector(count)?);
            }
            K_NAME => {
                if r.byte()? != 0 {
                    return Err(super::SevenZError::Unsupported(
                        "external file names".to_string(),
                    ));
                }
                let body = r.bytes(usize::try_from(size).map_err(|_| corrupt("name size"))? - 1)?;
                let mut units = body
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c));
                for f in &mut files {
                    let mut name = Vec::new();
                    loop {
                        match units.next() {
                            Some(0) => break,
                            Some(u) => name.push(u),
                            None => return Err(corrupt("file names truncated")),
                        }
                    }
                    f.name = String::from_utf16_lossy(&name);
                }
            }
            K_CTIME | K_ATIME | K_MTIME => {
                let defined = r.defined_vector(n)?;
                if r.byte()? != 0 {
                    return Err(super::SevenZError::Unsupported(
                        "external timestamps".to_string(),
                    ));
                }
                for (f, d) in files.iter_mut().zip(defined) {
                    if !d {
                        continue;
                    }
                    let t = r.u64_le()?;
                    match ty {
                        K_CTIME => f.ctime = Some(t),
                        K_ATIME => f.atime = Some(t),
                        _ => f.mtime = Some(t),
                    }
                }
            }
            K_WIN_ATTRIBUTES => {
                let defined = r.defined_vector(n)?;
                if r.byte()? != 0 {
                    return Err(super::SevenZError::Unsupported(
                        "external attributes".to_string(),
                    ));
                }
                for (f, d) in files.iter_mut().zip(defined) {
                    if d {
                        f.attributes = Some(r.u32_le()?);
                    }
                }
            }
            K_DUMMY => r.skip(size)?,
            // kStartPos, kComment, and anything newer: skipped by size.
            _ => r.skip(size)?,
        }
    }
    // Empty-stream entries are directories unless flagged as empty files.
    let mut empty_iter = 0;
    for (f, &e) in files.iter_mut().zip(&empty_stream) {
        if e {
            let is_file = empty_file
                .as_ref()
                .is_some_and(|v| v.get(empty_iter) == Some(&true));
            f.is_dir = !is_file;
            f.is_anti = anti
                .as_ref()
                .is_some_and(|v| v.get(empty_iter) == Some(&true));
            empty_iter += 1;
        }
    }
    Ok(files)
}
