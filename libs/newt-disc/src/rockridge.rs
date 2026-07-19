//! SUSP (IEEE P1281) and Rock Ridge (RRIP, IEEE P1282) system-use entries.

use crate::epoch_ms;

/// Detect SUSP on a root "." record's system-use area. Returns the SP skip
/// length and whether any RRIP entry is present.
pub(crate) fn detect(su: &[u8]) -> (usize, bool) {
    // SP must be the first entry: "SP" len=7 ver=1 0xBE 0xEF skip.
    let skip =
        if su.len() >= 7 && &su[0..2] == b"SP" && su[2] == 7 && su[4] == 0xBE && su[5] == 0xEF {
            usize::from(su[6])
        } else {
            0
        };
    let mut acc = RrAccum::default();
    acc.absorb(su);
    (skip, acc.has_rr)
}

/// Accumulates RRIP state for one directory record across its system-use
/// area and any chain of CE continuation areas.
#[derive(Default)]
pub(crate) struct RrAccum {
    has_rr: bool,
    name: Option<String>,
    name_done: bool,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u32>,
    modified: Option<i64>,
    accessed: Option<i64>,
    created: Option<i64>,
    /// Symlink target under construction.
    sl: Option<String>,
    sl_done: bool,
    /// Set while the last SL component's content continues in the next
    /// component record.
    sl_component_open: bool,
    child_link: Option<u64>,
    relocated: bool,
    /// Pending continuation area: (block, offset, length).
    ce: Option<(u64, u64, u64)>,
}

impl RrAccum {
    pub(crate) fn peek_ce(&self) -> Option<(u64, u64, u64)> {
        self.ce
    }

    pub(crate) fn take_ce(&mut self) -> Option<(u64, u64, u64)> {
        self.ce.take()
    }

    pub(crate) fn name(&self) -> Option<String> {
        self.name.clone().filter(|n| !n.is_empty())
    }

    pub(crate) fn symlink_target(&self) -> Option<String> {
        self.sl.clone().filter(|t| !t.is_empty())
    }

    pub(crate) fn mode(&self) -> Option<u32> {
        self.mode.map(|m| m & 0o7777)
    }

    pub(crate) fn uid(&self) -> Option<u32> {
        self.uid
    }

    pub(crate) fn gid(&self) -> Option<u32> {
        self.gid
    }

    pub(crate) fn nlink(&self) -> Option<u32> {
        self.nlink
    }

    pub(crate) fn times(&self) -> (Option<i64>, Option<i64>, Option<i64>) {
        (self.modified, self.accessed, self.created)
    }

    pub(crate) fn child_link(&self) -> Option<u64> {
        self.child_link
    }

    pub(crate) fn relocated(&self) -> bool {
        self.relocated
    }

    /// Parse one system-use (or continuation) area. Malformed entries end
    /// the area silently — SUSP data is best-effort decoration on top of a
    /// valid ISO record.
    pub(crate) fn absorb(&mut self, area: &[u8]) {
        let mut pos = 0usize;
        while pos + 4 <= area.len() {
            let sig = &area[pos..pos + 2];
            let len = usize::from(area[pos + 2]);
            if len < 4 || pos + len > area.len() {
                return;
            }
            let body = &area[pos..pos + len];
            match sig {
                b"ST" => return,
                b"CE" => {
                    if len >= 28 {
                        let block = u64::from(both_u32(body, 4));
                        let off = u64::from(both_u32(body, 12));
                        let clen = u64::from(both_u32(body, 20));
                        if clen > 0 {
                            self.ce = Some((block, off, clen));
                        }
                    }
                }
                b"RR" | b"PD" | b"SP" | b"ER" | b"ES" | b"PL" | b"SF" => {
                    // Recognized but carrying nothing we surface. RR marks
                    // RRIP presence.
                    if sig == b"RR" {
                        self.has_rr = true;
                    }
                }
                b"PX" => {
                    self.has_rr = true;
                    if len >= 36 {
                        self.mode = Some(both_u32(body, 4));
                        self.nlink = Some(both_u32(body, 12));
                        self.uid = Some(both_u32(body, 20));
                        self.gid = Some(both_u32(body, 28));
                    }
                }
                b"NM" => {
                    self.has_rr = true;
                    if !self.name_done && len >= 5 {
                        let flags = body[4];
                        let name = self.name.get_or_insert_with(String::new);
                        if flags & 0x02 != 0 {
                            name.push('.');
                        } else if flags & 0x04 != 0 {
                            name.push_str("..");
                        } else {
                            name.push_str(&String::from_utf8_lossy(&body[5..]));
                        }
                        if flags & 0x01 == 0 {
                            self.name_done = true;
                        }
                    }
                }
                b"SL" => {
                    self.has_rr = true;
                    if !self.sl_done && len >= 5 {
                        let flags = body[4];
                        self.absorb_sl_components(&body[5..]);
                        if flags & 0x01 == 0 {
                            self.sl_done = true;
                        }
                    }
                }
                b"TF" => {
                    self.has_rr = true;
                    self.absorb_tf(body);
                }
                b"CL" => {
                    self.has_rr = true;
                    if len >= 12 {
                        self.child_link = Some(u64::from(both_u32(body, 4)));
                    }
                }
                b"RE" => {
                    self.has_rr = true;
                    self.relocated = true;
                }
                _ => {}
            }
            pos += len;
        }
    }

