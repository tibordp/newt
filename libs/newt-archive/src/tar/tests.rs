use std::collections::HashMap;
use std::io::Read;

use crate::compress::Compression;
use crate::{EntryMeta, TarWriter};

fn meta(mode: u32, mtime_ms: i64) -> EntryMeta {
    EntryMeta {
        mode: Some(mode),
        uid: Some(1000),
        gid: Some(1000),
        uname: Some("user".into()),
        gname: Some("group".into()),
        mtime_ms: Some(mtime_ms),
    }
}

struct ReadEntry {
    path: String,
    kind: tar::EntryType,
    data: Vec<u8>,
    mode: u32,
    mtime: u64,
    link: Option<String>,
    pax: HashMap<String, String>,
}

/// Reads back with the `tar` crate as an oracle (it validates checksums and
/// ustar framing). Pax records are collected raw so tests can assert on them
/// without depending on tar-rs's override behavior.
fn read_back(bytes: &[u8]) -> Vec<ReadEntry> {
    let mut archive = tar::Archive::new(bytes);
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let pax = entry
                .pax_extensions()
                .unwrap()
                .into_iter()
                .flatten()
                .map(|ext| {
                    let ext = ext.unwrap();
                    (
                        ext.key().unwrap().to_string(),
                        ext.value().unwrap().to_string(),
                    )
                })
                .collect();
            let header = entry.header();
            let read = ReadEntry {
                path: String::from_utf8(header.path_bytes().to_vec()).unwrap(),
                kind: header.entry_type(),
                mode: header.mode().unwrap(),
                mtime: header.mtime().unwrap(),
                link: header
                    .link_name_bytes()
                    .map(|b| String::from_utf8(b.to_vec()).unwrap()),
                pax,
                data: Vec::new(),
            };
            let mut read = read;
            entry.read_to_end(&mut read.data).unwrap();
            read
        })
        .collect()
}

#[test]
fn basic_entries() {
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.add_directory("dir", &meta(0o750, 1_700_000_000_000), &mut out)
        .unwrap();
    w.begin_file(
        "dir/hello.txt",
        11,
        &meta(0o640, 1_700_000_001_000),
        &mut out,
    )
    .unwrap();
    assert_eq!(w.write_data(b"hello", &mut out).unwrap(), 5);
    assert_eq!(w.write_data(b" world", &mut out).unwrap(), 6);
    assert_eq!(w.end_file(&mut out).unwrap(), 0);
    w.add_symlink(
        "dir/link",
        "hello.txt",
        &meta(0o777, 1_700_000_002_000),
        &mut out,
    )
    .unwrap();
    w.finish(&mut out).unwrap();

    assert_eq!(out.len() % 512, 0);
    let entries = read_back(&out);
    assert_eq!(entries.len(), 3);

    assert_eq!(entries[0].path, "dir/");
    assert_eq!(entries[0].kind, tar::EntryType::Directory);
    assert_eq!(entries[0].mode, 0o750);
    assert_eq!(entries[0].mtime, 1_700_000_000);

    assert_eq!(entries[1].path, "dir/hello.txt");
    assert_eq!(entries[1].kind, tar::EntryType::Regular);
    assert_eq!(entries[1].data, b"hello world");
    assert_eq!(entries[1].mode, 0o640);

    assert_eq!(entries[2].kind, tar::EntryType::Symlink);
    assert_eq!(entries[2].link.as_deref(), Some("hello.txt"));

    // No pax headers needed for any of these.
    assert!(entries.iter().all(|e| e.pax.is_empty()));
}

#[test]
fn ustar_prefix_split() {
    // >100 bytes total but splittable at a '/' → no pax header.
    let dir = "d".repeat(90);
    let path = format!("{dir}/{}", "f".repeat(60));
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.begin_file(&path, 0, &meta(0o644, 0), &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let entries = read_back(&out);
    assert_eq!(entries[0].path, path);
    assert!(entries[0].pax.is_empty());
}

#[test]
fn pax_long_path_and_millis() {
    // A single 150-byte component cannot be split → pax path record. Since a
    // pax header exists anyway, sub-second mtime rides along.
    let path = "x".repeat(150);
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.begin_file(&path, 3, &meta(0o644, 1_700_000_000_123), &mut out)
        .unwrap();
    w.write_data(b"abc", &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let entries = read_back(&out);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].pax.get("path").unwrap(), &path);
    assert_eq!(entries[0].pax.get("mtime").unwrap(), "1700000000.123");
    assert_eq!(entries[0].data, b"abc");
}

