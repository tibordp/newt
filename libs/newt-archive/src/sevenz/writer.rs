//! Streaming 7z writer, sans-IO like [`crate::ZipWriter`]: entries and file
//! data go in, archive bytes come out through the caller's `Vec<u8>`.
//!
//! Files are packed back to back into solid folders (one LZMA2 stream each,
//! `SOLID_BLOCK` of input per folder), so the bytes stream out as they are
//! compressed and the header follows the last folder. The one thing the
//! format cannot stream is its 32-byte start header: it names the header's
//! offset, size and CRC, which are known only at the end. The writer emits a
//! placeholder for it up front and [`finish`](SevenZWriter::finish) returns
//! the real one for the caller to put at offset 0.
//!
//! With a password every folder is AES-256-CBC encrypted the 7-Zip way (key
//! from 2^19 SHA-256 rounds, a random IV per folder, no salt); the header
//! stays in the clear, as 7-Zip's does without `-mhe`.

use std::io;

use crate::EntryMeta;
use crate::compress::RawLzma2Encoder;
use crate::crypto::AesCbcEncryptor;

use super::folder::{AesParams, derive_key};
use super::header::{
    K_CODERS_UNPACK_SIZE, K_CRC, K_EMPTY_FILE, K_EMPTY_STREAM, K_END, K_FILES_INFO, K_FOLDER,
    K_HEADER, K_MAIN_STREAMS_INFO, K_MTIME, K_NAME, K_NUM_UNPACK_STREAM, K_PACK_INFO, K_SIZE,
    K_SUBSTREAMS_INFO, K_UNPACK_INFO, K_WIN_ATTRIBUTES,
};
use super::{SIGNATURE, SIGNATURE_HEADER_LEN};

/// Input per solid folder before the next one starts. Large enough for a
/// good ratio, small enough that a read deep into an archive we wrote
/// decodes a bounded prefix.
pub const SOLID_BLOCK: u64 = 256 << 20;

/// 7-Zip's key-derivation rounds exponent.
const AES_CYCLES: u8 = 19;

const COPY_ID: &[u8] = &[0x00];
const LZMA2_ID: &[u8] = &[0x21];
const AES_ID: &[u8] = &[0x06, 0xF1, 0x07, 0x01];

const FILE_ATTRIBUTE_READONLY: u32 = 0x01;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
/// The high half holds a unix mode.
const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;

const EPOCH_DELTA_100NS: i64 = 116_444_736_000_000_000;

pub struct SevenZWriter {
    level: i32,
    solid_block: u64,
    /// Password-derived key and its parameters, shared by every folder.
    aes: Option<(AesParams, [u8; 32])>,
    default_mtime_ms: i64,
    /// Bytes emitted so far, the placeholder start header included.
    offset: u64,
    folders: Vec<FolderRecord>,
    open: Option<OpenFolder>,
    /// A file entry begun but with no data yet: it becomes a stream on its
    /// first byte, else an empty file.
    pending: Option<usize>,
    files: Vec<FileRecord>,
    finished: bool,
}

struct FolderRecord {
    coders: Vec<CoderRecord>,
    packed_size: u64,
    /// Output size of each coder, in coder order.
    unpack_sizes: Vec<u64>,
    /// Size and CRC of each entry stored in the folder.
    substreams: Vec<(u64, u32)>,
}

struct CoderRecord {
    id: &'static [u8],
    props: Vec<u8>,
}

/// A folder mid-stream: the entry data flows through the CRC, the coder and
/// the cipher into the caller's output.
struct OpenFolder {
    coders: Vec<CoderRecord>,
    encoder: Encoder,
    cipher: Option<AesCbcEncryptor>,
    /// Bytes taken in so far.
    unpacked: u64,
    /// Coder output so far, before encryption.
    coded: u64,
    packed: u64,
    substreams: Vec<(u64, u32)>,
    current: Option<(u64, crc32fast::Hasher)>,
}

enum Encoder {
    Copy,
    Lzma2(RawLzma2Encoder),
}

struct FileRecord {
    name: String,
    /// Index into the concatenated substreams of all folders, or none for
    /// a directory or empty file.
    has_stream: bool,
    is_dir: bool,
    mtime: u64,
    attributes: u32,
}

