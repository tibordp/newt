//! Sans-IO ISO 9660 / UDF disc image reader.
//!
//! No IO trait anywhere: every multi-read operation is a small state machine
//! that hands the caller absolute byte ranges of the image it needs next and
//! consumes the fetched bytes on the following `step`. All ranges in one
//! `Step::Need` batch are independent, so an async caller can fetch them
//! concurrently — on a high-latency backend (an image on S3) a directory
//! listing costs a few round-trips regardless of entry count.
//!
//! Parsed entries address file content as absolute image extents, so reading
//! a file out of the image is pure range arithmetic for the caller; no
//! decompression, no buffering. Malformed images are user data, never a
//! programming error: everything is a `DiscError`, nothing panics.

// `Step::Need` legitimately carries single-range batches.
#![allow(clippy::single_range_in_vec_init)]

mod iso9660;
mod rockridge;
mod udf;

use std::collections::HashMap;
use std::ops::Range;

/// Logical sector size of an `.iso` image. UDF logical block size is read
/// from the volume descriptors but is 2048 for disc images in practice.
pub const SECTOR: u64 = 2048;

/// Directory data larger than this is treated as corrupt rather than
/// buffered (a directory this big would itself be a pathology).
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;

/// Symlink target payloads larger than this are corrupt.
const MAX_SYMLINK_BYTES: u64 = 64 * 1024;

/// Upper bound on `step` rounds for a single operation — a backstop against
/// crafted images that chain continuations forever.
const MAX_ROUNDS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscError {
    /// Not ISO 9660 and no UDF anchor — this file is not a disc image.
    NotADisc,
    /// Recognized but outside supported scope (VAT/sparable partitions, …).
    Unsupported(String),
    /// Structurally invalid image data.
    Corrupt(String),
}

impl std::fmt::Display for DiscError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscError::NotADisc => write!(f, "not an ISO 9660 or UDF disc image"),
            DiscError::Unsupported(m) => write!(f, "unsupported disc image feature: {}", m),
            DiscError::Corrupt(m) => write!(f, "corrupt disc image: {}", m),
        }
    }
}

impl std::error::Error for DiscError {}

pub type Result<T> = std::result::Result<T, DiscError>;

fn corrupt(msg: impl Into<String>) -> DiscError {
    DiscError::Corrupt(msg.into())
}

/// One fetched byte range, exactly as requested.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub offset: u64,
    pub data: Vec<u8>,
}

/// Progress of a sans-IO operation. `Need` lists absolute image byte ranges
/// the caller must fetch (they may be fetched concurrently) and feed to the
/// next `step` call; ranges are already validated to lie within the image.
#[derive(Debug)]
pub enum Step<T> {
    Need(Vec<Range<u64>>),
    Done(T),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentKind {
    Recorded,
    /// Allocated-but-unrecorded (UDF): reads as zeros.
    Sparse,
}

/// A run of file content, in absolute image byte offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    pub offset: u64,
    pub len: u64,
    pub kind: ExtentKind,
}

#[derive(Debug, Clone)]
pub enum EntryData {
    Extents(Vec<Extent>),
    /// Data embedded in the metadata structure itself (UDF inline ICBs,
    /// resolved symlink payloads).
    Inline(Vec<u8>),
}

/// A directory entry. For directories, `data` addresses the directory's own
/// content (record area / FID stream) and `size` is that content's length.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub data: EntryData,
    /// Permission bits (no file-type bits).
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub nlink: Option<u32>,
    /// Milliseconds since the Unix epoch.
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
    pub link_target: Option<String>,
    pub hidden: bool,
}

impl Entry {
    fn dir(name: String, data: EntryData, size: u64) -> Self {
        Entry {
            name,
            kind: EntryKind::Dir,
            size,
            data,
            mode: None,
            uid: None,
            gid: None,
            nlink: None,
            modified: None,
            accessed: None,
            created: None,
            link_target: None,
            hidden: false,
        }
    }
}

/// A parsed disc filesystem: everything needed to list directories and map
/// file reads, with no further volume-level reads.
#[derive(Debug)]
pub enum DiscFs {
    Iso(iso9660::IsoFs),
    Udf(udf::UdfFs),
}

impl DiscFs {
    pub fn root(&self) -> &Entry {
        match self {
            DiscFs::Iso(fs) => &fs.root,
            DiscFs::Udf(fs) => &fs.root,
        }
    }

    /// Human-readable flavor, e.g. "ISO 9660 (Rock Ridge)" or "UDF 2.50".
    pub fn describe(&self) -> String {
        match self {
            DiscFs::Iso(fs) => fs.describe(),
            DiscFs::Udf(fs) => fs.describe(),
        }
    }

