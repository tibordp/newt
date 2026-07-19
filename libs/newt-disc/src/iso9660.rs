//! ISO 9660 (ECMA-119) with Joliet and Rock Ridge extensions.

use std::collections::HashSet;
use std::ops::Range;

use crate::rockridge::{self, RrAccum};
use crate::{
    DiscError, Entry, EntryData, EntryKind, Extent, ExtentKind, ListProgress, Result, SECTOR,
    Store, corrupt, epoch_ms, recorded_ranges, slice, u16_le, u32_le,
};

const FLAG_HIDDEN: u8 = 1 << 0;
const FLAG_DIR: u8 = 1 << 1;
const FLAG_ASSOCIATED: u8 = 1 << 2;
const FLAG_MULTI_EXTENT: u8 = 1 << 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Plain,
    Joliet,
    RockRidge,
}

#[derive(Debug)]
pub struct IsoFs {
    pub image_size: u64,
    pub root: Entry,
    pub volume_label: Option<String>,
    block_size: u64,
    variant: Variant,
    /// SUSP skip length from the root SP entry.
    susp_skip: usize,
    /// Kept so Rock Ridge detection can switch the root back to the
    /// primary hierarchy after a Joliet root was provisionally chosen.
    primary_root: Entry,
}

impl IsoFs {
    pub(crate) fn describe(&self) -> String {
        match self.variant {
            Variant::Plain => "ISO 9660".to_string(),
            Variant::Joliet => "ISO 9660 (Joliet)".to_string(),
            Variant::RockRidge => "ISO 9660 (Rock Ridge)".to_string(),
        }
    }

    pub(crate) fn from_descriptors(vds: VolumeDescriptors, image_size: u64) -> Result<IsoFs> {
        let primary = vds.primary.ok_or(DiscError::NotADisc)?;
        let block_size = primary.block_size;
        let primary_root = parse_root_record(&primary.root_record, block_size, false)?;
        // Provisional: Joliet if present; Rock Ridge detection may override.
        let (variant, root) = match &vds.joliet {
            Some(svd) => (
                Variant::Joliet,
                parse_root_record(&svd.root_record, block_size, false)?,
            ),
            None => (Variant::Plain, primary_root.clone()),
        };
        Ok(IsoFs {
            image_size,
            root,
            volume_label: primary.volume_label,
            block_size,
            variant,
            susp_skip: 0,
            primary_root,
        })
    }

    /// The range holding the primary root directory's first block — enough
    /// to see the "." record's system-use area for Rock Ridge detection.
    pub(crate) fn primary_root_probe_range(&self, image_size: u64) -> Result<Range<u64>> {
        let start = match &self.primary_root.data {
            EntryData::Extents(exts) if !exts.is_empty() => exts[0].offset,
            _ => return Err(corrupt("primary root directory has no extent")),
        };
        let len = self.block_size.min(self.primary_root.size.max(1));
        if start + len > image_size {
            return Err(corrupt("root directory extends past end of image"));
        }
        Ok(start..start + len)
    }

    /// Inspect the primary root's "." record for SUSP/RRIP. On detection the
    /// primary hierarchy (with Rock Ridge names/attributes) takes precedence
    /// over Joliet.
    pub(crate) fn detect_rock_ridge(&mut self, root_block: &[u8]) {
        let Some(dot) = first_record(root_block, self.block_size) else {
            return;
        };
        let (skip, found) = rockridge::detect(&dot.system_use);
        if found {
            self.susp_skip = skip;
            self.variant = Variant::RockRidge;
            self.root = self.primary_root.clone();
        }
    }

    pub(crate) fn begin_list(&self, dir: Entry) -> Result<ListProgress<IsoListState>> {
        let extents = match &dir.data {
            EntryData::Extents(e) => e.clone(),
            EntryData::Inline(_) => return Err(corrupt("ISO directories are never inline")),
        };
        let needs = recorded_ranges(&extents);
        Ok(ListProgress::Need(IsoListState::ReadDir { extents }, needs))
    }

