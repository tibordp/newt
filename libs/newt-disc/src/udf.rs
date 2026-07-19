//! UDF (ECMA-167 / OSTA UDF 1.02–2.60), read-only. Type-1 (physical) and
//! Type-2 Metadata partition maps; VAT and sparable maps (packet-written
//! media) are out of scope.

use std::collections::HashSet;
use std::ops::Range;

use crate::{
    DiscError, Entry, EntryData, EntryKind, Extent, ExtentKind, ListProgress, MAX_SYMLINK_BYTES,
    Result, Step, Store, corrupt, epoch_ms, recorded_ranges, slice, u16_le, u32_le, u64_le,
};

// Descriptor tag identifiers.
const TAG_PVD: u16 = 1;
const TAG_AVDP: u16 = 2;
const TAG_VDP: u16 = 3;
const TAG_PD: u16 = 5;
const TAG_LVD: u16 = 6;
const TAG_TD: u16 = 8;
const TAG_FSD: u16 = 256;
const TAG_FID: u16 = 257;
const TAG_AED: u16 = 258;
const TAG_IE: u16 = 259;
const TAG_FE: u16 = 261;
const TAG_EFE: u16 = 266;

// ICB file types.
const FT_DIR: u8 = 4;
const FT_SYMLINK: u8 = 12;
const FT_METADATA: u8 = 250;
const FT_METADATA_MIRROR: u8 = 251;

/// Cap on how much of a volume descriptor sequence we'll scan.
const MAX_VDS_BYTES: u64 = 1024 * 1024;
/// ICB indirection (strategy 4096) hop bound.
const MAX_ICB_HOPS: u8 = 8;

/// Logical sector sizes we probe for. 2048 is optical media / `.iso`;
/// 512 shows up when a UDF volume was formatted onto hard-disk-profile
/// media (e.g. macOS `newfs_udf` on an attached image) and then dumped.
pub(crate) const SECTOR_SIZES: [u64; 4] = [512, 1024, 2048, 4096];

// ---------------------------------------------------------------------------
// Low-level structures
// ---------------------------------------------------------------------------

