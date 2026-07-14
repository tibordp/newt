use std::io::{self, Write};

use crate::crypto::AesCtrEncryptor;
use crate::{EntryMeta, now_unix_secs};

const LFH_SIG: u32 = 0x04034b50;
const DD_SIG: u32 = 0x08074b50;
const CDH_SIG: u32 = 0x02014b50;
const EOCD_SIG: u32 = 0x06054b50;
const EOCD64_SIG: u32 = 0x06064b50;
const EOCD64_LOCATOR_SIG: u32 = 0x07064b50;

const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const METHOD_AES: u16 = 99;

const FLAG_ENCRYPTED: u16 = 1 << 0;
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
const FLAG_UTF8: u16 = 1 << 11;

const VERSION_BASE: u16 = 20;
const VERSION_ZIP64: u16 = 45;
const VERSION_AES: u16 = 51;

const U32_MAX: u64 = 0xFFFF_FFFF;

/// Entries whose predicted size reaches this get zip64 framing. The width of a
/// data descriptor's size fields is committed in the local header, before the
/// data is streamed — so this errs comfortably below 4 GiB, leaving room for
/// deflate worst-case expansion (~0.03%) plus AES overhead without a non-zip64
/// entry ever overflowing a 32-bit compressed size.
const ZIP64_THRESHOLD: u64 = 0xFF00_0000;

/// Streaming zip writer for append-only sinks: entry data is framed with data
/// descriptors (general-purpose bit 3), so nothing is ever patched in place.
/// The central directory is accumulated in memory — O(entries) metadata, not
/// file data — and written by `finish`. Entry paths must be relative,
/// `/`-separated; they are always marked UTF-8 (bit 11).
///
/// With a password, file and symlink entries use WinZip AES-256 (AE-2; AE-1
/// with a real CRC for entries under 20 bytes, per the WinZip recommendation).
pub struct ZipWriter {
    level: i32,
    password: Option<String>,
    default_mtime_secs: i64,
    offset: u64,
    cd: Vec<CdEntry>,
    open: Option<OpenEntry>,
    finished: bool,
}

struct CdEntry {
    name: String,
    flags: u16,
    method: u16,
    version_needed: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
    external_attrs: u32,
    ut_mtime: Option<i32>,
    aes: Option<AesExtra>,
}

#[derive(Clone, Copy)]
struct AesExtra {
    vendor_version: u16, // 1 = AE-1 (real CRC), 2 = AE-2 (CRC zeroed)
    method: u16,         // the actual compression method inside
}

struct OpenEntry {
    zip64: bool,
    deflate: Option<flate2::write::DeflateEncoder<Vec<u8>>>,
    crc: crc32fast::Hasher,
    usize: u64,
    csize: u64,
    encryptor: Option<AesCtrEncryptor>,
}

impl ZipWriter {
    pub fn new(level: Option<i32>, password: Option<&str>) -> ZipWriter {
        ZipWriter {
            level: level.unwrap_or(6).clamp(0, 9),
            password: password.map(str::to_owned),
            default_mtime_secs: now_unix_secs(),
            offset: 0,
            cd: Vec::new(),
            open: None,
            finished: false,
        }
    }

    pub fn add_directory(
        &mut self,
        path: &str,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        let path = if path.ends_with('/') {
            path.to_string()
        } else {
            format!("{path}/")
        };
        // DOS directory bit alongside the unix mode, for non-unix readers.
        let attrs = unix_attrs(0o040000, meta.mode.unwrap_or(0o755)) | 0x10;
        self.add_immediate(path, &[], meta, attrs, false, out)
    }

    pub fn add_symlink(
        &mut self,
        path: &str,
        target: &str,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        let attrs = unix_attrs(0o120000, meta.mode.unwrap_or(0o777));
        self.add_immediate(path.to_string(), target.as_bytes(), meta, attrs, true, out)
    }

