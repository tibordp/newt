//! End-to-end parser tests over real generated images (see
//! `fixtures/regenerate.sh`). The driver here is the reference for how a
//! caller runs the sans-IO loop against random-access storage.

use std::io::Read;

use newt_disc::{Chunk, DiscError, DiscFs, Entry, EntryData, EntryKind, ExtentKind, ProbeOp, Step};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/fixtures/{}.iso.gz", env!("CARGO_MANIFEST_DIR"), name);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{}: {}", path, e));
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(file)
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn serve(image: &[u8], ranges: Vec<std::ops::Range<u64>>) -> Vec<Chunk> {
    ranges
        .into_iter()
        .map(|r| Chunk {
            offset: r.start,
            data: image[r.start as usize..r.end as usize].to_vec(),
        })
        .collect()
}

fn try_probe(image: &[u8]) -> Result<DiscFs, DiscError> {
    let mut op = ProbeOp::new(image.len() as u64);
    let mut fetched = Vec::new();
    loop {
        match op.step(fetched)? {
            Step::Done(fs) => return Ok(fs),
            Step::Need(ranges) => fetched = serve(image, ranges),
        }
    }
}

fn probe(image: &[u8]) -> DiscFs {
    try_probe(image).unwrap()
}

fn list(image: &[u8], fs: &DiscFs, dir: &Entry) -> Vec<Entry> {
    let mut op = fs.list_dir(dir);
    let mut fetched = Vec::new();
    loop {
        match op.step(fetched).unwrap() {
            Step::Done(mut entries) => {
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                return entries;
            }
            Step::Need(ranges) => fetched = serve(image, ranges),
        }
    }
}

fn find<'e>(entries: &'e [Entry], name: &str) -> &'e Entry {
    entries.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "no entry {:?}; have {:?}",
            name,
            entries.iter().map(|e| &e.name).collect::<Vec<_>>()
        )
    })
}