    pub fn volume_label(&self) -> Option<String> {
        match self {
            DiscFs::Iso(fs) => fs.volume_label.clone(),
            DiscFs::Udf(fs) => fs.volume_label.clone(),
        }
    }

    /// Begin listing the contents of `dir` (which must be a directory entry
    /// produced by this filesystem; the root comes from [`DiscFs::root`]).
    pub fn list_dir(&self, dir: &Entry) -> ListDirOp<'_> {
        ListDirOp::new(self, dir)
    }
}

// ---------------------------------------------------------------------------
// Fetch bookkeeping shared by the operations
// ---------------------------------------------------------------------------

/// Requested ranges and their fetched bytes. Operations request ranges,
/// remember them, and read them back once supplied; `take`/`get` fail if the
/// caller didn't honor the contract.
#[derive(Debug, Default)]
struct Store {
    chunks: HashMap<u64, Vec<u8>>,
}

impl Store {
    fn supply(&mut self, fetched: Vec<Chunk>) {
        for c in fetched {
            self.chunks.insert(c.offset, c.data);
        }
    }

    fn try_get(&self, range: &Range<u64>) -> Option<&[u8]> {
        let data = self.chunks.get(&range.start)?;
        let len = (range.end - range.start) as usize;
        data.get(..len)
    }

    fn get(&self, range: &Range<u64>) -> Result<&[u8]> {
        let data = self
            .chunks
            .get(&range.start)
            .ok_or_else(|| corrupt(format!("range at {} was not supplied", range.start)))?;
        let len = (range.end - range.start) as usize;
        if data.len() < len {
            return Err(corrupt(format!(
                "short read at {}: got {} of {} bytes",
                range.start,
                data.len(),
                len
            )));
        }
        Ok(&data[..len])
    }

    /// Concatenate the recorded extents of `extents`, materializing sparse
    /// runs as zeros. All recorded ranges must have been supplied.
    fn concat_extents(&self, extents: &[Extent]) -> Result<Vec<u8>> {
        let total: u64 = extents.iter().map(|e| e.len).sum();
        let mut out = Vec::with_capacity(total as usize);
        for e in extents {
            match e.kind {
                ExtentKind::Recorded => {
                    out.extend_from_slice(self.get(&(e.offset..e.offset + e.len))?)
                }
                ExtentKind::Sparse => out.resize(out.len() + e.len as usize, 0),
            }
        }
        Ok(out)
    }
}

fn recorded_ranges(extents: &[Extent]) -> Vec<Range<u64>> {
    extents
        .iter()
        .filter(|e| e.kind == ExtentKind::Recorded)
        .map(|e| e.offset..e.offset + e.len)
        .collect()
}

// ---------------------------------------------------------------------------
// Byte-level helpers (checked slicing — malformed input must never panic)
// ---------------------------------------------------------------------------

fn slice(buf: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    buf.get(
        off..off
            .checked_add(len)
            .ok_or_else(|| corrupt("offset overflow"))?,
    )
    .ok_or_else(|| corrupt(format!("structure truncated at offset {}", off)))
}

fn u16_le(buf: &[u8], off: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(slice(buf, off, 2)?.try_into().unwrap()))
}

fn u32_le(buf: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(slice(buf, off, 4)?.try_into().unwrap()))
}

fn u64_le(buf: &[u8], off: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(slice(buf, off, 8)?.try_into().unwrap()))
}

/// Civil date/time (+ timezone offset in minutes east of UTC) → Unix epoch
/// milliseconds. Returns `None` for out-of-range field values.
#[allow(clippy::too_many_arguments)]
pub(crate) fn epoch_ms(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    millis: u32,
    tz_minutes: i32,
) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = i64::from(year) - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + i64::from(hour) * 3_600 + i64::from(min) * 60 + i64::from(sec)
        - i64::from(tz_minutes) * 60;
    Some(secs * 1_000 + i64::from(millis))
}

// ---------------------------------------------------------------------------
// ProbeOp — volume recognition
// ---------------------------------------------------------------------------

/// Probes an image: UDF (preferred — the authoritative view on hybrid/bridge
/// discs, and the only one correct for >4 GB files) falling back to ISO 9660
/// with Rock Ridge > Joliet > plain precedence.
pub struct ProbeOp {
    image_size: u64,
    store: Store,
    rounds: usize,
    state: ProbeState,
}