/// Validate a descriptor tag; returns the tag identifier. The tag checksum
/// covers the first 16 bytes except the checksum byte itself. CRC is not
/// verified (some mastering tools are sloppy with it; the checksum plus
/// structural validation is plenty for recognition).
fn parse_tag(buf: &[u8]) -> Option<u16> {
    if buf.len() < 16 {
        return None;
    }
    let id = u16::from_le_bytes([buf[0], buf[1]]);
    if id == 0 {
        return None;
    }
    let mut sum = 0u8;
    for (i, b) in buf[..16].iter().enumerate() {
        if i != 4 {
            sum = sum.wrapping_add(*b);
        }
    }
    (sum == buf[4]).then_some(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LongAd {
    /// Extent length with the 2-bit type stripped.
    len: u64,
    ext_type: u8,
    lb: u64,
    part: u16,
}

fn parse_long_ad(buf: &[u8], off: usize) -> Result<LongAd> {
    let raw = u32_le(buf, off)?;
    Ok(LongAd {
        len: u64::from(raw & 0x3FFF_FFFF),
        ext_type: (raw >> 30) as u8,
        lb: u64::from(u32_le(buf, off + 4)?),
        part: u16_le(buf, off + 8)?,
    })
}

/// OSTA CS0 d-characters: a compression-id byte (8 or 16) then the payload.
fn decode_dchars(bytes: &[u8]) -> String {
    match bytes.split_first() {
        Some((8, rest)) => rest.iter().map(|&b| char::from(b)).collect(),
        Some((16, rest)) => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::new(),
    }
}

/// A dstring field: d-characters with the final byte holding the length.
fn decode_dstring(field: &[u8]) -> Option<String> {
    let (&len, content) = field.split_last()?;
    let len = usize::from(len).min(content.len());
    let s = decode_dchars(&content[..len]);
    let s = s.trim_end_matches('\0').to_string();
    (!s.is_empty()).then_some(s)
}

/// 12-byte timestamp.
fn decode_timestamp(b: &[u8]) -> Option<i64> {
    if b.len() < 12 {
        return None;
    }
    let tz_type = u16::from_le_bytes([b[0], b[1]]);
    let tz12 = i32::from(tz_type & 0x0FFF);
    let tz = if tz12 >= 0x800 { tz12 - 0x1000 } else { tz12 };
    let tz_min = if (-1440..=1440).contains(&tz) { tz } else { 0 };
    let year = i32::from(i16::from_le_bytes([b[2], b[3]]));
    if year == 0 {
        return None;
    }
    epoch_ms(
        year,
        u32::from(b[4]),
        u32::from(b[5]),
        u32::from(b[6]),
        u32::from(b[7]),
        u32::from(b[8]),
        u32::from(b[9]).min(99) * 10,
        tz_min,
    )
}

/// UDF permission bits (5 per class, low-to-high: other/group/owner with
/// execute=1, write=2, read=4 within each class) → Unix rwx bits.
fn decode_permissions(p: u32) -> u32 {
    ((p >> 10) & 0o7) << 6 | ((p >> 5) & 0o7) << 3 | (p & 0o7)
}

fn regid_identifier(buf: &[u8], off: usize) -> Result<&[u8]> {
    slice(buf, off + 1, 23)
}

// ---------------------------------------------------------------------------
// Anchor + volume descriptor sequence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub(crate) struct Avdp {
    main: (u64, u64),
    reserve: (u64, u64),
}

/// Parse an anchor volume descriptor pointer; `sector` guards the tag
/// location field so stray checksummed garbage can't match.
pub(crate) fn parse_avdp(buf: &[u8], sector: u64) -> Option<Avdp> {
    if parse_tag(buf)? != TAG_AVDP {
        return None;
    }
    let location = u32_le(buf, 12).ok()?;
    if u64::from(location) != sector {
        return None;
    }
    let main_len = u64::from(u32_le(buf, 16).ok()?);
    let main_loc = u64::from(u32_le(buf, 20).ok()?);
    let res_len = u64::from(u32_le(buf, 24).ok()?);
    let res_loc = u64::from(u32_le(buf, 28).ok()?);
    if main_len == 0 {
        return None;
    }
    Some(Avdp {
        main: (main_loc, main_len),
        reserve: (res_loc, res_len),
    })
}

#[derive(Debug, Clone)]
enum RawMap {
    Physical {
        part_num: u16,
    },
    Metadata {
        part_num: u16,
        file_lb: u64,
        mirror_lb: u64,
    },
}

#[derive(Debug, Clone)]
struct VolInfo {
    volume_label: Option<String>,
    revision: Option<u16>,
    /// Logical block size from the LVD (usually equals the sector size).
    block: u64,
    fsd_ad: LongAd,
    maps: Vec<RawMap>,
    /// (partition number, start sector, length in sectors)
    pds: Vec<(u16, u64, u64)>,
}

fn parse_vds(buf: &[u8], sector: u64) -> Result<VdsOutcome> {
    let mut label = None;
    let mut lvd: Option<(LongAd, Vec<RawMap>, Option<u16>, u64)> = None;
    let mut pds: Vec<(u16, u64, u64)> = Vec::new();

    for block in buf.chunks(sector as usize) {
        let Some(id) = parse_tag(block) else { break };
        match id {
            TAG_PVD => {
                if label.is_none() {
                    label = decode_dstring(slice(block, 24, 32)?);
                }
            }
            TAG_PD => {
                let part_num = u16_le(block, 22)?;
                let start = u64::from(u32_le(block, 188)?);
                let count = u64::from(u32_le(block, 192)?);
                pds.push((part_num, start, count));
            }
            TAG_LVD => {
                let block_size = u64::from(u32_le(block, 212)?);
                if !SECTOR_SIZES.contains(&block_size) {
                    return Err(DiscError::Unsupported(format!(
                        "UDF logical block size {}",
                        block_size
                    )));
                }
                let lvd_label = decode_dstring(slice(block, 84, 128)?);
                if lvd_label.is_some() {
                    label = lvd_label;
                }
                // Domain identifier suffix carries the UDF revision (BCD).
                let revision = u16_le(block, 216 + 24).ok().filter(|r| *r != 0);
                let fsd_ad = parse_long_ad(block, 248)?;
                let n_maps = u32_le(block, 268)? as usize;
                let map_table_len = u32_le(block, 264)? as usize;
                let table = slice(block, 440, map_table_len)?;
                let mut maps = Vec::new();
                let mut pos = 0usize;
                for _ in 0..n_maps.min(64) {
                    let hdr = slice(table, pos, 2)?;
                    let (mtype, mlen) = (hdr[0], usize::from(hdr[1]));
                    if mlen < 2 || pos + mlen > table.len() {
                        return Err(corrupt("partition map table overrun"));
                    }
                    let map = slice(table, pos, mlen)?;
                    match mtype {
                        1 => maps.push(RawMap::Physical {
                            part_num: u16_le(map, 4)?,
                        }),
                        2 => {
                            let ident = regid_identifier(map, 4)?;
                            if ident.starts_with(b"*UDF Metadata Partition") {
                                maps.push(RawMap::Metadata {
                                    part_num: u16_le(map, 38)?,
                                    file_lb: u64::from(u32_le(map, 40)?),
                                    mirror_lb: u64::from(u32_le(map, 44)?),
                                });
                            } else if ident.starts_with(b"*UDF Virtual Partition") {
                                return Err(DiscError::Unsupported(
                                    "UDF virtual partition (VAT / packet-written media)".into(),
                                ));
                            } else if ident.starts_with(b"*UDF Sparable Partition") {
                                return Err(DiscError::Unsupported(
                                    "UDF sparable partition (packet-written media)".into(),
                                ));
                            } else {
                                return Err(DiscError::Unsupported(format!(
                                    "UDF type-2 partition map \"{}\"",
                                    String::from_utf8_lossy(ident).trim_end_matches('\0')
                                )));
                            }
                        }
                        t => {
                            return Err(DiscError::Unsupported(format!(
                                "UDF partition map type {}",
                                t
                            )));
                        }
                    }
                    pos += mlen;
                }
                lvd = Some((fsd_ad, maps, revision, block_size));
            }
            TAG_VDP => {
                let len = u64::from(u32_le(block, 20)?);
                let loc = u64::from(u32_le(block, 24)?);
                return Ok(VdsOutcome::Continue { loc, len });
            }
            TAG_TD => break,
            _ => {}
        }
    }

    let (fsd_ad, maps, revision, block) =
        lvd.ok_or_else(|| corrupt("volume descriptor sequence has no logical volume"))?;
    Ok(VdsOutcome::Done(VolInfo {
        volume_label: label,
        revision,
        block,
        fsd_ad,
        maps,
        pds,
    }))
}

enum VdsOutcome {
    Done(VolInfo),
    /// Volume Descriptor Pointer: sequence continues in another extent.
    Continue {
        loc: u64,
        len: u64,
    },
}

// ---------------------------------------------------------------------------
// Partitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum PartitionRef {
    /// Absolute byte offset of the partition start.
    Physical { start: u64 },
    /// Metadata partition: logical blocks resolve through the metadata
    /// file's extents (absolute byte runs, block-aligned).
    Metadata { extents: Vec<Extent> },
}