fn read_content(image: &[u8], entry: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    match &entry.data {
        EntryData::Inline(data) => out.extend_from_slice(data),
        EntryData::Extents(extents) => {
            for e in extents {
                match e.kind {
                    ExtentKind::Recorded => out
                        .extend_from_slice(&image[e.offset as usize..(e.offset + e.len) as usize]),
                    ExtentKind::Sparse => out.resize(out.len() + e.len as usize, 0),
                }
            }
        }
    }
    out.truncate(entry.size as usize);
    out
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in data {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

const HELLO: &[u8] = b"Hello from the disc image!\n";
const BIG_FNV: u64 = 0x4155a52457fe334;

/// Walk `sub/deeper/deep.txt` from the root, checking content on the way.
fn check_tree(image: &[u8], fs: &DiscFs, root_entries: &[Entry], sub: &str) {
    let sub = find(root_entries, sub);
    assert_eq!(sub.kind, EntryKind::Dir);
    let sub_entries = list(image, fs, sub);
    let nested = find(&sub_entries, "nested.txt");
    assert_eq!(read_content(image, nested), b"nested\n");
    let deeper = find(&sub_entries, "deeper");
    assert_eq!(deeper.kind, EntryKind::Dir);
    let deeper_entries = list(image, fs, deeper);
    let deep = find(&deeper_entries, "deep.txt");
    assert_eq!(read_content(image, deep), b"deep file content\n");
}

#[test]
fn plain_iso9660() {
    let image = fixture("plain");
    let fs = probe(&image);
    assert_eq!(fs.describe(), "ISO 9660");
    assert_eq!(fs.volume_label().as_deref(), Some("NEWTTEST"));
    let entries = list(&image, &fs, &fs.root().clone());
    // Plain ISO level: names uppercased/mangled by the mastering tool.
    let hello = find(&entries, "HELLO.TXT");
    assert_eq!(hello.kind, EntryKind::File);
    assert_eq!(read_content(&image, hello), HELLO);
    let big = find(&entries, "BIG.BIN");
    assert_eq!(big.size, 65536);
    assert_eq!(fnv1a(&read_content(&image, big)), BIG_FNV);
    assert!(entries.iter().all(|e| e.modified.is_some()));

    let sub = find(&entries, "SUB");
    let sub_entries = list(&image, &fs, sub);
    let nested = find(&sub_entries, "NESTED.TXT");
    assert_eq!(read_content(&image, nested), b"nested\n");
}

#[test]
fn joliet() {
    let image = fixture("joliet");
    let fs = probe(&image);
    assert_eq!(fs.describe(), "ISO 9660 (Joliet)");
    let entries = list(&image, &fs, &fs.root().clone());
    assert_eq!(read_content(&image, find(&entries, "hello.txt")), HELLO);
    assert_eq!(
        fnv1a(&read_content(&image, find(&entries, "big.bin"))),
        BIG_FNV
    );
    // UCS-2 names survive.
    find(&entries, "Ünïcødé nämé.txt");
    assert!(find(&entries, ".hidden.txt").hidden);
    check_tree(&image, &fs, &entries, "sub");
}

#[test]
fn rock_ridge() {
    let image = fixture("rockridge");
    let fs = probe(&image);
    assert_eq!(fs.describe(), "ISO 9660 (Rock Ridge)");
    assert_eq!(fs.volume_label().as_deref(), Some("NEWTRR"));
    let entries = list(&image, &fs, &fs.root().clone());

    let hello = find(&entries, "hello.txt");
    assert_eq!(read_content(&image, hello), HELLO);
    assert_eq!(hello.mode, Some(0o444));
    assert_eq!(
        fnv1a(&read_content(&image, find(&entries, "big.bin"))),
        BIG_FNV
    );

    // Symlinks: relative, directory, absolute.
    let link = find(&entries, "link_to_hello");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.link_target.as_deref(), Some("hello.txt"));
    assert_eq!(link.mode, Some(0o555));
    assert_eq!(
        find(&entries, "link_to_deeper").link_target.as_deref(),
        Some("sub/deeper")
    );
    assert_eq!(
        find(&entries, "link_abs").link_target.as_deref(),
        Some("/sub/nested.txt")
    );

    // NM entries.
    find(
        &entries,
        "a_rather_long_rock_ridge_file_name_that_exceeds_iso_level_one.txt",
    );
    // CE continuation areas: name + symlink target too large for the
    // 255-byte directory record.
    let very_long = format!("prefix_{}_suffix.txt", "x".repeat(180));
    let vl = find(&entries, &very_long);
    assert_eq!(read_content(&image, vl), b"very long name\n");
    let long_target: String = format!(
        "sub/{}",
        (0..8)
            .map(|i| format!("seg{}", i.to_string().repeat(20)))
            .collect::<Vec<_>>()
            .join("/")
    );
    assert_eq!(
        find(&entries, "link_long").link_target.as_deref(),
        Some(long_target.as_str())
    );

    check_tree(&image, &fs, &entries, "sub");
}

#[test]
fn udf_150() {
    let image = fixture("udf150");
    let fs = probe(&image);
    assert_eq!(fs.describe(), "UDF 1.50");
    assert_eq!(fs.volume_label().as_deref(), Some("NEWTTEST"));
    let entries = list(&image, &fs, &fs.root().clone());
    let hello = find(&entries, "hello.txt");
    assert_eq!(read_content(&image, hello), HELLO);
    assert_eq!(hello.mode, Some(0o644));
    assert_eq!(
        fnv1a(&read_content(&image, find(&entries, "big.bin"))),
        BIG_FNV
    );
    find(&entries, "Ünïcødé nämé.txt");
    check_tree(&image, &fs, &entries, "sub");
}

/// UDF 2.50: metadata partition, 512-byte sectors (hard-disk profile
/// media), symlinks.
#[test]
fn udf_250_metadata_partition() {
    let image = fixture("udf250");
    let fs = probe(&image);
    assert_eq!(fs.describe(), "UDF 2.50");
    assert_eq!(fs.volume_label().as_deref(), Some("NEWTUDF250"));
    let entries = list(&image, &fs, &fs.root().clone());

    let hello = find(&entries, "hello.txt");
    assert_eq!(read_content(&image, hello), HELLO);
    // File data extents resolve through the physical partition even though
    // the FEs live in the metadata partition.
    assert_eq!(
        fnv1a(&read_content(&image, find(&entries, "big.bin"))),
        BIG_FNV
    );

    let link = find(&entries, "link_to_hello");
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.link_target.as_deref(), Some("hello.txt"));
    assert_eq!(
        find(&entries, "link_to_deeper").link_target.as_deref(),
        Some("sub/deeper")
    );

    check_tree(&image, &fs, &entries, "sub");
}

/// A bridge image carrying ISO 9660 + Joliet + UDF: the UDF view wins.
#[test]
fn hybrid_prefers_udf() {
    let image = fixture("hybrid");
    let fs = probe(&image);
    assert_eq!(fs.describe(), "UDF 1.02");
    let entries = list(&image, &fs, &fs.root().clone());
    assert_eq!(read_content(&image, find(&entries, "hello.txt")), HELLO);
    find(&entries, "Ünïcødé nämé.txt");
    check_tree(&image, &fs, &entries, "sub");
}