    fn absorb_sl_components(&mut self, mut comps: &[u8]) {
        while comps.len() >= 2 {
            let cflags = comps[0];
            let clen = usize::from(comps[1]);
            if 2 + clen > comps.len() {
                return;
            }
            let content = &comps[2..2 + clen];
            let target = self.sl.get_or_insert_with(String::new);
            let continues_prev = std::mem::replace(&mut self.sl_component_open, false);
            if !continues_prev && !target.is_empty() && !target.ends_with('/') {
                target.push('/');
            }
            if cflags & 0x08 != 0 {
                // Root: restart from "/".
                target.clear();
                target.push('/');
            } else if cflags & 0x02 != 0 {
                target.push('.');
            } else if cflags & 0x04 != 0 {
                target.push_str("..");
            } else {
                target.push_str(&String::from_utf8_lossy(content));
                if cflags & 0x01 != 0 {
                    self.sl_component_open = true;
                }
            }
            comps = &comps[2 + clen..];
        }
    }

    fn absorb_tf(&mut self, body: &[u8]) {
        if body.len() < 5 {
            return;
        }
        let flags = body[4];
        let long_form = flags & 0x80 != 0;
        let stamp_len = if long_form { 17 } else { 7 };
        let mut pos = 5usize;
        let mut take = || {
            let s = body.get(pos..pos + stamp_len)?;
            pos += stamp_len;
            if long_form {
                decode_long_datetime(s)
            } else {
                decode_short_datetime(s)
            }
        };
        // Field order: creation, modify, access, attributes, …
        if flags & 0x01 != 0 {
            self.created = take();
        }
        if flags & 0x02 != 0 {
            self.modified = take();
        }
        if flags & 0x04 != 0 {
            self.accessed = take();
        }
    }
}

/// Both-endian u32: value stored LE then BE; read the LE half.
fn both_u32(buf: &[u8], off: usize) -> u32 {
    buf.get(off..off + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .unwrap_or(0)
}

fn decode_short_datetime(b: &[u8]) -> Option<i64> {
    if b.len() < 7 || b[..6] == [0; 6] {
        return None;
    }
    epoch_ms(
        1900 + i32::from(b[0]),
        u32::from(b[1]),
        u32::from(b[2]),
        u32::from(b[3]),
        u32::from(b[4]),
        u32::from(b[5]),
        0,
        i32::from(b[6] as i8) * 15,
    )
}

/// 17-byte "8.4.26.1" ASCII digit form: YYYYMMDDHHMMSScc + tz.
fn decode_long_datetime(b: &[u8]) -> Option<i64> {
    if b.len() < 17 {
        return None;
    }
    let digits = std::str::from_utf8(&b[..16]).ok()?;
    let num = |r: std::ops::Range<usize>| digits.get(r)?.parse::<u32>().ok();
    let year = num(0..4)? as i32;
    if year == 0 {
        return None;
    }
    epoch_ms(
        year,
        num(4..6)?,
        num(6..8)?,
        num(8..10)?,
        num(10..12)?,
        num(12..14)?,
        num(14..16)? * 10,
        i32::from(b[16] as i8) * 15,
    )
}
