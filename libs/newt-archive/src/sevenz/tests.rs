//! Reader tests against `fixtures/*.7z`, written by 7-Zip (see
//! `fixtures/regenerate.py` for the layouts and switches). Folder decoding
//! goes through iluvatar exactly as the VFS will drive it, over in-memory
//! byte slices.

use std::sync::Arc;

use super::*;
use iluvatar::{
    Bcj2Streams, EngineRequest, FixedInterval, StreamIndex, StreamIndexer, StreamReader,
};

const MTIME_MS: i64 = 1_700_000_000_000;

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "basic" => include_bytes!("fixtures/basic.7z"),
        "solid" => include_bytes!("fixtures/solid.7z"),
        "blocks" => include_bytes!("fixtures/blocks.7z"),
        "nonsolid" => include_bytes!("fixtures/nonsolid.7z"),
        "bigdict" => include_bytes!("fixtures/bigdict.7z"),
        "times" => include_bytes!("fixtures/times.7z"),
        "lzma" => include_bytes!("fixtures/lzma.7z"),
        "copy" => include_bytes!("fixtures/copy.7z"),
        "bzip2" => include_bytes!("fixtures/bzip2.7z"),
        "deflate" => include_bytes!("fixtures/deflate.7z"),
        "zstd" => include_bytes!("fixtures/zstd.7z"),
        "ppmd" => include_bytes!("fixtures/ppmd.7z"),
        "x86" => include_bytes!("fixtures/x86.7z"),
        "arm" => include_bytes!("fixtures/arm.7z"),
        "armt" => include_bytes!("fixtures/armt.7z"),
        "ppc" => include_bytes!("fixtures/ppc.7z"),
        "sparc" => include_bytes!("fixtures/sparc.7z"),
        "ia64" => include_bytes!("fixtures/ia64.7z"),
        "arm64" => include_bytes!("fixtures/arm64.7z"),
        "riscv" => include_bytes!("fixtures/riscv.7z"),
        "bcj2" => include_bytes!("fixtures/bcj2.7z"),
        "bcj2_tails" => include_bytes!("fixtures/bcj2_tails.7z"),
        "bcj2_encrypted" => include_bytes!("fixtures/bcj2_encrypted.7z"),
        "deflate64" => include_bytes!("fixtures/deflate64.7z"),
        "delta" => include_bytes!("fixtures/delta.7z"),
        "encrypted" => include_bytes!("fixtures/encrypted.7z"),
        "encrypted_header" => include_bytes!("fixtures/encrypted_header.7z"),
        _ => panic!("no fixture {}", name),
    }
}

// ─── Content generators, mirrored from regenerate.py ───

fn pattern(i: u32, n: usize) -> Vec<u8> {
    (0..n).map(|j| ((j as u32 * 7 + i) % 251) as u8).collect()
}

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 33) as u8
        })
        .collect()
}

fn code_like(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed;
    let mut out = Vec::with_capacity(n + 24);
    while out.len() < n {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let r = (s >> 33) as u32;
        match r % 6 {
            0 => {
                out.push(0xE8);
                out.extend_from_slice(&((r >> 8) % 4096).to_le_bytes());
            }
            1 => out.extend_from_slice(&(0xEB00_0000u32 | (r & 0x00FF_FFFF)).to_be_bytes()),
            2 => out.extend_from_slice(b"the quick brown fox "),
            _ => out.extend_from_slice(&r.to_le_bytes()),
        }
    }
    out.truncate(n);
    out
}

fn small_set() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("hello.txt", b"hello world\n".to_vec()),
        ("dir/nested.txt", b"nested content\n".to_vec()),
        ("dir/big.bin", pattern(3, 100_000)),
        ("empty.txt", Vec::new()),
        ("π — unicode.txt", "unicode ☃\n".as_bytes().to_vec()),
    ]
}

fn solid_set() -> Vec<(String, Vec<u8>)> {
    (0..24)
        .map(|i| {
            if i % 3 == 0 {
                (
                    format!("solid/noise{:02}.bin", i),
                    noise(i as u64 + 1, 40_000 + i * 1_000),
                )
            } else {
                (
                    format!("solid/pat{:02}.bin", i),
                    pattern(i as u32, 50_000 + i * 2_000),
                )
            }
        })
        .collect()
}

// ─── Drivers ───

fn serve(image: &[u8], ranges: &[Range<u64>]) -> Vec<Chunk> {
    ranges
        .iter()
        .map(|r| Chunk {
            offset: r.start,
            data: image[r.start as usize..r.end as usize].to_vec(),
        })
        .collect()
}