    /// Opens a streaming file entry. `size_hint` (the scanned size) picks the
    /// data-descriptor width (zip64 or not) and, under encryption, AE-1 vs
    /// AE-2 — the data itself may then be shorter or longer; only crossing
    /// 4 GiB on a non-zip64 entry is fatal.
    pub fn begin_file(
        &mut self,
        path: &str,
        size_hint: Option<u64>,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.check_idle()?;
        let zip64 = size_hint.is_none_or(|size| size >= ZIP64_THRESHOLD);
        let encrypted = self.password.is_some();
        let inner_method = if self.level == 0 {
            METHOD_STORE
        } else {
            METHOD_DEFLATE
        };
        let aes = encrypted.then_some(AesExtra {
            vendor_version: if size_hint.is_some_and(|size| size < 20) {
                1
            } else {
                2
            },
            method: inner_method,
        });

        let mut entry = CdEntry {
            name: path.to_string(),
            flags: FLAG_UTF8 | FLAG_DATA_DESCRIPTOR | if encrypted { FLAG_ENCRYPTED } else { 0 },
            method: if encrypted { METHOD_AES } else { inner_method },
            version_needed: version_needed(zip64, encrypted),
            dos_time: 0,
            dos_date: 0,
            crc: 0,
            csize: 0,
            usize: 0,
            offset: self.offset,
            external_attrs: unix_attrs(0o100000, meta.mode.unwrap_or(0o644)),
            ut_mtime: None,
            aes,
        };
        self.fill_times(&mut entry, meta);
        self.write_lfh(&entry, zip64, None, out);

        let mut open = OpenEntry {
            zip64,
            deflate: (inner_method == METHOD_DEFLATE).then(|| {
                flate2::write::DeflateEncoder::new(
                    Vec::new(),
                    flate2::Compression::new(self.level as u32),
                )
            }),
            crc: crc32fast::Hasher::new(),
            usize: 0,
            csize: 0,
            encryptor: None,
        };
        if let Some(password) = &self.password {
            let (prelude, encryptor) = AesCtrEncryptor::new(password)?;
            open.csize = prelude.len() as u64;
            self.put(&prelude, out);
            open.encryptor = Some(encryptor);
        }
        self.open = Some(open);
        self.cd.push(entry);
        Ok(())
    }

    pub fn write_data(&mut self, buf: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        let Some(open) = self.open.as_mut() else {
            return Err(io::Error::other("write_data outside of a file entry"));
        };
        open.crc.update(buf);
        open.usize += buf.len() as u64;

        let mut produced = Vec::new();
        match &mut open.deflate {
            Some(encoder) => {
                encoder.write_all(buf)?;
                produced.append(encoder.get_mut());
            }
            None => produced.extend_from_slice(buf),
        }
        self.emit_entry_data(produced, out)
    }

    /// Closes the streaming entry: flushes the compressor and encryption
    /// trailer, then writes the data descriptor with the actual CRC/sizes.
    pub fn end_file(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        let Some(mut open) = self.open.take() else {
            return Err(io::Error::other("end_file outside of a file entry"));
        };
        let tail = match open.deflate.take() {
            Some(encoder) => encoder.finish()?,
            None => Vec::new(),
        };
        self.open = Some(open);
        self.emit_entry_data(tail, out)?;
        let mut open = self.open.take().unwrap();

        if let Some(encryptor) = open.encryptor.take() {
            let auth = encryptor.finish();
            open.csize += auth.len() as u64;
            self.put(&auth, out);
        }

        let entry = self.cd.last_mut().unwrap();
        entry.usize = open.usize;
        entry.csize = open.csize;
        // AE-2 zeroes the CRC; the HMAC authenticates instead.
        entry.crc = match entry.aes {
            Some(AesExtra {
                vendor_version: 2, ..
            }) => 0,
            _ => open.crc.finalize(),
        };

        let mut dd = Vec::new();
        le32(&mut dd, DD_SIG);
        le32(&mut dd, entry.crc);
        if open.zip64 {
            le64(&mut dd, entry.csize);
            le64(&mut dd, entry.usize);
        } else {
            le32(&mut dd, entry.csize as u32);
            le32(&mut dd, entry.usize as u32);
        }
        self.put(&dd, out);
        Ok(())
    }