#[test]
fn pax_long_linkname() {
    let target = format!("../{}", "t".repeat(120));
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.add_symlink("link", &target, &meta(0o777, 0), &mut out)
        .unwrap();
    w.finish(&mut out).unwrap();

    let entries = read_back(&out);
    assert_eq!(entries[0].pax.get("linkpath").unwrap(), &target);
}

#[test]
fn pax_large_uid() {
    let mut m = meta(0o644, 0);
    m.uid = Some(0o10000000); // one past the 7-octal-digit field limit
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.begin_file("f", 0, &m, &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let entries = read_back(&out);
    assert_eq!(entries[0].pax.get("uid").unwrap(), "2097152");
}

#[test]
fn pax_negative_mtime() {
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.begin_file("f", 0, &meta(0o644, -1_500), &mut out)
        .unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let entries = read_back(&out);
    // -1500ms = -2s + 500ms
    assert_eq!(entries[0].pax.get("mtime").unwrap(), "-2.500");
}

#[test]
fn short_read_is_padded() {
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.begin_file("f", 10, &meta(0o644, 0), &mut out).unwrap();
    w.write_data(b"abcd", &mut out).unwrap();
    assert_eq!(w.end_file(&mut out).unwrap(), 6);
    w.finish(&mut out).unwrap();

    let entries = read_back(&out);
    assert_eq!(entries[0].data, b"abcd\0\0\0\0\0\0");
}

#[test]
fn overshoot_is_truncated() {
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.begin_file("f", 4, &meta(0o644, 0), &mut out).unwrap();
    assert_eq!(w.write_data(b"abcdef", &mut out).unwrap(), 4);
    assert_eq!(w.write_data(b"gh", &mut out).unwrap(), 0);
    assert_eq!(w.end_file(&mut out).unwrap(), 0);
    w.finish(&mut out).unwrap();

    assert_eq!(read_back(&out)[0].data, b"abcd");
}

#[test]
fn defaults_when_metadata_absent() {
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    w.add_directory("d", &EntryMeta::default(), &mut out)
        .unwrap();
    w.begin_file("d/f", 0, &EntryMeta::default(), &mut out)
        .unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let entries = read_back(&out);
    assert_eq!(entries[0].mode, 0o755);
    assert_eq!(entries[1].mode, 0o644);
    // Default mtime is archive creation time, not zero.
    assert!(entries[1].mtime > 1_700_000_000);
}

#[test]
fn compressed_round_trip() {
    let payload: Vec<u8> = (0..100_000u32).flat_map(|i| i.to_le_bytes()).collect();
    for compression in [Compression::Gzip, Compression::Xz, Compression::Zstd] {
        let mut out = Vec::new();
        let mut w = TarWriter::new(compression, Some(1)).unwrap();
        w.begin_file("data.bin", payload.len() as u64, &meta(0o644, 0), &mut out)
            .unwrap();
        for chunk in payload.chunks(64 * 1024) {
            w.write_data(chunk, &mut out).unwrap();
        }
        w.end_file(&mut out).unwrap();
        w.finish(&mut out).unwrap();

        let mut tar_bytes = Vec::new();
        match compression {
            Compression::Gzip => {
                flate2::read::GzDecoder::new(&out[..])
                    .read_to_end(&mut tar_bytes)
                    .unwrap();
            }
            Compression::Xz => {
                xz2::read::XzDecoder::new(&out[..])
                    .read_to_end(&mut tar_bytes)
                    .unwrap();
            }
            Compression::Zstd => tar_bytes = zstd::stream::decode_all(&out[..]).unwrap(),
            Compression::None => unreachable!(),
        }
        let entries = read_back(&tar_bytes);
        assert_eq!(entries[0].data, payload);
    }
}

#[test]
fn misuse_errors() {
    let mut out = Vec::new();
    let mut w = TarWriter::new(Compression::None, None).unwrap();
    assert!(w.write_data(b"x", &mut out).is_err());
    assert!(w.end_file(&mut out).is_err());
    w.begin_file("f", 1, &EntryMeta::default(), &mut out)
        .unwrap();
    assert!(
        w.begin_file("g", 1, &EntryMeta::default(), &mut out)
            .is_err()
    );
}
