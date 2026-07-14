use std::io;

use crate::compress::{Compression, Compressor};
use crate::{EntryMeta, now_unix_secs, truncate_str};

const BLOCK: usize = 512;

// ustar header field offsets/lengths.
const NAME: (usize, usize) = (0, 100);
const MODE: (usize, usize) = (100, 8);
const UID: (usize, usize) = (108, 8);
const GID: (usize, usize) = (116, 8);
const SIZE: (usize, usize) = (124, 12);
const MTIME: (usize, usize) = (136, 12);
const CHKSUM: (usize, usize) = (148, 8);
const TYPEFLAG: usize = 156;
const LINKNAME: (usize, usize) = (157, 100);
const MAGIC: (usize, usize) = (257, 8); // "ustar\0" + version "00"
const UNAME: (usize, usize) = (265, 32);
const GNAME: (usize, usize) = (297, 32);
const PREFIX: (usize, usize) = (345, 155);

const TYPE_FILE: u8 = b'0';
const TYPE_SYMLINK: u8 = b'2';
const TYPE_DIRECTORY: u8 = b'5';
const TYPE_PAX: u8 = b'x';

/// Streaming tar (ustar + pax) writer, optionally wrapped in an outer stream
/// compressor. Entry paths must be relative, `/`-separated.
pub struct TarWriter {
    compressor: Compressor,
    state: State,
    default_mtime_secs: i64,
}

enum State {
    Idle,
    InFile { remaining: u64, padding: u64 },
    Finished,
}

impl TarWriter {
    pub fn new(compression: Compression, level: Option<i32>) -> io::Result<Self> {
        Ok(TarWriter {
            compressor: Compressor::new(compression, level)?,
            state: State::Idle,
            default_mtime_secs: now_unix_secs(),
        })
    }

    pub fn add_directory(
        &mut self,
        path: &str,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.check_idle()?;
        let path = if path.ends_with('/') {
            path.to_string()
        } else {
            format!("{path}/")
        };
        self.emit_header(&path, TYPE_DIRECTORY, 0, None, meta, 0o755, out)
    }

    pub fn add_symlink(
        &mut self,
        path: &str,
        target: &str,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.check_idle()?;
        self.emit_header(path, TYPE_SYMLINK, 0, Some(target), meta, 0o777, out)
    }

    /// Declares a file entry of exactly `size` bytes. Data follows via
    /// `write_data`; the declared size is a hard commitment (tar headers
    /// precede data), so `end_file` pads any shortfall with zeros and
    /// `write_data` refuses overshoot.
    pub fn begin_file(
        &mut self,
        path: &str,
        size: u64,
        meta: &EntryMeta,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.check_idle()?;
        self.emit_header(path, TYPE_FILE, size, None, meta, 0o644, out)?;
        self.state = State::InFile {
            remaining: size,
            padding: (BLOCK as u64 - (size % BLOCK as u64)) % BLOCK as u64,
        };
        Ok(())
    }

    /// Returns the number of bytes accepted — less than `buf.len()` once the
    /// declared size is reached (source file grew after scanning).
    pub fn write_data(&mut self, buf: &[u8], out: &mut Vec<u8>) -> io::Result<usize> {
        let State::InFile { remaining, .. } = &mut self.state else {
            return Err(io::Error::other("write_data outside of a file entry"));
        };
        let take = buf
            .len()
            .min(usize::try_from(*remaining).unwrap_or(usize::MAX));
        *remaining -= take as u64;
        self.compressor.write(&buf[..take], out)?;
        Ok(take)
    }

    /// Closes the current file entry, zero-padding up to the declared size and
    /// the 512-byte block boundary. Returns the shortfall that was padded
    /// (non-zero when the source file shrank after scanning).
    pub fn end_file(&mut self, out: &mut Vec<u8>) -> io::Result<u64> {
        let State::InFile { remaining, padding } = self.state else {
            return Err(io::Error::other("end_file outside of a file entry"));
        };
        self.write_zeros(remaining + padding, out)?;
        self.state = State::Idle;
        Ok(remaining)
    }