#[derive(Debug)]
pub struct UdfFs {
    pub image_size: u64,
    pub root: Entry,
    pub volume_label: Option<String>,
    revision: Option<u16>,
    /// Logical block size for within-partition addressing.
    block: u64,
    partitions: Vec<PartitionRef>,
}

impl UdfFs {
    pub(crate) fn describe(&self) -> String {
        match self.revision {
            Some(r) => format!("UDF {}.{:02x}", r >> 8, r & 0xFF),
            None => "UDF".to_string(),
        }
    }

    /// Translate `len` bytes starting at logical block `lb` of partition
    /// reference `part` into absolute image extents.
    fn resolve(&self, part: u16, lb: u64, len: u64) -> Result<Vec<Extent>> {
        let pref = self
            .partitions
            .get(usize::from(part))
            .ok_or_else(|| corrupt(format!("reference to unknown partition {}", part)))?;
        resolve_in(pref, lb, len, self.block)
    }
}

fn resolve_in(pref: &PartitionRef, lb: u64, len: u64, block: u64) -> Result<Vec<Extent>> {
    match pref {
        PartitionRef::Physical { start } => Ok(vec![Extent {
            offset: start + lb * block,
            len,
            kind: ExtentKind::Recorded,
        }]),
        PartitionRef::Metadata { extents } => {
            let mut out = Vec::new();
            let mut skip = lb * block;
            let mut remaining = len;
            for e in extents {
                if remaining == 0 {
                    break;
                }
                if skip >= e.len {
                    skip -= e.len;
                    continue;
                }
                let take = (e.len - skip).min(remaining);
                out.push(Extent {
                    offset: e.offset + skip,
                    len: take,
                    kind: e.kind,
                });
                skip = 0;
                remaining -= take;
            }
            if remaining > 0 {
                return Err(corrupt("read past the end of the metadata partition"));
            }
            Ok(out)
        }
    }
}

