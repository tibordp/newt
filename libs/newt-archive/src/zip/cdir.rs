//! Central-directory record parsing: one record at a time out of a rolling
//! buffer, with the full extra-field and name-encoding treatment.

use super::{
    AesStrength, Encryption, EntryKind, Method, Result, ZipEntry, ZipError, corrupt, cp437,
    dos_datetime_ms, filetime_ms, slice, u16_le, u32_le, u64_le,
};

pub(super) const CD_ENTRY_SIG: u32 = 0x0201_4b50;
/// Digital-signature record — terminates the entry sequence early.
const CD_DIGSIG_SIG: u32 = 0x0505_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;

const FLAG_ENCRYPTED: u16 = 1 << 0;
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
const FLAG_STRONG_ENCRYPTION: u16 = 1 << 6;
const FLAG_UTF8: u16 = 1 << 11;

const HOST_UNIX: u8 = 3;
const HOST_DARWIN: u8 = 19;

const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;

const DOS_ATTR_HIDDEN: u32 = 0x02;
const DOS_ATTR_DIR: u32 = 0x10;

pub(super) enum ParseOutcome {
    /// The buffer holds a complete record: the decoded entry (`None` for
    /// records we skip, e.g. empty names) and the bytes consumed.
    Entry(Option<Box<ZipEntry>>, usize),
    /// The buffer starts mid-record; more central-directory bytes needed.
    NeedMore,
    /// A non-entry signature — the entry sequence has ended.
    End,
}

