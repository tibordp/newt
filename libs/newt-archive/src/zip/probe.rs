//! Archive recognition: EOCD scan, zip64, base-offset skew, and the chunked
//! central-directory read.

use std::collections::HashMap;
use std::ops::Range;

use super::cdir::{self, ParseOutcome};
use super::{
    Chunk, EOCD_SCAN_WINDOW, MAX_ROUNDS, READ_SLICE, Result, Step, Store, ZipEntry, ZipError,
    ZipFs, corrupt, cp437, u16_le, u32_le, u64_le,
};

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;

/// How far before the zip64 locator we scan for the EOCD64 record when the
/// locator's offset doesn't hold one (prepended-data skew): the minimal
/// 56-byte record plus room for v2 extensible data.
const EOCD64_BACKSCAN: u64 = 56 + 4096;

#[derive(Debug, Clone, Copy, Default)]
pub struct ProbeProgress {
    pub cd_bytes_done: u64,
    pub cd_bytes_total: u64,
    pub entries: u64,
}

/// Central-directory location, before the skew is resolved.
#[derive(Debug, Clone, Copy)]
struct CdLocation {
    cd_offset: u64,
    cd_size: u64,
    /// File position where the record pointing at the CD begins (EOCD, or
    /// EOCD64 once resolved) — the CD must end here, which is what pins the
    /// base offset.
    record_pos: u64,
}

enum State {
    Start,
    /// Scanning the fetched tail window.
    Tail {
        window: Range<u64>,
    },
    /// EOCD referenced a zip64 locator that fell outside the tail window
    /// (only possible with a near-maximal comment).
    Locator {
        eocd_pos: u64,
        cd: CdLocation,
    },
    /// Waiting for the EOCD64 candidate ranges.
    Eocd64 {
        pointed: Option<Range<u64>>,
        backscan: Range<u64>,
    },
    /// Reading the central directory; `pending` is the slice requested by
    /// the previous step (`None` right after the location was resolved).
    ReadCd {
        pending: Option<Range<u64>>,
    },
    Finished,
}

pub struct ZipProbeOp {
    file_size: u64,
    state: State,
    store: Store,
    rounds: usize,
    comment: Option<String>,
    base_offset: u64,
    /// Absolute CD bounds once the skew is known.
    cd_range: Range<u64>,
    cd_next: u64,
    carry: Vec<u8>,
    entries: Vec<ZipEntry>,
    by_name: HashMap<String, usize>,
}