fn probe_with(image: &[u8], password: Option<&str>) -> Result<SevenZFs> {
    let mut op = SevenZProbeOp::new(image.len() as u64);
    let mut fetched = Vec::new();
    loop {
        match op.step(fetched)? {
            ProbeStep::Done(fs) => return Ok(fs),
            ProbeStep::Need(ranges) => fetched = serve(image, &ranges),
            ProbeStep::NeedPassword => {
                let pw = password.ok_or(SevenZError::PasswordRequired)?;
                let key = derive_key(pw, op.aes_params().expect("params"));
                op.set_key(key);
                fetched = Vec::new();
            }
        }
    }
}

fn probe(image: &[u8]) -> Result<SevenZFs> {
    probe_with(image, None)
}

fn entry<'a>(fs: &'a SevenZFs, name: &str) -> &'a SevenZEntry {
    fs.entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no entry {}", name))
}

fn folder_key(fs: &SevenZFs, folder: usize, password: Option<&str>) -> Option<[u8; 32]> {
    let params = fs.folders[folder].aes.as_ref()?;
    Some(derive_key(password?, params))
}

/// A BCJ2 folder's side streams, decoded from the image.
fn sides(
    image: &[u8],
    info: &FolderInfo,
    key: Option<[u8; 32]>,
) -> Result<Option<Arc<Bcj2Streams>>> {
    let Some(b) = &info.bcj2 else {
        return Ok(None);
    };
    let decode = |side: &SideStream| {
        side.decode(
            &image[side.packed.start as usize..side.packed.end as usize],
            key,
        )
    };
    Ok(Some(Arc::new(Bcj2Streams {
        call: decode(&b.call)?,
        jump: decode(&b.jump)?,
        rc: decode(&b.rc)?,
    })))
}

/// Feed packed bytes to an engine request; returns false at end of stream.
fn feed_packed<F: FnMut(&[u8])>(
    image: &[u8],
    packed: &Range<u64>,
    at: u64,
    len: usize,
    mut provide: F,
) -> bool {
    let start = packed.start + at;
    if start >= packed.end {
        return false;
    }
    let end = (start + len as u64).min(packed.end);
    provide(&image[start as usize..end as usize]);
    true
}

fn index_folder(
    image: &[u8],
    fs: &SevenZFs,
    folder: usize,
    password: Option<&str>,
    interval: u64,
) -> Result<StreamIndex> {
    let info = &fs.folders[folder];
    let spec = info.codec_spec(
        folder_key(fs, folder, password),
        sides(image, info, folder_key(fs, folder, password))?,
    )?;
    let mut indexer = StreamIndexer::new(
        spec,
        FixedInterval::new(interval),
        Some(info.packed.end - info.packed.start),
    )
    .map_err(|e| corrupt(e.to_string()))?;
    indexer.set_unpacked_len(info.unpacked_len);
    let mut pos = 0u64;
    loop {
        match indexer.step() {
            EngineRequest::NeedInput => {
                let fed = feed_packed(image, &info.packed, pos, 4096, |d| {
                    pos += d.len() as u64;
                    indexer.provide_data(d);
                });
                if !fed {
                    indexer.signal_eof();
                }
            }
            EngineRequest::SeekAndRead { offset, len } => {
                pos = offset;
                let fed = feed_packed(image, &info.packed, pos, len, |d| {
                    pos += d.len() as u64;
                    indexer.provide_data(d);
                });
                if !fed {
                    indexer.signal_eof();
                }
            }
            EngineRequest::OutputReady => {
                let mut b = [0u8; 4096];
                while indexer.read_output(&mut b) > 0 {}
            }
            EngineRequest::Done => break,
            EngineRequest::Error(e) => return Err(corrupt(e.to_string())),
        }
    }
    Ok(indexer.finish())
}

fn read_with_index(
    image: &[u8],
    fs: &SevenZFs,
    folder: usize,
    index: &StreamIndex,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>> {
    let info = &fs.folders[folder];
    let mut reader = StreamReader::new(index, offset, len).map_err(|e| corrupt(e.to_string()))?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 3000];
    loop {
        match reader.step() {
            EngineRequest::NeedInput => {
                let at = reader.compressed_position();
                let fed = feed_packed(image, &info.packed, at, 4096, |d| reader.provide_data(d));
                if !fed {
                    reader.signal_eof();
                }
            }
            EngineRequest::SeekAndRead { offset, len } => {
                let fed = feed_packed(image, &info.packed, offset, len, |d| reader.provide_data(d));
                if !fed {
                    reader.signal_eof();
                }
            }
            EngineRequest::OutputReady => loop {
                let n = reader.read_output(&mut buf);
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            },
            EngineRequest::Done => return Ok(out),
            EngineRequest::Error(e) => return Err(corrupt(e.to_string())),
        }
    }
}