/// Build partition references from the map table; `metadata` supplies the
/// resolved metadata-file extents when a metadata map is present.
fn build_partitions(
    info: &VolInfo,
    metadata: Option<Vec<Extent>>,
    sector: u64,
) -> Result<Vec<PartitionRef>> {
    let phys_start = |part_num: u16| -> Result<u64> {
        info.pds
            .iter()
            .find(|(num, _, _)| *num == part_num)
            .map(|(_, start, _)| start * sector)
            .ok_or_else(|| {
                corrupt(format!(
                    "no partition descriptor for partition {}",
                    part_num
                ))
            })
    };
    info.maps
        .iter()
        .map(|map| match map {
            RawMap::Physical { part_num } => Ok(PartitionRef::Physical {
                start: phys_start(*part_num)?,
            }),
            RawMap::Metadata { .. } => metadata
                .clone()
                .map(|extents| PartitionRef::Metadata { extents })
                .ok_or_else(|| corrupt("metadata partition map without metadata file")),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// File entries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct FeInfo {
    file_type: u8,
    uid: Option<u32>,
    gid: Option<u32>,
    mode: u32,
    nlink: u32,
    info_len: u64,
    modified: Option<i64>,
    accessed: Option<i64>,
    created: Option<i64>,
    data: FeData,
}

#[derive(Debug, Clone)]
enum FeData {
    Inline(Vec<u8>),
    Ads {
        ads: Vec<RawAd>,
        /// 0 = short_ad, 1 = long_ad — needed to parse continuation AEDs.
        alloc_type: u16,
        /// Continuation AED: (partition ref, logical block).
        next: Option<(u16, u64)>,
    },
}

#[derive(Debug, Clone, Copy)]
struct RawAd {
    len: u64,
    part: u16,
    lb: u64,
    kind: ExtentKind,
}

enum FeParsed {
    Fe(Box<FeInfo>),
    /// Strategy-4096 indirect entry pointing at the next ICB.
    Indirect {
        part: u16,
        lb: u64,
    },
}

/// Parse a File Entry / Extended File Entry / Indirect Entry block.
/// `own_part` is the partition reference the ICB was addressed through —
/// short allocation descriptors inherit it.
fn parse_icb_block(block: &[u8], own_part: u16) -> Result<FeParsed> {
    if block.len() < 224 {
        return Err(corrupt("ICB block too short"));
    }
    let id = parse_tag(block).ok_or_else(|| corrupt("invalid ICB descriptor tag"))?;
    if id == TAG_IE {
        let ad = parse_long_ad(block, 36)?;
        return Ok(FeParsed::Indirect {
            part: ad.part,
            lb: ad.lb,
        });
    }
    if id != TAG_FE && id != TAG_EFE {
        return Err(corrupt(format!("expected file entry, found tag {}", id)));
    }
    let extended = id == TAG_EFE;
    let file_type = block[27];
    let alloc_type = u16_le(block, 34)? & 0x7;

    let uid = u32_le(block, 36)?;
    let gid = u32_le(block, 40)?;
    let perms = u32_le(block, 44)?;
    let nlink = u32::from(u16_le(block, 48)?);
    let info_len = u64_le(block, 56)?;

    let (accessed, modified, created, l_ea_off, l_ad_off, data_off) = if extended {
        (
            80usize,
            92usize,
            Some(104usize),
            208usize,
            212usize,
            216usize,
        )
    } else {
        (72, 84, None, 168, 172, 176)
    };
    let l_ea = u32_le(block, l_ea_off)? as usize;
    let l_ad = u32_le(block, l_ad_off)? as usize;
    let ad_area = slice(block, data_off + l_ea, l_ad)?;

    let data = match alloc_type {
        3 => FeData::Inline(ad_area.to_vec()),
        0 | 1 => {
            let (ads, next) = parse_ad_area(ad_area, alloc_type, own_part)?;
            FeData::Ads {
                ads,
                alloc_type,
                next,
            }
        }
        t => {
            return Err(DiscError::Unsupported(format!(
                "allocation descriptor type {}",
                t
            )));
        }
    };

    Ok(FeParsed::Fe(Box::new(FeInfo {
        file_type,
        uid: (uid != u32::MAX).then_some(uid),
        gid: (gid != u32::MAX).then_some(gid),
        mode: decode_permissions(perms),
        nlink,
        info_len,
        modified: decode_timestamp(&block[modified..modified + 12]),
        accessed: decode_timestamp(&block[accessed..accessed + 12]),
        created: created.and_then(|off| decode_timestamp(&block[off..off + 12])),
        data,
    })))
}

/// Parsed allocation descriptors plus an optional continuation AED at
/// (partition ref, logical block).
type AdRun = (Vec<RawAd>, Option<(u16, u64)>);

/// Parse a run of short (type 0) or long (type 1) allocation descriptors.
fn parse_ad_area(area: &[u8], alloc_type: u16, own_part: u16) -> Result<AdRun> {
    let stride = if alloc_type == 0 { 8 } else { 16 };
    let mut ads = Vec::new();
    let mut pos = 0usize;
    while pos + stride <= area.len() {
        let raw = u32_le(area, pos)?;
        let len = u64::from(raw & 0x3FFF_FFFF);
        let ext_type = (raw >> 30) as u8;
        if len == 0 {
            break;
        }
        let (lb, part) = if alloc_type == 0 {
            (u64::from(u32_le(area, pos + 4)?), own_part)
        } else {
            (u64::from(u32_le(area, pos + 4)?), u16_le(area, pos + 8)?)
        };
        match ext_type {
            3 => return Ok((ads, Some((part, lb)))),
            0 => ads.push(RawAd {
                len,
                part,
                lb,
                kind: ExtentKind::Recorded,
            }),
            _ => ads.push(RawAd {
                len,
                part,
                lb,
                kind: ExtentKind::Sparse,
            }),
        }
        pos += stride;
    }
    Ok((ads, None))
}

/// Parse an Allocation Extent Descriptor continuation block.
fn parse_aed(block: &[u8], alloc_type: u16, own_part: u16) -> Result<AdRun> {
    let id = parse_tag(block).ok_or_else(|| corrupt("invalid AED tag"))?;
    if id != TAG_AED {
        return Err(corrupt(format!("expected AED, found tag {}", id)));
    }
    let l_ad = u32_le(block, 20)? as usize;
    parse_ad_area(slice(block, 24, l_ad)?, alloc_type, own_part)
}

// ---------------------------------------------------------------------------
// File identifier descriptors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Fid {
    name: String,
    hidden: bool,
    icb: LongAd,
}

fn parse_fids(buf: &[u8]) -> Result<Vec<Fid>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 38 <= buf.len() {
        let fid = &buf[pos..];
        let Some(id) = parse_tag(fid) else { break };
        if id != TAG_FID {
            return Err(corrupt(format!("expected FID, found tag {}", id)));
        }
        let characteristics = fid[18];
        let l_fi = usize::from(fid[19]);
        let icb = parse_long_ad(fid, 20)?;
        let l_iu = usize::from(u16_le(fid, 36)?);
        let name_bytes = slice(fid, 38 + l_iu, l_fi)?;
        let total = (38 + l_iu + l_fi).div_ceil(4) * 4;

        let deleted = characteristics & 0x04 != 0;
        let parent = characteristics & 0x08 != 0;
        if !deleted && !parent {
            let name = decode_dchars(name_bytes);
            if !name.is_empty() {
                out.push(Fid {
                    name,
                    hidden: characteristics & 0x01 != 0,
                    icb,
                });
            }
        }
        pos += total;
    }
    Ok(out)
}