    pub(crate) fn continue_list(
        &self,
        state: IsoListState,
        store: &Store,
    ) -> Result<ListProgress<IsoListState>> {
        match state {
            IsoListState::ReadDir { extents } => {
                let buf = store.concat_extents(&extents)?;
                let joliet = self.variant == Variant::Joliet;
                let records = parse_records(&buf, self.block_size, joliet)?;
                let mut builds: Vec<Build> = Vec::new();
                for rec in records {
                    if rec.name_bytes == [0x00] || rec.name_bytes == [0x01] {
                        continue; // "." / ".."
                    }
                    if rec.flags & FLAG_ASSOCIATED != 0 {
                        continue;
                    }
                    // Multi-extent: continuation records append to the
                    // previous record with the same identifier.
                    if let Some(last) = builds.last_mut()
                        && last.pending_more
                        && last.rec.name_bytes == rec.name_bytes
                    {
                        last.rec.extents.extend(rec.extents.clone());
                        last.rec.size += rec.size;
                        last.pending_more = rec.flags & FLAG_MULTI_EXTENT != 0;
                        continue;
                    }
                    let mut rr = RrAccum::default();
                    if self.variant == Variant::RockRidge {
                        rr.absorb(strip_skip(&rec.system_use, self.susp_skip));
                    }
                    builds.push(Build {
                        pending_more: rec.flags & FLAG_MULTI_EXTENT != 0,
                        rec,
                        rr,
                    });
                }
                self.advance(builds)
            }
            IsoListState::ReadCe { mut builds } => {
                for b in &mut builds {
                    // Absorb every continuation area already fetched; a CE
                    // discovered *inside* one goes to the next round.
                    while let Some((block, off, len)) = b.rr.peek_ce() {
                        let start = block * self.block_size + off;
                        let Some(area) = store.try_get(&(start..start + len)) else {
                            break;
                        };
                        let area = area.to_vec();
                        b.rr.take_ce();
                        b.rr.absorb(&area);
                    }
                }
                self.advance(builds)
            }
            IsoListState::FixChildLinks { mut entries, links } => {
                for (idx, target_offset) in links {
                    let block = store.get(&(target_offset..target_offset + self.block_size))?;
                    let dot = first_record(block, self.block_size)
                        .ok_or_else(|| corrupt("relocated directory has no \".\" record"))?;
                    let entry = &mut entries[idx];
                    entry.size = dot.size;
                    entry.data = EntryData::Extents(dot.extents.clone());
                }
                Ok(ListProgress::Done(entries))
            }
        }
    }

    /// Move a build set forward: gather outstanding CE areas, else finalize
    /// (which may in turn need child-link fixups).
    fn advance(&self, builds: Vec<Build>) -> Result<ListProgress<IsoListState>> {
        let mut ce_needs: Vec<Range<u64>> = Vec::new();
        let mut seen = HashSet::new();
        for b in &builds {
            if let Some((block, off, len)) = b.rr.peek_ce() {
                let start = block * self.block_size + off;
                if seen.insert((start, len)) {
                    ce_needs.push(start..start + len);
                }
            }
        }
        if !ce_needs.is_empty() {
            return Ok(ListProgress::Need(
                IsoListState::ReadCe { builds },
                ce_needs,
            ));
        }

        let mut entries = Vec::with_capacity(builds.len());
        let mut links: Vec<(usize, u64)> = Vec::new();
        for b in builds {
            if b.rr.relocated() {
                continue; // appears at its real location elsewhere
            }
            let child_link = b.rr.child_link();
            let Some(entry) = self.finalize(b)? else {
                continue;
            };
            if let Some(lba) = child_link {
                links.push((entries.len(), lba * self.block_size));
            }
            entries.push(entry);
        }
        if links.is_empty() {
            return Ok(ListProgress::Done(entries));
        }
        let needs = links
            .iter()
            .map(|(_, off)| *off..*off + self.block_size)
            .collect();
        Ok(ListProgress::Need(
            IsoListState::FixChildLinks { entries, links },
            needs,
        ))
    }