/// Read an entry through a fresh index (offset-0 checkpoint only).
fn read_entry(image: &[u8], fs: &SevenZFs, name: &str, password: Option<&str>) -> Result<Vec<u8>> {
    let e = entry(fs, name);
    let Some(loc) = e.location else {
        return Ok(Vec::new());
    };
    let info = &fs.folders[loc.folder];
    let spec = info.codec_spec(
        folder_key(fs, loc.folder, password),
        sides(image, info, folder_key(fs, loc.folder, password))?,
    )?;
    let mut index = StreamIndex::new(spec, Some(info.packed.end - info.packed.start));
    index.unpacked_len = Some(info.unpacked_len);
    read_with_index(image, fs, loc.folder, &index, loc.offset, e.size)
}

fn check_set(image: &[u8], fs: &SevenZFs, files: &[(&str, Vec<u8>)], password: Option<&str>) {
    for (name, data) in files {
        let got =
            read_entry(image, fs, name, password).unwrap_or_else(|e| panic!("{}: {}", name, e));
        assert_eq!(got.len(), data.len(), "{} length", name);
        assert!(got == *data, "{} content", name);
        let e = entry(fs, name);
        if let Some(crc) = e.crc32 {
            assert_eq!(crc32fast::hash(data), crc, "{} crc", name);
        }
    }
}

// ─── Tests ───

#[test]
fn basic_lists_entries_with_metadata() {
    let image = fixture("basic");
    let fs = probe(image).unwrap();
    let names: Vec<&str> = fs.entries.iter().map(|e| e.name.as_str()).collect();
    for expected in [
        "hello.txt",
        "dir",
        "dir/nested.txt",
        "dir/big.bin",
        "empty.txt",
        "emptydir",
        "links/soft.txt",
        "π — unicode.txt",
    ] {
        assert!(
            names.contains(&expected),
            "missing {} in {:?}",
            expected,
            names
        );
    }
    assert_eq!(entry(&fs, "dir").kind, EntryKind::Dir);
    assert_eq!(entry(&fs, "emptydir").kind, EntryKind::Dir);
    assert_eq!(entry(&fs, "empty.txt").kind, EntryKind::File);
    assert_eq!(entry(&fs, "empty.txt").size, 0);
    assert!(entry(&fs, "empty.txt").location.is_none());
    assert_eq!(entry(&fs, "links/soft.txt").kind, EntryKind::Symlink);
    let big = entry(&fs, "dir/big.bin");
    assert_eq!(big.kind, EntryKind::File);
    assert_eq!(big.size, 300_000);
    assert_eq!(big.modified, Some(MTIME_MS));
    assert!(
        big.mode.is_some(),
        "unix mode from the attributes' high half"
    );
    assert_eq!(fs.folders.len(), 1, "7-Zip's default is one solid folder");
    assert_eq!(
        read_entry(image, &fs, "links/soft.txt", None).unwrap(),
        b"../hello.txt"
    );
    let mut files = small_set();
    files[2] = ("dir/big.bin", pattern(3, 300_000));
    check_set(image, &fs, &files, None);
}

#[test]
fn single_coder_variants_read_back() {
    for name in ["lzma", "copy", "bzip2", "deflate", "zstd", "delta"] {
        let image = fixture(name);
        let fs = probe(image).unwrap_or_else(|e| panic!("{}: {}", name, e));
        fs.folders[0]
            .supported()
            .unwrap_or_else(|e| panic!("{}: {}", name, e));
        check_set(image, &fs, &small_set(), None);
    }
}

#[test]
fn bcj_folders_read_back() {
    let files = [
        ("prog.bin", code_like(9, 60_000)),
        ("readme.txt", b"code-like payload\n".to_vec()),
    ];
    for name in [
        "x86", "arm", "armt", "ppc", "sparc", "ia64", "arm64", "riscv",
    ] {
        let image = fixture(name);
        let fs = probe(image).unwrap_or_else(|e| panic!("{}: {}", name, e));
        check_set(image, &fs, &files, None);
    }
}