    pub fn finish(mut self, out: &mut Vec<u8>) -> io::Result<()> {
        self.check_idle()?;
        self.write_zeros(2 * BLOCK as u64, out)?;
        self.state = State::Finished;
        self.compressor.finish(out)
    }

    fn check_idle(&self) -> io::Result<()> {
        match self.state {
            State::Idle => Ok(()),
            State::InFile { .. } => Err(io::Error::other("previous file entry not closed")),
            State::Finished => Err(io::Error::other("archive already finished")),
        }
    }

    fn write_zeros(&mut self, mut count: u64, out: &mut Vec<u8>) -> io::Result<()> {
        let zeros = [0u8; BLOCK];
        while count > 0 {
            let take = count.min(BLOCK as u64) as usize;
            self.compressor.write(&zeros[..take], out)?;
            count -= take as u64;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_header(
        &mut self,
        path: &str,
        typeflag: u8,
        size: u64,
        linkname: Option<&str>,
        meta: &EntryMeta,
        default_mode: u32,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        // (key, value) pax records for whatever the ustar fields can't hold.
        let mut pax: Vec<(&str, String)> = Vec::new();
        let mut hdr = [0u8; BLOCK];

        match split_ustar_name(path) {
            Some((prefix, name)) => {
                hdr[NAME.0..NAME.0 + name.len()].copy_from_slice(name.as_bytes());
                hdr[PREFIX.0..PREFIX.0 + prefix.len()].copy_from_slice(prefix.as_bytes());
            }
            None => {
                pax.push(("path", path.to_string()));
                let name = truncate_str(path, NAME.1);
                hdr[NAME.0..NAME.0 + name.len()].copy_from_slice(name.as_bytes());
            }
        }

        write_octal(
            &mut hdr,
            MODE,
            (meta.mode.unwrap_or(default_mode) & 0o7777) as u64,
        );
        if !write_octal(&mut hdr, UID, meta.uid.unwrap_or(0)) {
            pax.push(("uid", meta.uid.unwrap().to_string()));
            write_octal(&mut hdr, UID, 0);
        }
        if !write_octal(&mut hdr, GID, meta.gid.unwrap_or(0)) {
            pax.push(("gid", meta.gid.unwrap().to_string()));
            write_octal(&mut hdr, GID, 0);
        }
        if !write_octal(&mut hdr, SIZE, size) {
            pax.push(("size", size.to_string()));
            write_octal(&mut hdr, SIZE, 0);
        }

        let mtime_ms = meta.mtime_ms.unwrap_or(self.default_mtime_secs * 1000);
        let mtime_secs = mtime_ms.div_euclid(1000);
        if mtime_secs < 0 || !write_octal(&mut hdr, MTIME, mtime_secs as u64) {
            pax.push(("mtime", pax_mtime(mtime_ms)));
            write_octal(&mut hdr, MTIME, 0);
        }

        hdr[TYPEFLAG] = typeflag;

        if let Some(target) = linkname {
            if target.len() <= LINKNAME.1 {
                hdr[LINKNAME.0..LINKNAME.0 + target.len()].copy_from_slice(target.as_bytes());
            } else {
                pax.push(("linkpath", target.to_string()));
                let short = truncate_str(target, LINKNAME.1);
                hdr[LINKNAME.0..LINKNAME.0 + short.len()].copy_from_slice(short.as_bytes());
            }
        }

        hdr[MAGIC.0..MAGIC.0 + 8].copy_from_slice(b"ustar\x0000");
        if let Some(uname) = &meta.uname {
            let uname = truncate_str(uname, UNAME.1 - 1);
            hdr[UNAME.0..UNAME.0 + uname.len()].copy_from_slice(uname.as_bytes());
        }
        if let Some(gname) = &meta.gname {
            let gname = truncate_str(gname, GNAME.1 - 1);
            hdr[GNAME.0..GNAME.0 + gname.len()].copy_from_slice(gname.as_bytes());
        }

        if !pax.is_empty() {
            // Sub-second precision is worth a record only when a pax header
            // exists anyway (matches GNU tar's default second precision).
            if mtime_ms.rem_euclid(1000) != 0 && !pax.iter().any(|(k, _)| *k == "mtime") {
                pax.push(("mtime", pax_mtime(mtime_ms)));
            }
            self.emit_pax_header(path, &pax, mtime_secs, out)?;
        }

        write_checksum(&mut hdr);
        self.compressor.write(&hdr, out)
    }

    fn emit_pax_header(
        &mut self,
        path: &str,
        records: &[(&str, String)],
        mtime_secs: i64,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        let mut data = Vec::new();
        for (key, value) in records {
            pax_record(key, value, &mut data);
        }

        let mut hdr = [0u8; BLOCK];
        let name = format!("PaxHeaders.0/{path}");
        let name = truncate_str(&name, NAME.1);
        hdr[NAME.0..NAME.0 + name.len()].copy_from_slice(name.as_bytes());
        write_octal(&mut hdr, MODE, 0o644);
        write_octal(&mut hdr, UID, 0);
        write_octal(&mut hdr, GID, 0);
        write_octal(&mut hdr, SIZE, data.len() as u64);
        if mtime_secs < 0 || !write_octal(&mut hdr, MTIME, mtime_secs as u64) {
            write_octal(&mut hdr, MTIME, 0);
        }
        hdr[TYPEFLAG] = TYPE_PAX;
        hdr[MAGIC.0..MAGIC.0 + 8].copy_from_slice(b"ustar\x0000");
        write_checksum(&mut hdr);

        self.compressor.write(&hdr, out)?;
        self.compressor.write(&data, out)?;
        self.write_zeros(
            (BLOCK as u64 - (data.len() as u64 % BLOCK as u64)) % BLOCK as u64,
            out,
        )
    }
}

/// Splits into ustar (prefix, name) if the path fits, preferring pure-name.
fn split_ustar_name(path: &str) -> Option<(&str, &str)> {
    if path.len() <= NAME.1 {
        return Some(("", path));
    }
    for (i, b) in path.bytes().enumerate() {
        if b == b'/' {
            let (prefix, rest) = (&path[..i], &path[i + 1..]);
            if prefix.len() <= PREFIX.1 && !rest.is_empty() && rest.len() <= NAME.1 {
                return Some((prefix, rest));
            }
        }
    }
    None
}

/// Zero-padded NUL-terminated octal; false if the value needs more digits
/// than the field has (caller falls back to a pax record).
fn write_octal(hdr: &mut [u8; BLOCK], (offset, len): (usize, usize), value: u64) -> bool {
    let digits = len - 1;
    if digits < 22 && value > (1u64 << (3 * digits)) - 1 {
        return false;
    }
    let text = format!("{value:0>digits$o}");
    hdr[offset..offset + digits].copy_from_slice(text.as_bytes());
    hdr[offset + digits] = 0;
    true
}

fn write_checksum(hdr: &mut [u8; BLOCK]) {
    hdr[CHKSUM.0..CHKSUM.0 + CHKSUM.1].fill(b' ');
    let sum: u32 = hdr.iter().map(|b| *b as u32).sum();
    let text = format!("{sum:06o}");
    hdr[CHKSUM.0..CHKSUM.0 + 6].copy_from_slice(text.as_bytes());
    hdr[CHKSUM.0 + 6] = 0;
    // Trailing space after the NUL, per historic convention.
}

/// "<len> <key>=<value>\n" where len counts the whole record, itself included.
fn pax_record(key: &str, value: &str, out: &mut Vec<u8>) {
    let base = key.len() + value.len() + 3;
    let mut len = base + 1;
    loop {
        let with_digits = base + len.to_string().len();
        if with_digits == len {
            break;
        }
        len = with_digits;
    }
    out.extend_from_slice(format!("{len} {key}={value}\n").as_bytes());
}

/// Pax mtime value: integer seconds, with millisecond fraction when present.
fn pax_mtime(mtime_ms: i64) -> String {
    let secs = mtime_ms.div_euclid(1000);
    let millis = mtime_ms.rem_euclid(1000);
    if millis == 0 {
        secs.to_string()
    } else {
        format!("{secs}.{millis:03}")
    }
}

#[cfg(test)]
mod tests;