/// UDF symlink payload: a sequence of path components.
fn parse_path_components(buf: &[u8]) -> Option<String> {
    let mut target = String::new();
    let mut pos = 0usize;
    while pos + 4 <= buf.len() {
        let ctype = buf[pos];
        let clen = usize::from(buf[pos + 1]);
        let content = buf.get(pos + 4..pos + 4 + clen)?;
        if !target.is_empty() && !target.ends_with('/') {
            target.push('/');
        }
        match ctype {
            1 | 2 => {
                target.clear();
                target.push('/');
            }
            3 => target.push_str(".."),
            4 => target.push('.'),
            5 => target.push_str(&decode_dchars(content)),
            _ => return None,
        }
        pos += 4 + clen;
    }
    (!target.is_empty()).then_some(target)
}

// ---------------------------------------------------------------------------
// Probe state machine
// ---------------------------------------------------------------------------

pub(crate) struct UdfProbe {
    image_size: u64,
    /// Logical sector size the anchor was found at.
    sector: u64,
    avdp: Avdp,
    tried_reserve: bool,
    state: PState,
}

enum PState {
    Vds {
        extent: (u64, u64),
    },
    MetaFe {
        info: VolInfo,
        phys_start: u64,
        file_lb: u64,
        mirror_lb: u64,
        trying_mirror: bool,
    },
    Fsd {
        info: VolInfo,
        partitions: Vec<PartitionRef>,
        fsd_range: Range<u64>,
    },
    RootFe {
        info: VolInfo,
        partitions: Vec<PartitionRef>,
        icb_part: u16,
        icb_range: Range<u64>,
        hops: u8,
    },
    /// Transient placeholder while a step is in flight.
    Taken,
}

impl UdfProbe {
    pub(crate) fn new(avdp: Avdp, image_size: u64, sector: u64) -> Self {
        UdfProbe {
            image_size,
            sector,
            avdp,
            tried_reserve: false,
            state: PState::Vds { extent: avdp.main },
        }
    }

    pub(crate) fn initial_needs(&self) -> Vec<Range<u64>> {
        vec![vds_range(self.avdp.main, self.image_size, self.sector)]
    }