impl ZipProbeOp {
    pub fn new(file_size: u64) -> Self {
        ZipProbeOp {
            file_size,
            state: State::Start,
            store: Store::default(),
            rounds: 0,
            comment: None,
            base_offset: 0,
            cd_range: 0..0,
            cd_next: 0,
            carry: Vec::new(),
            entries: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn progress(&self) -> ProbeProgress {
        ProbeProgress {
            cd_bytes_done: self.cd_next - self.cd_range.start,
            cd_bytes_total: self.cd_range.end - self.cd_range.start,
            entries: self.entries.len() as u64,
        }
    }

    pub fn step(&mut self, fetched: Vec<Chunk>) -> Result<Step<ZipFs>> {
        self.rounds += 1;
        if self.rounds > MAX_ROUNDS {
            return Err(corrupt("operation exceeded its round budget"));
        }
        self.store.supply(fetched);

        loop {
            match std::mem::replace(&mut self.state, State::Finished) {
                State::Start => {
                    if self.file_size < 22 {
                        return Err(ZipError::NotAZip);
                    }
                    let window = self.file_size.saturating_sub(EOCD_SCAN_WINDOW)..self.file_size;
                    self.state = State::Tail {
                        window: window.clone(),
                    };
                    return Ok(Step::Need(vec![window]));
                }
                State::Tail { window } => {
                    let buf = self.store.get(&window)?.to_vec();
                    let (eocd_rel, cd) = self.parse_eocd(&buf, window.start)?;
                    let eocd_pos = window.start + eocd_rel as u64;

                    // A zip64 locator immediately precedes the EOCD when
                    // present. It is almost always inside the tail window;
                    // fetch it separately in the pathological comment case.
                    if eocd_pos >= 20 {
                        let loc_pos = eocd_pos - 20;
                        if loc_pos >= window.start {
                            let rel = (loc_pos - window.start) as usize;
                            let loc = &buf[rel..rel + 20];
                            if u32_le(loc, 0)? == EOCD64_LOCATOR_SIG {
                                self.store.clear();
                                return self.request_eocd64(loc, loc_pos, cd);
                            }
                        } else {
                            self.state = State::Locator { eocd_pos, cd };
                            return Ok(Step::Need(vec![loc_pos..eocd_pos]));
                        }
                    }
                    if cd.needs_zip64() {
                        return Err(corrupt("zip64 sizes without a zip64 locator"));
                    }
                    self.store.clear();
                    self.begin_cd(cd)?;
                }
                State::Locator { eocd_pos, cd } => {
                    let range = eocd_pos - 20..eocd_pos;
                    let loc = self.store.get(&range)?.to_vec();
                    if u32_le(&loc, 0)? == EOCD64_LOCATOR_SIG {
                        let loc_pos = eocd_pos - 20;
                        self.store.clear();
                        return self.request_eocd64(&loc, loc_pos, cd);
                    }
                    if cd.needs_zip64() {
                        return Err(corrupt("zip64 sizes without a zip64 locator"));
                    }
                    self.store.clear();
                    self.begin_cd(cd)?;
                }
                State::Eocd64 { pointed, backscan } => {
                    let cd = self.parse_eocd64(pointed, backscan)?;
                    self.store.clear();
                    self.begin_cd(cd)?;
                }
                State::ReadCd { pending } => {
                    if let Some(range) = pending {
                        let data = self.store.get(&range)?.to_vec();
                        self.store.clear();
                        self.cd_next = range.end;
                        self.carry.extend_from_slice(&data);
                        if !self.parse_cd_records()? {
                            // Digital-signature record: entries ended early.
                            self.cd_next = self.cd_range.end;
                        }
                    }
                    if self.cd_next < self.cd_range.end {
                        let slice_end = (self.cd_next + READ_SLICE).min(self.cd_range.end);
                        let range = self.cd_next..slice_end;
                        self.state = State::ReadCd {
                            pending: Some(range.clone()),
                        };
                        return Ok(Step::Need(vec![range]));
                    }
                    if !self.carry.is_empty() {
                        return Err(corrupt("central directory ends mid-record"));
                    }
                    return Ok(Step::Done(self.finish()));
                }
                State::Finished => return Err(corrupt("probe stepped after completion")),
            }
        }
    }

    /// Backward-scan the tail window for the EOCD. A signature whose comment
    /// length reaches exactly to EOF wins; failing that, the last plausible
    /// signature is accepted and trailing bytes are treated as junk.
    fn parse_eocd(&mut self, buf: &[u8], window_start: u64) -> Result<(usize, CdLocation)> {
        let mut fallback: Option<usize> = None;
        if buf.len() >= 22 {
            for pos in (0..=buf.len() - 22).rev() {
                if u32_le(buf, pos)? != EOCD_SIG {
                    continue;
                }
                let comment_len = u16_le(buf, pos + 20)? as usize;
                if pos + 22 + comment_len == buf.len() {
                    return self.decode_eocd(buf, pos, window_start);
                }
                if fallback.is_none() && pos + 22 + comment_len <= buf.len() {
                    fallback = Some(pos);
                }
            }
        }
        match fallback {
            Some(pos) => self.decode_eocd(buf, pos, window_start),
            None => Err(ZipError::NotAZip),
        }
    }

    fn decode_eocd(
        &mut self,
        buf: &[u8],
        pos: usize,
        window_start: u64,
    ) -> Result<(usize, CdLocation)> {
        let disk = u16_le(buf, pos + 4)?;
        let cd_disk = u16_le(buf, pos + 6)?;
        if disk != cd_disk || (disk != 0 && disk != 0xFFFF) {
            return Err(ZipError::Unsupported(
                "split (multi-disk) archive".to_string(),
            ));
        }
        let cd_size = u64::from(u32_le(buf, pos + 12)?);
        let cd_offset = u64::from(u32_le(buf, pos + 16)?);
        let comment_len = u16_le(buf, pos + 20)? as usize;
        let comment = super::slice(buf, pos + 22, comment_len)?;
        if !comment.is_empty() {
            self.comment = Some(match std::str::from_utf8(comment) {
                Ok(s) => s.to_owned(),
                Err(_) => cp437::decode(comment),
            });
        }
        Ok((
            pos,
            CdLocation {
                cd_offset,
                cd_size,
                record_pos: window_start + pos as u64,
            },
        ))
    }

    /// Request both EOCD64 candidates in one batch: where the locator points,
    /// and a back-scan window ending at the locator (which is where the
    /// record actually sits when prepended data skews recorded offsets).
    fn request_eocd64(&mut self, loc: &[u8], loc_pos: u64, _cd: CdLocation) -> Result<Step<ZipFs>> {
        let total_disks = u32_le(loc, 16)?;
        if total_disks > 1 {
            return Err(ZipError::Unsupported(
                "split (multi-disk) archive".to_string(),
            ));
        }
        let pointed_off = u64_le(loc, 8)?;
        let pointed = (pointed_off + 56 <= loc_pos).then(|| pointed_off..pointed_off + 56);
        let backscan = loc_pos.saturating_sub(EOCD64_BACKSCAN)..loc_pos;
        let mut needs: Vec<Range<u64>> = Vec::new();
        if let Some(p) = &pointed {
            // The back-scan window usually contains the pointed range; only
            // request it separately when it doesn't.
            if !(backscan.start <= p.start && p.end <= backscan.end) {
                needs.push(p.clone());
            }
        }
        needs.push(backscan.clone());
        self.state = State::Eocd64 { pointed, backscan };
        Ok(Step::Need(needs))
    }

    fn parse_eocd64(
        &mut self,
        pointed: Option<Range<u64>>,
        backscan: Range<u64>,
    ) -> Result<CdLocation> {
        let back = self.store.get(&backscan)?.to_vec();

        // Prefer the locator-pointed position (the common, unskewed case).
        if let Some(p) = pointed {
            let rec = if backscan.start <= p.start && p.end <= backscan.end {
                let rel = (p.start - backscan.start) as usize;
                back[rel..rel + 56].to_vec()
            } else {
                self.store.get(&p)?.to_vec()
            };
            if u32_le(&rec, 0)? == EOCD64_SIG {
                return decode_eocd64(&rec, p.start);
            }
        }
        // Skewed archive: scan backward from the locator for the record.
        if back.len() >= 56 {
            for pos in (0..=back.len() - 56).rev() {
                if u32_le(&back, pos)? == EOCD64_SIG {
                    return decode_eocd64(&back[pos..pos + 56], backscan.start + pos as u64);
                }
            }
        }
        Err(corrupt("zip64 locator points at no EOCD64 record"))
    }

    /// Resolve the base offset and start (or skip) the CD read.
    fn begin_cd(&mut self, cd: CdLocation) -> Result<()> {
        let recorded_end = cd
            .cd_offset
            .checked_add(cd.cd_size)
            .ok_or_else(|| corrupt("central directory size overflow"))?;
        if recorded_end > cd.record_pos {
            return Err(corrupt("central directory overlaps its end record"));
        }
        // The CD ends exactly where the record that points at it begins;
        // anything before the recorded offsets is prepended data (SFX stub).
        self.base_offset = cd.record_pos - recorded_end;
        self.cd_range = cd.cd_offset + self.base_offset..cd.record_pos;
        self.cd_next = self.cd_range.start;
        self.state = State::ReadCd { pending: None };
        Ok(())
    }

    /// Parse whole records out of the carry buffer. Returns false when the
    /// entry sequence terminated early (digital-signature record).
    fn parse_cd_records(&mut self) -> Result<bool> {
        let mut pos = 0;
        let cont = loop {
            match cdir::parse_record(&self.carry[pos..], self.base_offset)? {
                ParseOutcome::Entry(entry, consumed) => {
                    pos += consumed;
                    let Some(entry) = entry else { continue };
                    // Duplicate names: the last record wins, in place.
                    match self.by_name.entry(entry.name.clone()) {
                        std::collections::hash_map::Entry::Occupied(slot) => {
                            self.entries[*slot.get()] = *entry;
                        }
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert(self.entries.len());
                            self.entries.push(*entry);
                        }
                    }
                }
                ParseOutcome::NeedMore => break true,
                ParseOutcome::End => {
                    self.carry.clear();
                    return Ok(false);
                }
            }
        };
        self.carry.drain(..pos);
        Ok(cont)
    }

    fn finish(&mut self) -> ZipFs {
        ZipFs {
            entries: std::mem::take(&mut self.entries),
            comment: self.comment.take(),
            base_offset: self.base_offset,
            file_size: self.file_size,
        }
    }
}

impl CdLocation {
    fn needs_zip64(&self) -> bool {
        self.cd_offset == 0xFFFF_FFFF || self.cd_size == 0xFFFF_FFFF
    }
}

fn decode_eocd64(rec: &[u8], record_pos: u64) -> Result<CdLocation> {
    let disk = u32_le(rec, 16)?;
    let cd_disk = u32_le(rec, 20)?;
    if disk != cd_disk || disk != 0 {
        return Err(ZipError::Unsupported(
            "split (multi-disk) archive".to_string(),
        ));
    }
    Ok(CdLocation {
        cd_size: u64_le(rec, 40)?,
        cd_offset: u64_le(rec, 48)?,
        record_pos,
    })
}