    fn finalize(&self, b: Build) -> Result<Option<Entry>> {
        let rec = b.rec;
        let rr = b.rr;

        let name = match rr.name() {
            Some(n) => n.to_string(),
            None => decode_iso_name(&rec.name_bytes, self.variant == Variant::Joliet),
        };
        if name.is_empty() || name == "." || name == ".." {
            return Ok(None);
        }

        let is_dir = rec.flags & FLAG_DIR != 0 || rr.child_link().is_some();
        let link_target = rr.symlink_target();
        let kind = if link_target.is_some() {
            EntryKind::Symlink
        } else if is_dir {
            EntryKind::Dir
        } else {
            EntryKind::File
        };

        let (modified, accessed, created) = rr.times();
        Ok(Some(Entry {
            hidden: rec.flags & FLAG_HIDDEN != 0 || name.starts_with('.'),
            name,
            kind,
            size: rec.size,
            data: EntryData::Extents(rec.extents),
            mode: rr.mode(),
            uid: rr.uid(),
            gid: rr.gid(),
            nlink: rr.nlink(),
            modified: modified.or(rec.recorded),
            accessed,
            created,
            link_target,
        }))
    }
}

pub(crate) enum IsoListState {
    ReadDir {
        extents: Vec<Extent>,
    },
    ReadCe {
        builds: Vec<Build>,
    },
    FixChildLinks {
        entries: Vec<Entry>,
        /// (index into `entries`, absolute offset of the relocated
        /// directory's first block).
        links: Vec<(usize, u64)>,
    },
}

pub(crate) struct Build {
    rec: RawRecord,
    rr: RrAccum,
    /// Last seen record had the multi-extent flag: the next record with the
    /// same identifier continues this file.
    pending_more: bool,
}

// ---------------------------------------------------------------------------
// Volume descriptors
// ---------------------------------------------------------------------------

pub(crate) struct VolumeDescriptors {
    pub primary: Option<Vd>,
    pub joliet: Option<Vd>,
}

pub(crate) struct Vd {
    root_record: Vec<u8>,
    block_size: u64,
    volume_label: Option<String>,
}

/// Scan the volume descriptor window (starting at sector 16).
pub(crate) fn parse_volume_descriptors(buf: &[u8]) -> Result<VolumeDescriptors> {
    let mut out = VolumeDescriptors {
        primary: None,
        joliet: None,
    };
    let mut found_any = false;
    for sector in buf.chunks(SECTOR as usize) {
        if sector.len() < SECTOR as usize {
            break;
        }
        if &sector[1..6] != b"CD001" {
            break;
        }
        found_any = true;
        match sector[0] {
            1 if out.primary.is_none() => {
                out.primary = Some(parse_vd(sector, false)?);
            }
            2 if out.joliet.is_none() => {
                // Joliet is a supplementary volume with a UCS-2 escape
                // sequence (levels 1-3).
                let esc = &sector[88..120];
                if esc.starts_with(&[0x25, 0x2F, 0x40])
                    || esc.starts_with(&[0x25, 0x2F, 0x43])
                    || esc.starts_with(&[0x25, 0x2F, 0x45])
                {
                    out.joliet = Some(parse_vd(sector, true)?);
                }
            }
            255 => break,
            _ => {}
        }
    }
    if !found_any {
        return Err(DiscError::NotADisc);
    }
    Ok(out)
}

fn parse_vd(sector: &[u8], joliet: bool) -> Result<Vd> {
    let block_size = u64::from(u16_le(sector, 128)?);
    if !matches!(block_size, 512 | 1024 | 2048) {
        return Err(corrupt(format!("bad logical block size {}", block_size)));
    }
    let root_len = usize::from(sector[156]);
    let root_record = slice(sector, 156, root_len.max(34))?.to_vec();
    let label_raw = &sector[40..72];
    let label = if joliet {
        decode_ucs2(label_raw)
    } else {
        String::from_utf8_lossy(label_raw).into_owned()
    };
    let label = label.trim_end_matches([' ', '\0']).to_string();
    Ok(Vd {
        root_record,
        block_size,
        volume_label: (!label.is_empty()).then_some(label),
    })
}