    pub(crate) fn step(&mut self, store: &Store) -> Result<Step<UdfFs>> {
        {
            let sector = self.sector;
            match std::mem::replace(&mut self.state, PState::Taken) {
                PState::Vds { extent } => {
                    let range = vds_range(extent, self.image_size, sector);
                    let buf = store.get(&range)?;
                    let outcome = match parse_vds(buf, sector) {
                        Ok(o) => o,
                        Err(e) => {
                            // Fall back to the reserve sequence once.
                            if !self.tried_reserve && self.avdp.reserve.1 > 0 {
                                self.tried_reserve = true;
                                let extent = self.avdp.reserve;
                                let needs = vec![vds_range(extent, self.image_size, sector)];
                                self.state = PState::Vds { extent };
                                return Ok(Step::Need(needs));
                            }
                            return Err(e);
                        }
                    };
                    match outcome {
                        VdsOutcome::Continue { loc, len } => {
                            let extent = (loc, len);
                            let needs = vec![vds_range(extent, self.image_size, sector)];
                            self.state = PState::Vds { extent };
                            Ok(Step::Need(needs))
                        }
                        VdsOutcome::Done(info) => {
                            let meta = info.maps.iter().find_map(|m| match m {
                                RawMap::Metadata {
                                    part_num,
                                    file_lb,
                                    mirror_lb,
                                } => Some((*part_num, *file_lb, *mirror_lb)),
                                _ => None,
                            });
                            match meta {
                                Some((part_num, file_lb, mirror_lb)) => {
                                    let blk = info.block;
                                    let phys_start = info
                                        .pds
                                        .iter()
                                        .find(|(num, _, _)| *num == part_num)
                                        .map(|(_, start, _)| start * sector)
                                        .ok_or_else(|| {
                                            corrupt("metadata map references unknown partition")
                                        })?;
                                    let fe_off = phys_start + file_lb * blk;
                                    self.state = PState::MetaFe {
                                        info,
                                        phys_start,
                                        file_lb,
                                        mirror_lb,
                                        trying_mirror: false,
                                    };
                                    Ok(Step::Need(vec![fe_off..fe_off + blk]))
                                }
                                None => {
                                    let partitions = build_partitions(&info, None, sector)?;
                                    let (state, need) = self.to_fsd(info, partitions)?;
                                    self.state = state;
                                    Ok(Step::Need(vec![need]))
                                }
                            }
                        }
                    }
                }

                PState::MetaFe {
                    info,
                    phys_start,
                    file_lb,
                    mirror_lb,
                    trying_mirror,
                } => {
                    let blk = info.block;
                    let fe_off = phys_start + file_lb * blk;
                    let block = store.get(&(fe_off..fe_off + blk))?;
                    let parsed = parse_metadata_file(block, phys_start, blk);
                    match parsed {
                        Ok(extents) => {
                            let partitions = build_partitions(&info, Some(extents), sector)?;
                            let (state, need) = self.to_fsd(info, partitions)?;
                            self.state = state;
                            Ok(Step::Need(vec![need]))
                        }
                        Err(e) => {
                            if !trying_mirror && mirror_lb != u64::from(u32::MAX) {
                                let m_off = phys_start + mirror_lb * blk;
                                self.state = PState::MetaFe {
                                    info,
                                    phys_start,
                                    file_lb: mirror_lb,
                                    mirror_lb,
                                    trying_mirror: true,
                                };
                                return Ok(Step::Need(vec![m_off..m_off + blk]));
                            }
                            Err(e)
                        }
                    }
                }

                PState::Fsd {
                    info,
                    partitions,
                    fsd_range,
                } => {
                    let block = store.get(&fsd_range)?;
                    let id =
                        parse_tag(block).ok_or_else(|| corrupt("invalid file set descriptor"))?;
                    if id != TAG_FSD {
                        return Err(corrupt(format!("expected FSD, found tag {}", id)));
                    }
                    let root_ad = parse_long_ad(block, 400)?;
                    let icb_extents = resolve_ref(
                        &partitions,
                        root_ad.part,
                        root_ad.lb,
                        info.block,
                        info.block,
                    )?;
                    let icb_range = single_recorded(&icb_extents)?;
                    self.state = PState::RootFe {
                        info,
                        partitions,
                        icb_part: root_ad.part,
                        icb_range: icb_range.clone(),
                        hops: 0,
                    };
                    Ok(Step::Need(vec![icb_range]))
                }

                PState::RootFe {
                    info,
                    partitions,
                    icb_part,
                    icb_range,
                    hops,
                } => {
                    let block = store.get(&icb_range)?;
                    match parse_icb_block(block, icb_part)? {
                        FeParsed::Indirect { part, lb } => {
                            if hops >= MAX_ICB_HOPS {
                                return Err(corrupt("ICB indirection loop"));
                            }
                            let extents =
                                resolve_ref(&partitions, part, lb, info.block, info.block)?;
                            let icb_range = single_recorded(&extents)?;
                            self.state = PState::RootFe {
                                info,
                                partitions,
                                icb_part: part,
                                icb_range: icb_range.clone(),
                                hops: hops + 1,
                            };
                            Ok(Step::Need(vec![icb_range]))
                        }
                        FeParsed::Fe(fe) => {
                            if fe.file_type != FT_DIR {
                                return Err(corrupt("root ICB is not a directory"));
                            }
                            let data = fe_data_to_entry_data(&fe, &partitions, info.block)?;
                            let root = Entry::dir(String::new(), data, fe.info_len);
                            Ok(Step::Done(UdfFs {
                                image_size: self.image_size,
                                root,
                                volume_label: info.volume_label.clone(),
                                revision: info.revision,
                                block: info.block,
                                partitions,
                            }))
                        }
                    }
                }

                PState::Taken => Err(corrupt("probe stepped in transient state")),
            }
        }
    }

    fn to_fsd(&self, info: VolInfo, partitions: Vec<PartitionRef>) -> Result<(PState, Range<u64>)> {
        let fsd_ad = info.fsd_ad;
        let extents = resolve_ref(&partitions, fsd_ad.part, fsd_ad.lb, info.block, info.block)?;
        let fsd_range = single_recorded(&extents)?;
        Ok((
            PState::Fsd {
                info,
                partitions,
                fsd_range: fsd_range.clone(),
            },
            fsd_range,
        ))
    }
}

fn vds_range(extent: (u64, u64), image_size: u64, sector: u64) -> Range<u64> {
    let (loc, len) = extent;
    let start = loc * sector;
    let len = len.clamp(sector, MAX_VDS_BYTES);
    start..(start + len).min(image_size)
}

fn resolve_ref(
    partitions: &[PartitionRef],
    part: u16,
    lb: u64,
    len: u64,
    block: u64,
) -> Result<Vec<Extent>> {
    let pref = partitions
        .get(usize::from(part))
        .ok_or_else(|| corrupt(format!("reference to unknown partition {}", part)))?;
    resolve_in(pref, lb, len, block)
}

fn single_recorded(extents: &[Extent]) -> Result<Range<u64>> {
    match extents {
        [e] if e.kind == ExtentKind::Recorded => Ok(e.offset..e.offset + e.len),
        _ => Err(corrupt("descriptor lies in a sparse or fragmented block")),
    }
}