    /// Writes the central directory and end-of-central-directory records
    /// (zip64 flavors included when thresholds demand them).
    pub fn finish(mut self, out: &mut Vec<u8>) -> io::Result<()> {
        self.check_idle()?;
        self.finished = true;

        let cd_offset = self.offset;
        let entries = std::mem::take(&mut self.cd);
        for entry in &entries {
            let mut rec = Vec::new();
            let mut zip64_extra = Vec::new();
            for value in [entry.usize, entry.csize, entry.offset] {
                if value > U32_MAX {
                    le64(&mut zip64_extra, value);
                }
            }
            let mut extra = Vec::new();
            if !zip64_extra.is_empty() {
                le16(&mut extra, 0x0001);
                le16(&mut extra, zip64_extra.len() as u16);
                extra.extend_from_slice(&zip64_extra);
            }
            push_common_extras(&mut extra, entry);

            le32(&mut rec, CDH_SIG);
            le16(&mut rec, (3 << 8) | entry.version_needed); // made by: unix
            le16(
                &mut rec,
                if zip64_extra.is_empty() {
                    entry.version_needed
                } else {
                    VERSION_ZIP64.max(entry.version_needed)
                },
            );
            le16(&mut rec, entry.flags);
            le16(&mut rec, entry.method);
            le16(&mut rec, entry.dos_time);
            le16(&mut rec, entry.dos_date);
            le32(&mut rec, entry.crc);
            le32(&mut rec, entry.csize.min(U32_MAX) as u32);
            le32(&mut rec, entry.usize.min(U32_MAX) as u32);
            le16(&mut rec, entry.name.len() as u16);
            le16(&mut rec, extra.len() as u16);
            le16(&mut rec, 0); // comment
            le16(&mut rec, 0); // disk number start
            le16(&mut rec, 0); // internal attributes
            le32(&mut rec, entry.external_attrs);
            le32(&mut rec, entry.offset.min(U32_MAX) as u32);
            rec.extend_from_slice(entry.name.as_bytes());
            rec.extend_from_slice(&extra);
            self.put(&rec, out);
        }
        let cd_size = self.offset - cd_offset;

        let needs_zip64 = entries.len() > 0xFFFF || cd_size > U32_MAX || cd_offset > U32_MAX;
        if needs_zip64 {
            let eocd64_offset = self.offset;
            let mut rec = Vec::new();
            le32(&mut rec, EOCD64_SIG);
            le64(&mut rec, 44); // remaining record size
            le16(&mut rec, (3 << 8) | VERSION_ZIP64);
            le16(&mut rec, VERSION_ZIP64);
            le32(&mut rec, 0); // this disk
            le32(&mut rec, 0); // cd disk
            le64(&mut rec, entries.len() as u64);
            le64(&mut rec, entries.len() as u64);
            le64(&mut rec, cd_size);
            le64(&mut rec, cd_offset);
            le32(&mut rec, EOCD64_LOCATOR_SIG);
            le32(&mut rec, 0); // disk with the zip64 EOCD
            le64(&mut rec, eocd64_offset);
            le32(&mut rec, 1); // total disks
            self.put(&rec, out);
        }

        let mut rec = Vec::new();
        le32(&mut rec, EOCD_SIG);
        le16(&mut rec, 0);
        le16(&mut rec, 0);
        le16(&mut rec, entries.len().min(0xFFFF) as u16);
        le16(&mut rec, entries.len().min(0xFFFF) as u16);
        le32(&mut rec, cd_size.min(U32_MAX) as u32);
        le32(&mut rec, cd_offset.min(U32_MAX) as u32);
        le16(&mut rec, 0); // comment
        self.put(&rec, out);
        Ok(())
    }

    /// Directory/symlink entries: data known upfront, exact sizes in the local
    /// header, no data descriptor.
    fn add_immediate(
        &mut self,
        name: String,
        data: &[u8],
        meta: &EntryMeta,
        external_attrs: u32,
        encrypt: bool,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.check_idle()?;
        let encrypted = encrypt && self.password.is_some() && !data.is_empty();
        let aes = encrypted.then_some(AesExtra {
            vendor_version: if data.len() < 20 { 1 } else { 2 },
            method: METHOD_STORE,
        });

        let mut payload = Vec::new();
        let crc = {
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(data);
            hasher.finalize()
        };
        if let Some(password) = &self.password
            && encrypted
        {
            let (prelude, mut encryptor) = AesCtrEncryptor::new(password)?;
            payload.extend_from_slice(&prelude);
            encryptor.encrypt(data, &mut payload);
            payload.extend_from_slice(&encryptor.finish());
        } else {
            payload.extend_from_slice(data);
        }

        let mut entry = CdEntry {
            name,
            flags: FLAG_UTF8 | if encrypted { FLAG_ENCRYPTED } else { 0 },
            method: if encrypted { METHOD_AES } else { METHOD_STORE },
            version_needed: version_needed(false, encrypted),
            dos_time: 0,
            dos_date: 0,
            crc: match aes {
                Some(AesExtra {
                    vendor_version: 2, ..
                }) => 0,
                _ => crc,
            },
            csize: payload.len() as u64,
            usize: data.len() as u64,
            offset: self.offset,
            external_attrs,
            ut_mtime: None,
            aes,
        };
        self.fill_times(&mut entry, meta);
        self.write_lfh(
            &entry,
            false,
            Some((entry.crc, entry.csize, entry.usize)),
            out,
        );
        self.cd.push(entry);
        self.put(&payload, out);
        Ok(())
    }