enum ProbeState {
    Start,
    /// VD window + anchor candidates requested. Anchor candidates carry
    /// (range, expected sector number, sector size).
    Recognize {
        vd_range: Range<u64>,
        anchor_candidates: Vec<(Range<u64>, u64, u64)>,
    },
    /// UDF confirmed; the UDF sub-machine owns further rounds.
    Udf(Box<udf::UdfProbe>),
    /// ISO confirmed; waiting on the root directory's first sector for
    /// Rock Ridge detection.
    IsoRoot {
        fs: Box<iso9660::IsoFs>,
        root_range: Range<u64>,
    },
    Finished,
}

impl ProbeOp {
    pub fn new(image_size: u64) -> Self {
        ProbeOp {
            image_size,
            store: Store::default(),
            rounds: 0,
            state: ProbeState::Start,
        }
    }

    pub fn step(&mut self, fetched: Vec<Chunk>) -> Result<Step<DiscFs>> {
        self.rounds += 1;
        if self.rounds > MAX_ROUNDS {
            return Err(corrupt("probe did not converge"));
        }
        self.store.supply(fetched);

        match std::mem::replace(&mut self.state, ProbeState::Finished) {
            ProbeState::Start => {
                if self.image_size < 17 * SECTOR {
                    return Err(DiscError::NotADisc);
                }
                // ISO volume descriptor window: sectors 16..48 (the VD
                // sequence is a handful of descriptors; 32 sectors is
                // beyond generous).
                let vd_end = (48 * SECTOR).min(self.image_size);
                let vd_range = 16 * SECTOR..vd_end;
                // UDF anchor candidates: sector 256, N-256, N-1 — at every
                // supported sector size, canonical 2048 first. An AVDP fits
                // well within 512 bytes.
                let mut anchor_candidates: Vec<(Range<u64>, u64, u64)> = Vec::new();
                for &ss in &[2048u64, 512, 1024, 4096] {
                    let n = self.image_size / ss;
                    for s in [256, n.saturating_sub(256), n.saturating_sub(1)] {
                        if s >= 256 && (s + 1) * ss <= self.image_size {
                            anchor_candidates.push((s * ss..s * ss + 512, s, ss));
                        }
                    }
                }
                let mut needs = vec![vd_range.clone()];
                let mut seen = std::collections::HashSet::new();
                for (r, _, _) in &anchor_candidates {
                    if seen.insert(r.start) {
                        needs.push(r.clone());
                    }
                }
                self.state = ProbeState::Recognize {
                    vd_range,
                    anchor_candidates,
                };
                Ok(Step::Need(needs))
            }

            ProbeState::Recognize {
                vd_range,
                anchor_candidates,
            } => {
                // UDF first: a valid anchor descriptor wins.
                let mut found = None;
                for (r, sector_no, ss) in &anchor_candidates {
                    let buf = self.store.get(r)?;
                    if let Some(a) = udf::parse_avdp(buf, *sector_no) {
                        found = Some((a, *ss));
                        break;
                    }
                }
                if let Some((avdp, ss)) = found {
                    let sub = Box::new(udf::UdfProbe::new(avdp, self.image_size, ss));
                    let needs = self.validate_needs(sub.initial_needs())?;
                    self.state = ProbeState::Udf(sub);
                    return Ok(Step::Need(needs));
                }

                // ISO 9660: scan the VD window.
                let buf = self.store.get(&vd_range)?;
                let vds = iso9660::parse_volume_descriptors(buf)?;
                let fs = iso9660::IsoFs::from_descriptors(vds, self.image_size)?;
                // Rock Ridge lives in the system-use area of the primary
                // root's "." record — fetch the root directory's first
                // sector to detect it.
                let root_range = fs.primary_root_probe_range(self.image_size)?;
                self.state = ProbeState::IsoRoot {
                    fs: Box::new(fs),
                    root_range: root_range.clone(),
                };
                Ok(Step::Need(vec![root_range]))
            }

            ProbeState::Udf(mut sub) => match sub.step(&self.store)? {
                Step::Need(needs) => {
                    self.state = ProbeState::Udf(sub);
                    let needs = self.validate_needs(needs)?;
                    Ok(Step::Need(needs))
                }
                Step::Done(fs) => Ok(Step::Done(DiscFs::Udf(fs))),
            },

            ProbeState::IsoRoot { mut fs, root_range } => {
                let buf = self.store.get(&root_range)?;
                fs.detect_rock_ridge(buf);
                Ok(Step::Done(DiscFs::Iso(*fs)))
            }

            ProbeState::Finished => Err(corrupt("probe stepped after completion")),
        }
    }