/// BCJ2 folders: the main stream through the chain, the three side
/// streams decoded whole. Trailing markers, checkpoints inside the folder
/// and a password (one AES coder per packed stream) all read back.
#[test]
fn bcj2_folders_read_back() {
    let code = code_like(9, 60_000);
    let image = fixture("bcj2");
    let fs = probe(image).unwrap();
    let info = &fs.folders[0];
    let b = info.bcj2.as_ref().expect("bcj2 sides");
    assert!(b.call.unpacked_len > 0 && b.rc.unpacked_len > 5, "{:?}", b);
    check_set(
        image,
        &fs,
        &[
            ("prog.bin", code.clone()),
            ("readme.txt", b"code-like payload\n".to_vec()),
        ],
        None,
    );
    let index = index_folder(image, &fs, 0, None, 16_384).unwrap();
    assert!(index.checkpoints.len() >= 3, "{}", index.checkpoints.len());
    for (offset, len) in [
        (0u64, 100u64),
        (20_000, 5_000),
        (59_990, 10),
        (40_000, 20_000),
    ] {
        let got = read_with_index(image, &fs, 0, &index, offset, len).unwrap();
        assert_eq!(
            got,
            &code[offset as usize..(offset + len) as usize],
            "{}+{}",
            offset,
            len
        );
    }

    let image = fixture("bcj2_tails");
    let fs = probe(image).unwrap();
    let mut call = code.clone();
    call.truncate(60_000 - 5);
    call.extend_from_slice(&[0xE8, 0x10, 0, 0, 0]);
    let mut e8 = code.clone();
    e8.push(0xE8);
    let mut e8_3 = code.clone();
    e8_3.extend_from_slice(&[0xE8, 1, 2]);
    let mut jcc = code.clone();
    jcc.extend_from_slice(&[0x0F, 0x85]);
    check_set(
        image,
        &fs,
        &[
            ("call.bin", call),
            ("e8.bin", e8),
            ("e8_3.bin", e8_3),
            ("jcc.bin", jcc),
        ],
        None,
    );

    let image = fixture("bcj2_encrypted");
    let fs = probe(image).unwrap();
    assert!(fs.folders[0].aes.is_some());
    assert!(matches!(
        read_entry(image, &fs, "prog.bin", None),
        Err(SevenZError::PasswordRequired)
    ));
    check_set(
        image,
        &fs,
        &[
            ("prog.bin", code),
            ("readme.txt", b"code-like payload\n".to_vec()),
        ],
        Some("secret"),
    );
}

#[test]
fn out_of_scope_coders_list_but_refuse_to_decode() {
    for (name, what, entry_name) in [
        ("ppmd", "PPMd", "hello.txt"),
        ("deflate64", "Deflate64", "hello.txt"),
    ] {
        let image = fixture(name);
        let fs = probe(image).unwrap_or_else(|e| panic!("{}: {}", name, e));
        assert!(fs.entries.iter().any(|e| e.name == entry_name), "{}", name);
        match fs.folders[0].supported() {
            Err(SevenZError::Unsupported(m)) => assert!(m.contains(what), "{}: {}", name, m),
            other => panic!("{}: {:?}", name, other),
        }
        assert!(matches!(
            read_entry(image, &fs, entry_name, None),
            Err(SevenZError::Unsupported(_))
        ));
    }
}

#[test]
fn nonsolid_archives_have_one_folder_per_file() {
    let image = fixture("nonsolid");
    let fs = probe(image).unwrap();
    let with_data = fs.entries.iter().filter(|e| e.location.is_some()).count();
    assert_eq!(with_data, 4);
    assert_eq!(fs.folders.len(), 4);
    assert!(fs.folders.iter().all(|f| f.num_entries == 1));
    check_set(image, &fs, &small_set(), None);
}

#[test]
fn solid_blocks_span_several_folders() {
    let image = fixture("blocks");
    let fs = probe(image).unwrap();
    let files = solid_set();
    assert!(
        fs.folders.len() > 1,
        "expected -ms=200k to make several folders"
    );
    assert_eq!(
        fs.folders.iter().map(|f| f.num_entries).sum::<usize>(),
        files.len()
    );
    for (name, data) in &files {
        let got = read_entry(image, &fs, name, None).unwrap();
        assert!(got == *data, "{}", name);
    }
}

#[test]
fn big_dictionary_costs_the_whole_folder_per_checkpoint() {
    let image = fixture("bigdict");
    let fs = probe(image).unwrap();
    let info = &fs.folders[0];
    assert_eq!(info.checkpoint_cost(), info.unpacked_len);
    check_set(image, &fs, &small_set(), None);
}