    fn write_lfh(
        &mut self,
        entry: &CdEntry,
        streaming_zip64: bool,
        exact: Option<(u32, u64, u64)>,
        out: &mut Vec<u8>,
    ) {
        let mut extra = Vec::new();
        if streaming_zip64 {
            // Sizes unknown yet; the extra's presence tells streaming readers
            // the data descriptor uses 64-bit fields.
            le16(&mut extra, 0x0001);
            le16(&mut extra, 16);
            le64(&mut extra, 0);
            le64(&mut extra, 0);
        }
        push_common_extras(&mut extra, entry);

        let (crc, csize, usize) = match exact {
            Some((crc, csize, usize)) => (crc, csize as u32, usize as u32),
            None if streaming_zip64 => (0, U32_MAX as u32, U32_MAX as u32),
            None => (0, 0, 0),
        };

        let mut rec = Vec::new();
        le32(&mut rec, LFH_SIG);
        le16(&mut rec, entry.version_needed);
        le16(&mut rec, entry.flags);
        le16(&mut rec, entry.method);
        le16(&mut rec, entry.dos_time);
        le16(&mut rec, entry.dos_date);
        le32(&mut rec, crc);
        le32(&mut rec, csize);
        le32(&mut rec, usize);
        le16(&mut rec, entry.name.len() as u16);
        le16(&mut rec, extra.len() as u16);
        rec.extend_from_slice(entry.name.as_bytes());
        rec.extend_from_slice(&extra);
        self.put(&rec, out);
    }

    fn emit_entry_data(&mut self, mut produced: Vec<u8>, out: &mut Vec<u8>) -> io::Result<()> {
        let open = self.open.as_mut().unwrap();
        if let Some(encryptor) = open.encryptor.as_mut() {
            let plain = std::mem::take(&mut produced);
            encryptor.encrypt(&plain, &mut produced);
        }
        open.csize += produced.len() as u64;
        if !open.zip64 && (open.usize > U32_MAX || open.csize > U32_MAX) {
            return Err(io::Error::other(
                "file grew past 4 GiB while being archived; the zip entry was not \
                 sized for zip64",
            ));
        }
        self.put(&produced, out);
        Ok(())
    }

    fn fill_times(&mut self, entry: &mut CdEntry, meta: &EntryMeta) {
        let mtime_ms = meta.mtime_ms.unwrap_or(self.default_mtime_secs * 1000);
        let secs = mtime_ms.div_euclid(1000);
        (entry.dos_time, entry.dos_date) = dos_datetime(secs);
        entry.ut_mtime = (secs >= 0 && secs <= i32::MAX as i64).then_some(secs as i32);
    }

    fn check_idle(&self) -> io::Result<()> {
        if self.open.is_some() {
            Err(io::Error::other("previous file entry not closed"))
        } else if self.finished {
            Err(io::Error::other("archive already finished"))
        } else {
            Ok(())
        }
    }

    fn put(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(bytes);
        self.offset += bytes.len() as u64;
    }
}

fn version_needed(zip64: bool, encrypted: bool) -> u16 {
    let mut version = VERSION_BASE;
    if zip64 {
        version = version.max(VERSION_ZIP64);
    }
    if encrypted {
        version = version.max(VERSION_AES);
    }
    version
}

fn unix_attrs(file_type: u32, mode: u32) -> u32 {
    (file_type | (mode & 0o7777)) << 16
}

/// Extras common to local and central headers: extended timestamp (0x5455)
/// and the WinZip AES marker (0x9901).
fn push_common_extras(extra: &mut Vec<u8>, entry: &CdEntry) {
    if let Some(mtime) = entry.ut_mtime {
        le16(extra, 0x5455);
        le16(extra, 5);
        extra.push(0x01); // mtime present
        le32(extra, mtime as u32);
    }
    if let Some(aes) = entry.aes {
        le16(extra, 0x9901);
        le16(extra, 7);
        le16(extra, aes.vendor_version);
        extra.extend_from_slice(b"AE");
        extra.push(3); // AES-256
        le16(extra, aes.method);
    }
}

/// MS-DOS (time, date) from Unix seconds, interpreted as UTC (the convention
/// of most non-Windows writers); clamped to the representable 1980–2107 range.
fn dos_datetime(secs: i64) -> (u16, u16) {
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    if year < 1980 {
        return (0, (1 << 5) | 1); // 1980-01-01 00:00:00
    }
    if year > 2107 {
        return ((23 << 11) | (59 << 5) | 29, (127 << 9) | (12 << 5) | 31);
    }
    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = (((tod / 3600) as u16) << 11)
        | ((((tod / 60) % 60) as u16) << 5)
        | (((tod % 60) / 2) as u16);
    (time, date)
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn le16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn le64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests;