fn parse_root_record(record: &[u8], block_size: u64, joliet: bool) -> Result<Entry> {
    let rec = parse_record(record, block_size, joliet)?
        .ok_or_else(|| corrupt("empty root directory record"))?;
    Ok(Entry::dir(
        String::new(),
        EntryData::Extents(rec.extents),
        rec.size,
    ))
}

// ---------------------------------------------------------------------------
// Directory records
// ---------------------------------------------------------------------------

pub(crate) struct RawRecord {
    name_bytes: Vec<u8>,
    extents: Vec<Extent>,
    size: u64,
    flags: u8,
    recorded: Option<i64>,
    system_use: Vec<u8>,
}

/// Parse one directory record at the start of `buf`. Returns `None` for a
/// zero length byte (end-of-block padding).
fn parse_record(buf: &[u8], block_size: u64, _joliet: bool) -> Result<Option<RawRecord>> {
    let len = usize::from(*buf.first().ok_or_else(|| corrupt("empty record"))?);
    if len == 0 {
        return Ok(None);
    }
    if len < 34 {
        return Err(corrupt("directory record too short"));
    }
    let rec = slice(buf, 0, len)?;
    let ext_attr_len = u64::from(rec[1]);
    let lba = u64::from(u32_le(rec, 2)?);
    let data_len = u64::from(u32_le(rec, 10)?);
    let recorded = decode_dir_datetime(&rec[18..25]);
    let flags = rec[25];
    if rec[26] != 0 || rec[27] != 0 {
        return Err(DiscError::Unsupported("interleaved file".into()));
    }
    let name_len = usize::from(rec[32]);
    let name_bytes = slice(rec, 33, name_len)?.to_vec();
    let su_start = 33 + name_len + (1 - name_len % 2); // pad byte when name_len is even
    let system_use = if su_start < len {
        rec[su_start..].to_vec()
    } else {
        Vec::new()
    };
    let offset = (lba + ext_attr_len) * block_size;
    Ok(Some(RawRecord {
        name_bytes,
        extents: vec![Extent {
            offset,
            len: data_len,
            kind: ExtentKind::Recorded,
        }],
        size: data_len,
        flags,
        recorded,
        system_use,
    }))
}

/// Iterate all records in a directory's data. Records never span a logical
/// block boundary; a zero length byte skips to the next block.
fn parse_records(buf: &[u8], block_size: u64, joliet: bool) -> Result<Vec<RawRecord>> {
    let block = block_size as usize;
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() {
        let len = usize::from(buf[pos]);
        if len == 0 {
            pos = (pos / block + 1) * block;
            continue;
        }
        match parse_record(&buf[pos..], block_size, joliet)? {
            Some(rec) => out.push(rec),
            None => unreachable!(),
        }
        pos += len;
    }
    Ok(out)
}

/// The first record in a directory block (its "." record).
fn first_record(block: &[u8], block_size: u64) -> Option<RawRecord> {
    parse_record(block, block_size, false).ok().flatten()
}

fn strip_skip(su: &[u8], skip: usize) -> &[u8] {
    su.get(skip..).unwrap_or(&[])
}

fn decode_iso_name(bytes: &[u8], joliet: bool) -> String {
    let mut name = if joliet {
        decode_ucs2(bytes)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    // Strip the ";version" suffix and a trailing "." from an empty
    // extension, per the usual presentation of ECMA-119 identifiers.
    if let Some(pos) = name.rfind(';') {
        name.truncate(pos);
    }
    if name.ends_with('.') {
        name.pop();
    }
    name
}

fn decode_ucs2(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// 7-byte directory record date/time.
fn decode_dir_datetime(b: &[u8]) -> Option<i64> {
    if b.len() < 7 || b[..6] == [0; 6] {
        return None;
    }
    let tz_min = i32::from(b[6] as i8) * 15;
    epoch_ms(
        1900 + i32::from(b[0]),
        u32::from(b[1]),
        u32::from(b[2]),
        u32::from(b[3]),
        u32::from(b[4]),
        u32::from(b[5]),
        0,
        tz_min,
    )
}