/// Parse the metadata file's FE and resolve its extents (which live in the
/// underlying physical partition).
fn parse_metadata_file(block: &[u8], phys_start: u64, blk: u64) -> Result<Vec<Extent>> {
    let FeParsed::Fe(fe) = parse_icb_block(block, 0)? else {
        return Err(corrupt("metadata file ICB is indirect"));
    };
    if fe.file_type != FT_METADATA && fe.file_type != FT_METADATA_MIRROR {
        return Err(corrupt(format!(
            "metadata file has ICB type {}",
            fe.file_type
        )));
    }
    match &fe.data {
        FeData::Ads {
            ads, next: None, ..
        } => Ok(ads
            .iter()
            .map(|ad| Extent {
                offset: phys_start + ad.lb * blk,
                len: ad.len,
                kind: ad.kind,
            })
            .collect()),
        FeData::Ads { next: Some(_), .. } => Err(DiscError::Unsupported(
            "metadata file allocation spans an AED chain".into(),
        )),
        FeData::Inline(_) => Err(corrupt("metadata file data is inline")),
    }
}

fn fe_data_to_entry_data(
    fe: &FeInfo,
    partitions: &[PartitionRef],
    block: u64,
) -> Result<EntryData> {
    match &fe.data {
        FeData::Inline(data) => Ok(EntryData::Inline(data.clone())),
        FeData::Ads { ads, next, .. } => {
            if next.is_some() {
                return Err(corrupt("unresolved allocation continuation"));
            }
            let mut extents = Vec::new();
            for ad in ads {
                match ad.kind {
                    ExtentKind::Recorded => {
                        extents.extend(resolve_ref(partitions, ad.part, ad.lb, ad.len, block)?)
                    }
                    ExtentKind::Sparse => extents.push(Extent {
                        offset: 0,
                        len: ad.len,
                        kind: ExtentKind::Sparse,
                    }),
                }
            }
            Ok(EntryData::Extents(extents))
        }
    }
}

// ---------------------------------------------------------------------------
// Directory listing state machine
// ---------------------------------------------------------------------------

pub(crate) struct UdfListState {
    phase: UPhase,
}

enum UPhase {
    ReadDirData { extents: Vec<Extent> },
    Items { items: Vec<Item> },
}

struct Item {
    fid: Fid,
    hops: u8,
    state: ItemState,
}

enum ItemState {
    NeedIcb {
        part: u16,
        range: Range<u64>,
    },
    NeedAed {
        fe: Box<FeInfo>,
        part: u16,
        range: Range<u64>,
    },
    NeedSymlink {
        fe: Box<FeInfo>,
        extents: Vec<Extent>,
    },
    Ready {
        fe: Box<FeInfo>,
        target: Option<String>,
    },
    /// Transient placeholder while the state is being advanced.
    Taken,
}

impl UdfFs {
    pub(crate) fn begin_list(&self, dir: Entry) -> Result<ListProgress<UdfListState>> {
        match dir.data {
            EntryData::Extents(extents) => {
                let needs = recorded_ranges(&extents);
                Ok(ListProgress::Need(
                    UdfListState {
                        phase: UPhase::ReadDirData { extents },
                    },
                    needs,
                ))
            }
            EntryData::Inline(data) => {
                let items = self.fids_to_items(parse_fids(&data)?)?;
                Ok(ListProgress::Need(
                    UdfListState {
                        phase: UPhase::Items { items },
                    },
                    Vec::new(),
                ))
            }
        }
    }

    pub(crate) fn continue_list(
        &self,
        state: UdfListState,
        store: &Store,
    ) -> Result<ListProgress<UdfListState>> {
        match state.phase {
            UPhase::ReadDirData { extents } => {
                let buf = store.concat_extents(&extents)?;
                let items = self.fids_to_items(parse_fids(&buf)?)?;
                self.drive_items(items, store)
            }
            UPhase::Items { items } => self.drive_items(items, store),
        }
    }

    fn fids_to_items(&self, fids: Vec<Fid>) -> Result<Vec<Item>> {
        fids.into_iter()
            .map(|fid| {
                let extents = self.resolve(fid.icb.part, fid.icb.lb, self.block)?;
                let range = single_recorded(&extents)?;
                Ok(Item {
                    hops: 0,
                    state: ItemState::NeedIcb {
                        part: fid.icb.part,
                        range,
                    },
                    fid,
                })
            })
            .collect()
    }