/// Parse the record at the start of `buf`. `base_offset` is the SFX/prepended
/// data skew, folded into the returned header offset.
pub(super) fn parse_record(buf: &[u8], base_offset: u64) -> Result<ParseOutcome> {
    if buf.len() < 4 {
        return Ok(ParseOutcome::NeedMore);
    }
    match u32_le(buf, 0)? {
        CD_ENTRY_SIG => {}
        CD_DIGSIG_SIG | EOCD64_SIG | EOCD64_LOCATOR_SIG | EOCD_SIG => {
            return Ok(ParseOutcome::End);
        }
        other => {
            return Err(corrupt(format!(
                "expected central directory entry, found signature {:#010x}",
                other
            )));
        }
    }
    if buf.len() < 46 {
        return Ok(ParseOutcome::NeedMore);
    }

    let version_made_by = u16_le(buf, 4)?;
    let flags = u16_le(buf, 8)?;
    let raw_method = u16_le(buf, 10)?;
    let dos_time = u16_le(buf, 12)?;
    let dos_date = u16_le(buf, 14)?;
    let crc32 = u32_le(buf, 16)?;
    let mut compressed_size = u64::from(u32_le(buf, 20)?);
    let mut size = u64::from(u32_le(buf, 24)?);
    let name_len = u16_le(buf, 28)? as usize;
    let extra_len = u16_le(buf, 30)? as usize;
    let comment_len = u16_le(buf, 32)? as usize;
    let mut disk_start = u32::from(u16_le(buf, 34)?);
    let external_attrs = u32_le(buf, 38)?;
    let mut header_offset = u64::from(u32_le(buf, 42)?);

    let total = 46 + name_len + extra_len + comment_len;
    if buf.len() < total {
        return Ok(ParseOutcome::NeedMore);
    }

    let raw_name = slice(buf, 46, name_len)?;
    let extra = slice(buf, 46 + name_len, extra_len)?;

    // --- extra fields ---
    let mut zip64: Option<&[u8]> = None;
    let mut unicode_path: Option<String> = None;
    let mut ext_mtime: Option<i64> = None;
    let mut ntfs_times: Option<(Option<i64>, Option<i64>, Option<i64>)> = None;
    let mut uid = None;
    let mut gid = None;
    let mut aes: Option<(u16, u8, u16)> = None;

    let mut pos = 0;
    // A truncated trailing field ends the walk — extra fields are best-effort
    // decoration on a valid record, mirroring how SUSP areas are treated.
    while pos + 4 <= extra.len() {
        let id = u16_le(extra, pos)?;
        let len = u16_le(extra, pos + 2)? as usize;
        let Ok(data) = slice(extra, pos + 4, len) else {
            break;
        };
        match id {
            0x0001 => zip64 = Some(data),
            0x5455 => {
                // Central-directory variant carries the flags byte and, at
                // most, mtime — atime/ctime live only in the local header.
                if !data.is_empty() && data[0] & 1 != 0 && data.len() >= 5 {
                    let secs = i64::from(u32_le(data, 1)? as i32);
                    ext_mtime = Some(secs * 1_000);
                }
            }
            0x000a => {
                // reserved(4), then tag/size attribute list; tag 1 holds the
                // three FILETIMEs.
                let mut ap = 4;
                while ap + 4 <= data.len() {
                    let tag = u16_le(data, ap)?;
                    let tlen = u16_le(data, ap + 2)? as usize;
                    let Ok(tdata) = slice(data, ap + 4, tlen) else {
                        break;
                    };
                    if tag == 1 && tlen >= 24 {
                        ntfs_times = Some((
                            filetime_ms(u64_le(tdata, 0)?),
                            filetime_ms(u64_le(tdata, 8)?),
                            filetime_ms(u64_le(tdata, 16)?),
                        ));
                    }
                    ap += 4 + tlen;
                }
            }
            0x7875 => {
                // Info-ZIP "new" unix: version(1), uid_size(1), uid,
                // gid_size(1), gid — variable little-endian integers.
                if !data.is_empty() && data[0] == 1 {
                    let mut p = 1;
                    uid = read_varint(data, &mut p);
                    gid = read_varint(data, &mut p);
                }
            }
            0x7075 => {
                // Unicode path: honored only when its CRC over the raw name
                // matches (the standard staleness guard).
                if data.len() >= 5 && data[0] == 1 {
                    let name_crc = u32_le(data, 1)?;
                    if crc32fast::hash(raw_name) == name_crc {
                        unicode_path = Some(String::from_utf8_lossy(&data[5..]).into_owned());
                    }
                }
            }
            0x9901 if data.len() >= 7 => {
                let vendor_version = u16_le(data, 0)?;
                let strength = data[4];
                let actual_method = u16_le(data, 5)?;
                aes = Some((vendor_version, strength, actual_method));
            }
            _ => {}
        }
        pos += 4 + len;
    }

    // --- zip64 ---
    // Fields are present only for saturated 32-bit values, in fixed order.
    if let Some(z) = zip64 {
        let mut p = 0;
        let mut take_u64 = |cond: bool| -> Result<Option<u64>> {
            if !cond {
                return Ok(None);
            }
            let v = u64_le(z, p)?;
            p += 8;
            Ok(Some(v))
        };
        if let Some(v) = take_u64(size == 0xFFFF_FFFF)? {
            size = v;
        }
        if let Some(v) = take_u64(compressed_size == 0xFFFF_FFFF)? {
            compressed_size = v;
        }
        if let Some(v) = take_u64(header_offset == 0xFFFF_FFFF)? {
            header_offset = v;
        }
        if disk_start == 0xFFFF {
            disk_start = u32_le(z, p)?;
        }
    }
    if disk_start != 0 {
        return Err(ZipError::Unsupported(
            "split (multi-disk) archive".to_string(),
        ));
    }

    // --- name ---
    if raw_name.len() > super::MAX_NAME_BYTES {
        return Err(corrupt("entry name too long"));
    }
    let decoded = if let Some(u) = unicode_path {
        u
    } else if flags & FLAG_UTF8 != 0 {
        String::from_utf8_lossy(raw_name).into_owned()
    } else if let Ok(s) = std::str::from_utf8(raw_name) {
        // No UTF-8 flag but valid UTF-8: what Info-ZIP and 7-Zip assume in
        // practice; CP437 is the fallback for actual high-bit legacy names.
        s.to_owned()
    } else {
        cp437::decode(raw_name)
    };
    let name_is_dir = decoded.ends_with('/') || decoded.ends_with('\\');
    // Ancient Windows tools store `\`-separated paths.
    let name = decoded
        .replace('\\', "/")
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string();
    if name.is_empty() {
        return Ok(ParseOutcome::Entry(None, total));
    }

    // --- mode / kind / encryption ---
    let host = (version_made_by >> 8) as u8;
    let unix_mode = external_attrs >> 16;
    let unix_host = matches!(host, HOST_UNIX | HOST_DARWIN) && unix_mode != 0;

    let kind = if name_is_dir
        || (unix_host && unix_mode & S_IFMT == S_IFDIR)
        || external_attrs & DOS_ATTR_DIR != 0
    {
        EntryKind::Dir
    } else if unix_host && unix_mode & S_IFMT == S_IFLNK {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };

    let mut method = Method::from_raw(raw_method);
    let encryption = if flags & FLAG_ENCRYPTED == 0 {
        Encryption::None
    } else if flags & FLAG_STRONG_ENCRYPTION != 0 {
        Encryption::Unsupported
    } else if raw_method == 99 || aes.is_some() {
        match aes {
            Some((vendor_version @ (1 | 2), strength, actual_method)) => {
                let strength = match strength {
                    1 => AesStrength::Aes128,
                    2 => AesStrength::Aes192,
                    3 => AesStrength::Aes256,
                    _ => return Err(corrupt("invalid AES strength")),
                };
                method = Method::from_raw(actual_method);
                Encryption::Aes {
                    strength,
                    ae2: vendor_version == 2,
                }
            }
            _ => Encryption::Unsupported,
        }
    } else {
        Encryption::ZipCrypto {
            // With a data descriptor the CRC wasn't known at write time, so
            // the check byte is the DOS-time high byte instead.
            check_byte: if flags & FLAG_DATA_DESCRIPTOR != 0 {
                (dos_time >> 8) as u8
            } else {
                (crc32 >> 24) as u8
            },
        }
    };

    let (nt_m, nt_a, nt_c) = ntfs_times.unwrap_or((None, None, None));
    let entry = ZipEntry {
        name,
        kind,
        size,
        compressed_size,
        method,
        encryption,
        crc32,
        header_offset: header_offset + base_offset,
        mode: unix_host.then_some(unix_mode & 0o7777),
        uid,
        gid,
        modified: ext_mtime
            .or(nt_m)
            .or_else(|| dos_datetime_ms(dos_date, dos_time)),
        accessed: nt_a,
        created: nt_c,
        hidden: external_attrs & DOS_ATTR_HIDDEN != 0,
    };
    Ok(ParseOutcome::Entry(Some(Box::new(entry)), total))
}

/// Little-endian integer of 1/2/4/8 bytes, clamped to u32 range.
fn read_varint(data: &[u8], pos: &mut usize) -> Option<u32> {
    let len = *data.get(*pos)? as usize;
    let bytes = data.get(*pos + 1..*pos + 1 + len)?;
    *pos += 1 + len;
    let mut v: u64 = 0;
    for (i, &b) in bytes.iter().enumerate().take(8) {
        v |= u64::from(b) << (8 * i);
    }
    Some(u32::try_from(v).unwrap_or(u32::MAX))
}