impl SevenZWriter {
    /// `level` 0 stores, 1–9 pick the LZMA2 preset (default 6).
    pub fn new(level: Option<i32>, password: Option<&str>) -> io::Result<SevenZWriter> {
        let aes = password
            .map(|pw| {
                let mut iv = [0u8; 16];
                getrandom::fill(&mut iv).map_err(io::Error::other)?;
                let params = AesParams {
                    salt: Vec::new(),
                    iv,
                    cycles: AES_CYCLES,
                };
                let key = derive_key(pw, &params);
                Ok::<_, io::Error>((params, key))
            })
            .transpose()?;
        Ok(SevenZWriter {
            level: level.unwrap_or(6).clamp(0, 9),
            solid_block: SOLID_BLOCK,
            aes,
            default_mtime_ms: crate::now_unix_secs() * 1000,
            offset: 0,
            folders: Vec::new(),
            open: None,
            pending: None,
            files: Vec::new(),
            finished: false,
        })
    }

    /// Input per solid folder, in place of [`SOLID_BLOCK`].
    pub fn solid_block(mut self, bytes: u64) -> Self {
        self.solid_block = bytes.max(1);
        self
    }

    pub fn add_directory(
        &mut self,
        path: &str,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.check_idle()?;
        self.ensure_started(out);
        let mode = 0o040000 | (meta.mode.unwrap_or(0o755) & 0o7777);
        self.files.push(FileRecord {
            name: normalize(path),
            has_stream: false,
            is_dir: true,
            mtime: self.filetime(meta),
            attributes: FILE_ATTRIBUTE_DIRECTORY | unix_attrs(mode),
        });
        Ok(())
    }

    /// A symlink is a file whose content is its target, marked by the
    /// unix mode in its attributes.
    pub fn add_symlink(
        &mut self,
        path: &str,
        target: &str,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        let mode = 0o120000 | (meta.mode.unwrap_or(0o777) & 0o7777);
        self.begin_entry(path, meta, FILE_ATTRIBUTE_ARCHIVE | unix_attrs(mode), out)?;
        self.write_data(target.as_bytes(), out)?;
        self.end_file(out)
    }

    /// Opens a streaming file entry; `size_hint` is only informational.
    pub fn begin_file(
        &mut self,
        path: &str,
        size_hint: Option<u64>,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        let _ = size_hint;
        let mode = 0o100000 | (meta.mode.unwrap_or(0o644) & 0o7777);
        let mut attributes = FILE_ATTRIBUTE_ARCHIVE | unix_attrs(mode);
        if meta.mode.is_some_and(|m| m & 0o200 == 0) {
            attributes |= FILE_ATTRIBUTE_READONLY;
        }
        self.begin_entry(path, meta, attributes, out)
    }

    fn begin_entry(
        &mut self,
        path: &str,
        meta: &EntryMeta,
        attributes: u32,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.check_idle()?;
        self.ensure_started(out);
        self.files.push(FileRecord {
            name: normalize(path),
            has_stream: false,
            is_dir: false,
            mtime: self.filetime(meta),
            attributes,
        });
        self.pending = Some(self.files.len() - 1);
        Ok(())
    }

    pub fn write_data(&mut self, buf: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        let Some(file) = self.pending else {
            return Err(io::Error::other("write_data outside of a file entry"));
        };
        if buf.is_empty() {
            return Ok(());
        }
        if self.open.as_ref().is_none_or(|f| f.current.is_none()) {
            // First byte of the entry: it is a stream after all, in the
            // current folder or a fresh one once the block is full.
            if self
                .open
                .as_ref()
                .is_some_and(|f| f.unpacked >= self.solid_block)
            {
                self.close_folder(out)?;
            }
            if self.open.is_none() {
                self.open = Some(self.open_folder()?);
            }
            self.files[file].has_stream = true;
            self.open.as_mut().unwrap().current = Some((0, crc32fast::Hasher::new()));
        }
        let folder = self.open.as_mut().unwrap();
        let (size, hasher) = folder.current.as_mut().unwrap();
        *size += buf.len() as u64;
        hasher.update(buf);
        folder.unpacked += buf.len() as u64;
        let mut coded = Vec::new();
        match &mut folder.encoder {
            Encoder::Copy => coded.extend_from_slice(buf),
            Encoder::Lzma2(enc) => enc.write(buf, &mut coded)?,
        }
        let n = Self::emit_packed(folder, &coded, out);
        self.offset += n;
        Ok(())
    }

    pub fn end_file(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        let _ = out;
        let Some(_) = self.pending.take() else {
            return Err(io::Error::other("end_file outside of a file entry"));
        };
        if let Some(folder) = self.open.as_mut()
            && let Some((size, hasher)) = folder.current.take()
        {
            folder.substreams.push((size, hasher.finalize()));
        }
        Ok(())
    }