    fn drive_items(
        &self,
        mut items: Vec<Item>,
        store: &Store,
    ) -> Result<ListProgress<UdfListState>> {
        // Advance each item as far as the fetched data allows; anything
        // still blocked contributes to the next round's needs.
        for item in &mut items {
            self.advance_item(item, store)?;
        }

        let mut needs: Vec<Range<u64>> = Vec::new();
        let mut seen = HashSet::new();
        let mut all_ready = true;
        for item in &items {
            let range = match &item.state {
                ItemState::Ready { .. } => continue,
                ItemState::NeedIcb { range, .. } => vec![range.clone()],
                ItemState::NeedAed { range, .. } => vec![range.clone()],
                ItemState::NeedSymlink { extents, .. } => recorded_ranges(extents),
                ItemState::Taken => return Err(corrupt("listing item in transient state")),
            };
            all_ready = false;
            for r in range {
                if seen.insert((r.start, r.end)) {
                    needs.push(r);
                }
            }
        }
        if !all_ready {
            return Ok(ListProgress::Need(
                UdfListState {
                    phase: UPhase::Items { items },
                },
                needs,
            ));
        }

        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let ItemState::Ready { fe, target } = item.state else {
                unreachable!()
            };
            entries.push(self.finalize_entry(item.fid, *fe, target)?);
        }
        Ok(ListProgress::Done(entries))
    }

    fn advance_item(&self, item: &mut Item, store: &Store) -> Result<()> {
        loop {
            match std::mem::replace(&mut item.state, ItemState::Taken) {
                ItemState::NeedIcb { part, range } => {
                    let Some(block) = store.try_get(&range) else {
                        item.state = ItemState::NeedIcb { part, range };
                        return Ok(());
                    };
                    match parse_icb_block(block, part)? {
                        FeParsed::Indirect { part: npart, lb } => {
                            item.hops += 1;
                            if item.hops > MAX_ICB_HOPS {
                                return Err(corrupt("ICB indirection loop"));
                            }
                            let extents = self.resolve(npart, lb, self.block)?;
                            item.state = ItemState::NeedIcb {
                                part: npart,
                                range: single_recorded(&extents)?,
                            };
                        }
                        FeParsed::Fe(fe) => {
                            item.state = self.after_fe(fe)?;
                        }
                    }
                }
                ItemState::NeedAed {
                    mut fe,
                    part,
                    range,
                } => {
                    let Some(block) = store.try_get(&range) else {
                        item.state = ItemState::NeedAed { fe, part, range };
                        return Ok(());
                    };
                    item.hops += 1;
                    if item.hops > MAX_ICB_HOPS {
                        return Err(corrupt("allocation descriptor chain loop"));
                    }
                    let FeData::Ads {
                        mut ads,
                        alloc_type,
                        ..
                    } = fe.data
                    else {
                        return Err(corrupt("AED continuation on inline data"));
                    };
                    let (more, next) = parse_aed(block, alloc_type, part)?;
                    ads.extend(more);
                    fe.data = FeData::Ads {
                        ads,
                        alloc_type,
                        next,
                    };
                    item.state = self.after_fe(fe)?;
                }
                ItemState::NeedSymlink { fe, extents } => {
                    if recorded_ranges(&extents)
                        .iter()
                        .any(|r| store.try_get(r).is_none())
                    {
                        item.state = ItemState::NeedSymlink { fe, extents };
                        return Ok(());
                    }
                    let data = store.concat_extents(&extents)?;
                    let target = parse_path_components(&data);
                    item.state = ItemState::Ready { fe, target };
                }
                ready @ ItemState::Ready { .. } => {
                    item.state = ready;
                    return Ok(());
                }
                ItemState::Taken => return Err(corrupt("listing item in transient state")),
            }
        }
    }

    /// Classify a freshly-parsed FE: pending AED continuation, pending
    /// symlink payload, or ready.
    fn after_fe(&self, fe: Box<FeInfo>) -> Result<ItemState> {
        if let FeData::Ads {
            next: Some((part, lb)),
            ..
        } = &fe.data
        {
            let extents = self.resolve(*part, *lb, self.block)?;
            let range = single_recorded(&extents)?;
            return Ok(ItemState::NeedAed {
                part: *part,
                range,
                fe,
            });
        }
        if fe.file_type == FT_SYMLINK {
            match &fe.data {
                FeData::Inline(data) => {
                    let target = parse_path_components(data);
                    return Ok(ItemState::Ready { fe, target });
                }
                FeData::Ads { ads, .. } => {
                    if fe.info_len > MAX_SYMLINK_BYTES {
                        return Err(corrupt("symlink payload unreasonably large"));
                    }
                    let mut extents = Vec::new();
                    for ad in ads {
                        match ad.kind {
                            ExtentKind::Recorded => {
                                extents.extend(self.resolve(ad.part, ad.lb, ad.len)?)
                            }
                            ExtentKind::Sparse => extents.push(Extent {
                                offset: 0,
                                len: ad.len,
                                kind: ExtentKind::Sparse,
                            }),
                        }
                    }
                    return Ok(ItemState::NeedSymlink { fe, extents });
                }
            }
        }
        Ok(ItemState::Ready { fe, target: None })
    }

    fn finalize_entry(&self, fid: Fid, fe: FeInfo, target: Option<String>) -> Result<Entry> {
        let kind = match fe.file_type {
            FT_DIR => EntryKind::Dir,
            FT_SYMLINK => EntryKind::Symlink,
            _ => EntryKind::File,
        };
        let data = fe_data_to_entry_data(&fe, &self.partitions, self.block)?;
        Ok(Entry {
            name: fid.name,
            kind,
            size: fe.info_len,
            data,
            mode: Some(fe.mode),
            uid: fe.uid,
            gid: fe.gid,
            nlink: Some(fe.nlink),
            modified: fe.modified,
            accessed: fe.accessed,
            created: fe.created,
            link_target: target,
            hidden: fid.hidden,
        })
    }
}