#[test]
fn not_a_disc() {
    assert_eq!(try_probe(&[0u8; 200_000]).unwrap_err(), DiscError::NotADisc);
    assert_eq!(try_probe(b"hello").unwrap_err(), DiscError::NotADisc);
    // A ZIP-like blob big enough to reach the descriptor area.
    let mut blob = vec![0x50u8; 400_000];
    blob[0] = b'P';
    assert_eq!(try_probe(&blob).unwrap_err(), DiscError::NotADisc);
}

// ---------------------------------------------------------------------------
// Synthetic minimal ISO — exercises multi-extent files and hidden flags,
// which no common mastering tool produces on demand.
// ---------------------------------------------------------------------------

const S: usize = 2048;

fn both_u32(v: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&v.to_le_bytes());
    b[4..].copy_from_slice(&v.to_be_bytes());
    b
}

fn both_u16(v: u16) -> [u8; 4] {
    let mut b = [0u8; 4];
    b[..2].copy_from_slice(&v.to_le_bytes());
    b[2..].copy_from_slice(&v.to_be_bytes());
    b
}

fn dir_record(name: &[u8], lba: u32, len: u32, flags: u8) -> Vec<u8> {
    let name_len = name.len();
    let mut rec = vec![0u8; 33 + name_len + (1 - name_len % 2)];
    rec[0] = rec.len() as u8;
    rec[2..10].copy_from_slice(&both_u32(lba));
    rec[10..18].copy_from_slice(&both_u32(len));
    // Recording time: 2020-01-02 03:04:05 UTC.
    rec[18..25].copy_from_slice(&[120, 1, 2, 3, 4, 5, 0]);
    rec[25] = flags;
    rec[28..32].copy_from_slice(&both_u16(1));
    rec[32] = name_len as u8;
    rec[33..33 + name_len].copy_from_slice(name);
    rec
}

/// Build a minimal single-directory ISO with one file split over two
/// extents (multi-extent flag) and one hidden file.
fn synthetic_multiextent_iso() -> Vec<u8> {
    // Layout: sectors 16 PVD, 17 terminator, 18 root dir,
    // 19 part1, 20 part2, 21 hidden file.
    let mut image = vec![0u8; 22 * S];

    let part1 = vec![0xAA; S];
    let part2 = vec![0xBB; 100];
    image[19 * S..20 * S].copy_from_slice(&part1);
    image[20 * S..20 * S + 100].copy_from_slice(&part2);
    image[21 * S..21 * S + 7].copy_from_slice(b"secret\n");

    // Root directory records.
    let mut root = Vec::new();
    root.extend(dir_record(&[0x00], 18, S as u32, 0x02)); // "."
    root.extend(dir_record(&[0x01], 18, S as u32, 0x02)); // ".."
    root.extend(dir_record(b"SPLIT.BIN;1", 19, S as u32, 0x80)); // continues
    root.extend(dir_record(b"SPLIT.BIN;1", 20, 100, 0x00)); // final part
    root.extend(dir_record(b"SECRET.TXT;1", 21, 7, 0x01)); // hidden
    image[18 * S..18 * S + root.len()].copy_from_slice(&root);

    // PVD.
    let mut pvd = vec![0u8; S];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[40..48].copy_from_slice(b"SYNTHTIC");
    pvd[128..132].copy_from_slice(&both_u16(2048));
    let root_rec = dir_record(&[0x00], 18, S as u32, 0x02);
    pvd[156..156 + root_rec.len()].copy_from_slice(&root_rec);
    image[16 * S..17 * S].copy_from_slice(&pvd);

    // Terminator.
    image[17 * S] = 255;
    image[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
    image[17 * S + 6] = 1;

    image
}

#[test]
fn synthetic_multi_extent() {
    let image = synthetic_multiextent_iso();
    let fs = probe(&image);
    assert_eq!(fs.describe(), "ISO 9660");
    let entries = list(&image, &fs, &fs.root().clone());
    assert_eq!(entries.len(), 2);

    let split = find(&entries, "SPLIT.BIN");
    assert_eq!(split.size, 2048 + 100);
    let content = read_content(&image, split);
    assert_eq!(content.len(), 2148);
    assert!(content[..2048].iter().all(|&b| b == 0xAA));
    assert!(content[2048..].iter().all(|&b| b == 0xBB));

    let secret = find(&entries, "SECRET.TXT");
    assert!(secret.hidden);
    assert_eq!(read_content(&image, secret), b"secret\n");
    // Recording timestamp decoded: 2020-01-02 03:04:05 UTC.
    assert_eq!(secret.modified, Some(1_577_934_245_000));
}