    /// Closes the last folder and writes the header. Returns the start
    /// header, which belongs at offset 0 in place of the placeholder.
    pub fn finish(mut self, out: &mut Vec<u8>) -> io::Result<[u8; 32]> {
        self.check_idle()?;
        self.ensure_started(out);
        self.close_folder(out)?;
        self.finished = true;

        let header = self.header();
        let header_offset = self.offset - SIGNATURE_HEADER_LEN;
        out.extend_from_slice(&header);
        self.offset += header.len() as u64;

        let mut start = [0u8; 32];
        start[..6].copy_from_slice(&SIGNATURE);
        start[6] = 0;
        start[7] = 4;
        start[12..20].copy_from_slice(&header_offset.to_le_bytes());
        start[20..28].copy_from_slice(&(header.len() as u64).to_le_bytes());
        start[28..32].copy_from_slice(&crc32fast::hash(&header).to_le_bytes());
        let start_crc = crc32fast::hash(&start[12..32]);
        start[8..12].copy_from_slice(&start_crc.to_le_bytes());
        Ok(start)
    }

    fn check_idle(&self) -> io::Result<()> {
        if self.finished {
            return Err(io::Error::other("archive already finished"));
        }
        if self.pending.is_some() {
            return Err(io::Error::other("previous file entry not closed"));
        }
        Ok(())
    }

    /// The start header's place is taken before the first packed byte.
    fn ensure_started(&mut self, out: &mut Vec<u8>) {
        if self.offset == 0 {
            out.extend_from_slice(&[0u8; SIGNATURE_HEADER_LEN as usize]);
            self.offset = SIGNATURE_HEADER_LEN;
        }
    }

    fn filetime(&self, meta: &EntryMeta) -> u64 {
        let ms = meta.mtime_ms.unwrap_or(self.default_mtime_ms);
        (ms.saturating_mul(10_000).saturating_add(EPOCH_DELTA_100NS)).max(0) as u64
    }

    fn open_folder(&self) -> io::Result<OpenFolder> {
        let mut coders = Vec::new();
        let encoder = if self.level == 0 {
            if self.aes.is_none() {
                coders.push(CoderRecord {
                    id: COPY_ID,
                    props: Vec::new(),
                });
            }
            Encoder::Copy
        } else {
            let (enc, dict_size) = RawLzma2Encoder::new(self.level as u32)?;
            coders.push(CoderRecord {
                id: LZMA2_ID,
                props: vec![lzma2_dict_prop(dict_size)],
            });
            Encoder::Lzma2(enc)
        };
        let cipher = self.aes.as_ref().map(|(params, key)| {
            let mut props = vec![AES_CYCLES | 0x40, 0x0F];
            props.extend_from_slice(&params.iv);
            coders.push(CoderRecord { id: AES_ID, props });
            AesCbcEncryptor::new(key, &params.iv)
        });
        Ok(OpenFolder {
            coders,
            encoder,
            cipher,
            unpacked: 0,
            coded: 0,
            packed: 0,
            substreams: Vec::new(),
            current: None,
        })
    }

    /// Encrypt (if so) and emit coder output; returns the bytes emitted.
    fn emit_packed(folder: &mut OpenFolder, coded: &[u8], out: &mut Vec<u8>) -> u64 {
        folder.coded += coded.len() as u64;
        let before = out.len();
        match &mut folder.cipher {
            Some(cipher) => cipher.encrypt(coded, out),
            None => out.extend_from_slice(coded),
        }
        let n = (out.len() - before) as u64;
        folder.packed += n;
        n
    }

    fn close_folder(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        let Some(mut folder) = self.open.take() else {
            return Ok(());
        };
        let mut tail = Vec::new();
        if let Encoder::Lzma2(enc) = std::mem::replace(&mut folder.encoder, Encoder::Copy) {
            enc.finish(&mut tail)?;
        }
        let mut n = Self::emit_packed(&mut folder, &tail, out);
        if let Some(cipher) = folder.cipher.take() {
            let before = out.len();
            cipher.finish(out);
            let padded = (out.len() - before) as u64;
            folder.packed += padded;
            n += padded;
        }
        self.offset += n;
        // Coder order is unpacked side first: the cipher, if any, is last.
        let mut unpack_sizes = Vec::new();
        for coder in &folder.coders {
            unpack_sizes.push(if coder.id == AES_ID {
                folder.coded
            } else {
                folder.unpacked
            });
        }
        self.folders.push(FolderRecord {
            coders: folder.coders,
            packed_size: folder.packed,
            unpack_sizes,
            substreams: folder.substreams,
        });
        Ok(())
    }