#[test]
fn stored_times_include_creation_and_access() {
    let image = fixture("times");
    let fs = probe(image).unwrap();
    let e = entry(&fs, "hello.txt");
    assert_eq!(e.modified, Some(MTIME_MS));
    assert_eq!(e.accessed, Some(MTIME_MS));
    assert!(e.created.is_some());
    let plain = probe(fixture("basic")).unwrap();
    assert!(entry(&plain, "hello.txt").accessed.is_none());
}

#[test]
fn solid_folder_random_access_through_checkpoints() {
    let image = fixture("solid");
    let fs = probe(image).unwrap();
    let files = solid_set();
    assert_eq!(fs.folders.len(), 1);
    let index = index_folder(image, &fs, 0, None, 256 * 1024).unwrap();
    assert!(
        fs.folders[0].checkpoint_cost() >= 1 << 20,
        "LZMA2 checkpoints carry the dictionary"
    );
    assert!(index.complete);
    assert!(
        index.checkpoints.len() >= 4,
        "want checkpoints through the solid folder, got {}",
        index.checkpoints.len()
    );
    // Every entry whole, then slices from the middle of late entries (which
    // must not decode from the folder start: the reader restores the
    // nearest checkpoint).
    for (name, data) in &files {
        let e = entry(&fs, name);
        let loc = e.location.unwrap();
        let got = read_with_index(image, &fs, 0, &index, loc.offset, e.size).unwrap();
        assert!(got == *data, "{}", name);
    }
    let (name, data) = &files[23];
    let e = entry(&fs, name);
    let loc = e.location.unwrap();
    let got = read_with_index(image, &fs, 0, &index, loc.offset + 10_000, 5_000).unwrap();
    assert_eq!(got, &data[10_000..15_000]);
    let used = index.best_checkpoint_for_offset(loc.offset + 10_000).1;
    assert!(
        used.uncompressed_offset > 0,
        "a mid-folder checkpoint was used"
    );
}

#[test]
fn encrypted_data_needs_a_key_and_checks_it() {
    let image = fixture("encrypted");
    let fs = probe(image).unwrap();
    assert!(fs.folders[0].aes.is_some());
    assert_eq!(
        read_entry(image, &fs, "hello.txt", None).unwrap_err(),
        SevenZError::PasswordRequired
    );
    check_set(image, &fs, &small_set(), Some("secret"));
    let wrong = read_entry(image, &fs, "dir/big.bin", Some("wrong"));
    assert!(wrong.is_err() || wrong.unwrap() != pattern(3, 100_000));
}

#[test]
fn encrypted_header_prompts_and_verifies() {
    let image = fixture("encrypted_header");
    assert_eq!(probe(image).unwrap_err(), SevenZError::PasswordRequired);
    assert_eq!(
        probe_with(image, Some("wrong")).unwrap_err(),
        SevenZError::WrongPassword
    );
    // A wrong password does not spoil the probe: set the right key on the
    // same operation and continue.
    let mut op = SevenZProbeOp::new(image.len() as u64);
    let mut fetched = Vec::new();
    let mut tried_wrong = false;
    let fs = loop {
        match op.step(fetched) {
            Ok(ProbeStep::Done(fs)) => break fs,
            Ok(ProbeStep::Need(ranges)) => fetched = serve(image, &ranges),
            Ok(ProbeStep::NeedPassword) => {
                op.set_key(derive_key("wrong", op.aes_params().unwrap()));
                fetched = Vec::new();
            }
            Err(SevenZError::WrongPassword) if !tried_wrong => {
                tried_wrong = true;
                op.set_key(derive_key("secret", op.aes_params().unwrap()));
                fetched = Vec::new();
            }
            Err(e) => panic!("{}", e),
        }
    };
    assert!(tried_wrong);
    check_set(image, &fs, &small_set(), Some("secret"));
}

#[test]
fn not_a_7z_and_truncation() {
    assert_eq!(
        probe(b"PK\x03\x04 not seven zip at all, just bytes").unwrap_err(),
        SevenZError::NotA7z
    );
    let image = fixture("basic");
    assert!(matches!(
        probe(&image[..image.len() - 10]).unwrap_err(),
        SevenZError::Corrupt(_)
    ));
    let mut bad = image.to_vec();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    assert!(matches!(probe(&bad).unwrap_err(), SevenZError::Corrupt(_)));
}
