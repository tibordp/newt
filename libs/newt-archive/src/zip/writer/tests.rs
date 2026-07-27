use std::io::{Cursor, Read};

use super::{civil_from_days, dos_datetime};
use crate::{EntryMeta, ZipWriter};

fn meta(mode: u32, mtime_ms: i64) -> EntryMeta {
    EntryMeta {
        mode: Some(mode),
        mtime_ms: Some(mtime_ms),
        ..Default::default()
    }
}

fn read_archive(bytes: Vec<u8>) -> zip::ZipArchive<Cursor<Vec<u8>>> {
    zip::ZipArchive::new(Cursor::new(bytes)).unwrap()
}

#[test]
fn basic_entries() {
    let payload: Vec<u8> = (0..50_000u32).flat_map(|i| i.to_le_bytes()).collect();
    let mut out = Vec::new();
    let mut w = ZipWriter::new(None, None);
    w.add_directory("dir", &meta(0o750, 1_700_000_000_000), &mut out)
        .unwrap();
    w.begin_file(
        "dir/データ.bin",
        Some(payload.len() as u64),
        &meta(0o640, 1_700_000_000_000),
        &mut out,
    )
    .unwrap();
    for chunk in payload.chunks(64 * 1024) {
        w.write_data(chunk, &mut out).unwrap();
    }
    w.end_file(&mut out).unwrap();
    w.add_symlink(
        "dir/link",
        "データ.bin",
        &meta(0o777, 1_700_000_000_000),
        &mut out,
    )
    .unwrap();
    w.finish(&mut out).unwrap();

    let mut archive = read_archive(out);
    assert_eq!(archive.len(), 3);

    let dir = archive.by_index(0).unwrap();
    assert_eq!(dir.name(), "dir/");
    assert!(dir.is_dir());
    assert_eq!(dir.unix_mode().unwrap() & 0o7777, 0o750);
    drop(dir);

    let mut file = archive.by_index(1).unwrap();
    assert_eq!(file.name(), "dir/データ.bin");
    assert_eq!(file.compression(), zip::CompressionMethod::Deflated);
    assert_eq!(file.unix_mode().unwrap() & 0o7777, 0o640);
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();
    assert_eq!(data, payload);
    drop(file);

    let mut link = archive.by_index(2).unwrap();
    assert!(link.unix_mode().unwrap() & 0o170000 == 0o120000);
    let mut target = String::new();
    link.read_to_string(&mut target).unwrap();
    assert_eq!(target, "データ.bin");
}

#[test]
fn store_mode() {
    let mut out = Vec::new();
    let mut w = ZipWriter::new(Some(0), None);
    w.begin_file("f", Some(5), &meta(0o644, 0), &mut out)
        .unwrap();
    w.write_data(b"hello", &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let mut archive = read_archive(out);
    let mut file = archive.by_index(0).unwrap();
    assert_eq!(file.compression(), zip::CompressionMethod::Stored);
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();
    assert_eq!(data, b"hello");
}

#[test]
fn unknown_size_gets_zip64_framing() {
    // size_hint: None commits to zip64 data-descriptor framing; readers must
    // still accept the entry even though it stays small.
    let mut out = Vec::new();
    let mut w = ZipWriter::new(None, None);
    w.begin_file("f", None, &meta(0o644, 0), &mut out).unwrap();
    w.write_data(b"tiny", &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let mut archive = read_archive(out);
    let mut data = Vec::new();
    archive.by_index(0).unwrap().read_to_end(&mut data).unwrap();
    assert_eq!(data, b"tiny");
}

#[test]
fn zip64_many_entries() {
    // Over the u16 entry-count limit → zip64 EOCD + locator path.
    let mut out = Vec::new();
    let mut w = ZipWriter::new(Some(0), None);
    for i in 0..70_000 {
        w.begin_file(&format!("f{i}"), Some(0), &meta(0o644, 0), &mut out)
            .unwrap();
        w.end_file(&mut out).unwrap();
    }
    w.finish(&mut out).unwrap();

    let mut archive = read_archive(out);
    assert_eq!(archive.len(), 70_000);
    let mut data = Vec::new();
    archive
        .by_index(69_999)
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    assert_eq!(data, b"");
}

#[test]
fn aes_round_trip() {
    let payload: Vec<u8> = (0..50_000u32).flat_map(|i| i.to_le_bytes()).collect();
    let mut out = Vec::new();
    let mut w = ZipWriter::new(None, Some("hunter2"));
    w.begin_file(
        "big.bin",
        Some(payload.len() as u64),
        &meta(0o644, 0),
        &mut out,
    )
    .unwrap();
    for chunk in payload.chunks(64 * 1024) {
        w.write_data(chunk, &mut out).unwrap();
    }
    w.end_file(&mut out).unwrap();
    // Under 20 bytes → AE-1 (real CRC).
    w.begin_file("small.txt", Some(5), &meta(0o644, 0), &mut out)
        .unwrap();
    w.write_data(b"hello", &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.add_symlink("link", "big.bin", &meta(0o777, 0), &mut out)
        .unwrap();
    w.finish(&mut out).unwrap();

    let mut archive = read_archive(out);
    for (index, expected) in [(0usize, &payload[..]), (1, b"hello"), (2, b"big.bin")] {
        let mut file = archive.by_index_decrypt(index, b"hunter2").unwrap();
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();
        assert_eq!(data, expected, "entry {index}");
    }

    // Wrong password must be rejected (via the AES password verifier).
    assert!(archive.by_index_decrypt(0, b"wrong").is_err());
}

#[test]
fn misuse_errors() {
    let mut out = Vec::new();
    let mut w = ZipWriter::new(None, None);
    assert!(w.write_data(b"x", &mut out).is_err());
    assert!(w.end_file(&mut out).is_err());
    w.begin_file("f", Some(1), &EntryMeta::default(), &mut out)
        .unwrap();
    assert!(
        w.begin_file("g", Some(1), &EntryMeta::default(), &mut out)
            .is_err()
    );
}

#[test]
fn civil_calendar() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(19675), (2023, 11, 14));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    // Leap day.
    assert_eq!(civil_from_days(11016), (2000, 2, 29));
}

#[test]
fn dos_time_encoding() {
    // 2023-11-14 22:13:20 UTC
    let (time, date) = dos_datetime(1_700_000_000);
    assert_eq!(date >> 9, 2023 - 1980);
    assert_eq!((date >> 5) & 0xF, 11);
    assert_eq!(date & 0x1F, 14);
    assert_eq!(time >> 11, 22);
    assert_eq!((time >> 5) & 0x3F, 13);
    assert_eq!((time & 0x1F) * 2, 20);

    // Pre-1980 clamps to the epoch of the format.
    let (time, date) = dos_datetime(0);
    assert_eq!((time, date), (0, (1 << 5) | 1));
}