    fn header(&self) -> Vec<u8> {
        let mut h = vec![K_HEADER];
        if !self.folders.is_empty() {
            h.push(K_MAIN_STREAMS_INFO);
            h.push(K_PACK_INFO);
            number(&mut h, 0);
            number(&mut h, self.folders.len() as u64);
            h.push(K_SIZE);
            for f in &self.folders {
                number(&mut h, f.packed_size);
            }
            h.push(K_END);

            h.push(K_UNPACK_INFO);
            h.push(K_FOLDER);
            number(&mut h, self.folders.len() as u64);
            h.push(0);
            for f in &self.folders {
                number(&mut h, f.coders.len() as u64);
                for c in &f.coders {
                    let mut flags = c.id.len() as u8;
                    if !c.props.is_empty() {
                        flags |= 0x20;
                    }
                    h.push(flags);
                    h.extend_from_slice(c.id);
                    if !c.props.is_empty() {
                        number(&mut h, c.props.len() as u64);
                        h.extend_from_slice(&c.props);
                    }
                }
                // Coder i's input is fed by coder i+1's output; the last
                // coder reads the packed stream.
                for i in 1..f.coders.len() {
                    number(&mut h, i as u64 - 1);
                    number(&mut h, i as u64);
                }
            }
            h.push(K_CODERS_UNPACK_SIZE);
            for f in &self.folders {
                for &size in &f.unpack_sizes {
                    number(&mut h, size);
                }
            }
            h.push(K_END);

            h.push(K_SUBSTREAMS_INFO);
            h.push(K_NUM_UNPACK_STREAM);
            for f in &self.folders {
                number(&mut h, f.substreams.len() as u64);
            }
            h.push(K_SIZE);
            for f in &self.folders {
                for (size, _) in f.substreams.iter().take(f.substreams.len() - 1) {
                    number(&mut h, *size);
                }
            }
            h.push(K_CRC);
            h.push(1);
            for f in &self.folders {
                for (_, crc) in &f.substreams {
                    h.extend_from_slice(&crc.to_le_bytes());
                }
            }
            h.push(K_END);
            h.push(K_END);
        }

        if !self.files.is_empty() {
            h.push(K_FILES_INFO);
            number(&mut h, self.files.len() as u64);

            let empty: Vec<bool> = self.files.iter().map(|f| !f.has_stream).collect();
            if empty.iter().any(|&e| e) {
                let bits = bit_vector(&empty);
                h.push(K_EMPTY_STREAM);
                number(&mut h, bits.len() as u64);
                h.extend_from_slice(&bits);
                let empty_files: Vec<bool> = self
                    .files
                    .iter()
                    .filter(|f| !f.has_stream)
                    .map(|f| !f.is_dir)
                    .collect();
                if empty_files.iter().any(|&e| e) {
                    let bits = bit_vector(&empty_files);
                    h.push(K_EMPTY_FILE);
                    number(&mut h, bits.len() as u64);
                    h.extend_from_slice(&bits);
                }
            }

            let mut names = vec![0u8];
            for f in &self.files {
                for unit in f.name.encode_utf16().chain(std::iter::once(0)) {
                    names.extend_from_slice(&unit.to_le_bytes());
                }
            }
            h.push(K_NAME);
            number(&mut h, names.len() as u64);
            h.extend_from_slice(&names);

            h.push(K_MTIME);
            number(&mut h, 2 + 8 * self.files.len() as u64);
            h.push(1);
            h.push(0);
            for f in &self.files {
                h.extend_from_slice(&f.mtime.to_le_bytes());
            }

            h.push(K_WIN_ATTRIBUTES);
            number(&mut h, 2 + 4 * self.files.len() as u64);
            h.push(1);
            h.push(0);
            for f in &self.files {
                h.extend_from_slice(&f.attributes.to_le_bytes());
            }
            h.push(K_END);
        }
        h.push(K_END);
        h
    }
}

fn unix_attrs(mode: u32) -> u32 {
    FILE_ATTRIBUTE_UNIX_EXTENSION | (mode << 16)
}

/// The LZMA2 dictionary property: the smallest encodable size at least
/// `dict_size`.
fn lzma2_dict_prop(dict_size: u32) -> u8 {
    (0..40u8)
        .find(|&p| ((2 | u64::from(p & 1)) << (u64::from(p) / 2 + 11)) >= u64::from(dict_size))
        .unwrap_or(40)
}

/// 7z's variable-length NUMBER: the leading one-bits of the first byte
/// count the bytes that follow.
fn number(out: &mut Vec<u8>, value: u64) {
    let mut first = 0u8;
    let mut mask = 0x80u8;
    let mut extra = 0;
    for i in 0..8 {
        if value < (1u64 << (7 * (i + 1))) {
            first |= (value >> (8 * i)) as u8;
            break;
        }
        first |= mask;
        mask >>= 1;
        extra = i + 1;
    }
    out.push(first);
    out.extend_from_slice(&value.to_le_bytes()[..extra]);
}

/// Most significant bit of each byte first.
fn bit_vector(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (i, &b) in bits.iter().enumerate() {
        if b {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

/// Archive names are `/`-separated with no leading separator.
fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}