    fn validate_needs(&self, needs: Vec<Range<u64>>) -> Result<Vec<Range<u64>>> {
        validate_needs(needs, self.image_size)
    }
}

fn validate_needs(needs: Vec<Range<u64>>, image_size: u64) -> Result<Vec<Range<u64>>> {
    for r in &needs {
        if r.end > image_size || r.start >= r.end {
            return Err(corrupt(format!(
                "structure points outside the image: {}..{} (image is {} bytes)",
                r.start, r.end, image_size
            )));
        }
    }
    Ok(needs)
}

// ---------------------------------------------------------------------------
// ListDirOp — directory listing
// ---------------------------------------------------------------------------

/// Progress of a format-specific listing sub-machine. `Need` with an empty
/// range list means "advance me again without new reads".
pub(crate) enum ListProgress<S> {
    Need(S, Vec<Range<u64>>),
    Done(Vec<Entry>),
}

/// Lists one directory. Rounds: directory data → (ISO) Rock Ridge
/// continuation areas / (UDF) per-entry ICBs, symlink payloads. All needs in
/// a round are independent and may be fetched concurrently.
pub struct ListDirOp<'fs> {
    fs: &'fs DiscFs,
    store: Store,
    rounds: usize,
    state: ListState,
}

enum ListState {
    Start { dir: Entry },
    Iso(iso9660::IsoListState),
    Udf(udf::UdfListState),
    Finished,
}

impl<'fs> ListDirOp<'fs> {
    fn new(fs: &'fs DiscFs, dir: &Entry) -> Self {
        ListDirOp {
            fs,
            store: Store::default(),
            rounds: 0,
            state: ListState::Start { dir: dir.clone() },
        }
    }

    pub fn step(&mut self, fetched: Vec<Chunk>) -> Result<Step<Vec<Entry>>> {
        self.rounds += 1;
        if self.rounds > MAX_ROUNDS {
            return Err(corrupt("directory listing did not converge"));
        }
        self.store.supply(fetched);

        let image_size = match self.fs {
            DiscFs::Iso(fs) => fs.image_size,
            DiscFs::Udf(fs) => fs.image_size,
        };

        loop {
            let progress = match std::mem::replace(&mut self.state, ListState::Finished) {
                ListState::Start { dir } => {
                    if dir.kind != EntryKind::Dir {
                        return Err(corrupt("list_dir on a non-directory entry"));
                    }
                    if dir.size > MAX_DIR_BYTES {
                        return Err(corrupt("directory data unreasonably large"));
                    }
                    match self.fs {
                        DiscFs::Iso(fs) => wrap_iso(fs.begin_list(dir)?),
                        DiscFs::Udf(fs) => wrap_udf(fs.begin_list(dir)?),
                    }
                }
                ListState::Iso(state) => match self.fs {
                    DiscFs::Iso(fs) => wrap_iso(fs.continue_list(state, &self.store)?),
                    DiscFs::Udf(_) => return Err(corrupt("listing state mismatch")),
                },
                ListState::Udf(state) => match self.fs {
                    DiscFs::Udf(fs) => wrap_udf(fs.continue_list(state, &self.store)?),
                    DiscFs::Iso(_) => return Err(corrupt("listing state mismatch")),
                },
                ListState::Finished => return Err(corrupt("listing stepped after completion")),
            };
            match progress {
                ListProgress::Done(entries) => return Ok(Step::Done(entries)),
                ListProgress::Need(state, needs) if needs.is_empty() => {
                    // Sub-machine can advance without reads (e.g. inline
                    // directory data) — loop instead of a wasted round-trip.
                    self.state = state;
                    self.rounds += 1;
                    if self.rounds > MAX_ROUNDS {
                        return Err(corrupt("directory listing did not converge"));
                    }
                }
                ListProgress::Need(state, needs) => {
                    self.state = state;
                    return Ok(Step::Need(validate_needs(needs, image_size)?));
                }
            }
        }
    }
}

fn wrap_iso(p: ListProgress<iso9660::IsoListState>) -> ListProgress<ListState> {
    match p {
        ListProgress::Need(s, n) => ListProgress::Need(ListState::Iso(s), n),
        ListProgress::Done(e) => ListProgress::Done(e),
    }
}

fn wrap_udf(p: ListProgress<udf::UdfListState>) -> ListProgress<ListState> {
    match p {
        ListProgress::Need(s, n) => ListProgress::Need(ListState::Udf(s), n),
        ListProgress::Done(e) => ListProgress::Done(e),
    }
}
