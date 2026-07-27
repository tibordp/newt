use super::*;

/// Drive a probe against an in-memory archive.
fn probe(image: &[u8]) -> Result<ZipFs> {
    let mut op = ZipProbeOp::new(image.len() as u64);
    let mut fetched = Vec::new();
    loop {
        match op.step(fetched)? {
            Step::Done(fs) => return Ok(fs),
            Step::Need(ranges) => {
                fetched = serve(image, &ranges);
            }
        }
    }
}

fn serve(image: &[u8], ranges: &[std::ops::Range<u64>]) -> Vec<Chunk> {
    ranges
        .iter()
        .map(|r| Chunk {
            offset: r.start,
            data: image[r.start as usize..r.end as usize].to_vec(),
        })
        .collect()
}

/// Open an entry and read its full plaintext.
fn read_entry(image: &[u8], fs: &ZipFs, name: &str, password: Option<&[u8]>) -> Result<Vec<u8>> {
    read_entry_range(image, fs, name, password, 0, u64::MAX)
}

fn read_entry_range(
    image: &[u8],
    fs: &ZipFs,
    name: &str,
    password: Option<&[u8]>,
    start: u64,
    len: u64,
) -> Result<Vec<u8>> {
    let entry = fs
        .entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no entry named {}", name));
    let mut op = EntryOpenOp::new(entry, fs.file_size)?;
    let mut fetched = Vec::new();
    let open = loop {
        match op.step(fetched)? {
            Step::Done(open) => break open,
            Step::Need(ranges) => fetched = serve(image, &ranges),
        }
    };
    let key = match (open.needs_password(), password) {
        (true, Some(pw)) => Some(open.verify_password(entry, pw)?),
        (true, None) => return Err(ZipError::PasswordRequired),
        (false, _) => None,
    };
    let mut reader = EntryReader::new(entry, &open, key.as_ref(), start)?;
    let want = len.min(entry.size.saturating_sub(start));
    let mut out = Vec::new();
    let mut pending: Option<Chunk> = None;
    loop {
        if reader.buffered() > 0 {
            out.extend(reader.take_output(want as usize - out.len()));
            if out.len() as u64 >= want {
                // Full-stream reads still drive verification to completion.
                if start == 0 && len == u64::MAX {
                    loop {
                        match reader.step(pending.take())? {
                            ReadStep::Done => return Ok(out),
                            ReadStep::Need(r) => {
                                pending = Some(serve(image, &[r]).pop().unwrap());
                            }
                            ReadStep::Output => {
                                reader.take_output(usize::MAX);
                            }
                        }
                    }
                }
                return Ok(out);
            }
            continue;
        }
        match reader.step(pending.take())? {
            ReadStep::Done => return Ok(out),
            ReadStep::Need(r) => pending = Some(serve(image, &[r]).pop().unwrap()),
            ReadStep::Output => {}
        }
    }
}

/// Build a zip with our own writer: one deflated file, one stored file.
fn writer_zip() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = crate::ZipWriter::new(None, None);
    let meta = crate::EntryMeta {
        mode: Some(0o644),
        mtime_ms: Some(1_700_000_000_000),
        ..Default::default()
    };
    w.begin_file("hello.txt", Some(12), &meta, &mut out)
        .unwrap();
    w.write_data(b"hello world\n", &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();
    out
}

// ---------------------------------------------------------------------------
// Foreign archives (authored by the `zip` crate)
// ---------------------------------------------------------------------------

fn foreign_writer() -> zip::ZipWriter<std::io::Cursor<Vec<u8>>> {
    zip::ZipWriter::new(std::io::Cursor::new(Vec::new()))
}

fn foreign_finish(w: zip::ZipWriter<std::io::Cursor<Vec<u8>>>) -> Vec<u8> {
    w.finish().unwrap().into_inner()
}

fn opts(method: zip::CompressionMethod) -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default()
        .compression_method(method)
        .unix_permissions(0o644)
}

fn patterned(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[test]
fn reads_own_writer_output() {
    let image = writer_zip();
    let fs = probe(&image).expect("probe");
    assert_eq!(fs.base_offset, 0);
    let entry = fs.entries.iter().find(|e| e.name == "hello.txt").unwrap();
    assert_eq!(entry.size, 12);
    assert_eq!(entry.kind, EntryKind::File);
    assert_eq!(entry.mode, Some(0o644));
    assert_eq!(entry.modified, Some(1_700_000_000_000));
    assert_eq!(
        read_entry(&image, &fs, "hello.txt", None).unwrap(),
        b"hello world\n"
    );
}

#[test]
fn foreign_methods_and_metadata() {
    use std::io::Write;
    let mut w = foreign_writer();
    let body = patterned(100_000);
    w.start_file("stored.bin", opts(zip::CompressionMethod::Stored))
        .unwrap();
    w.write_all(&body).unwrap();
    w.start_file("deflated.bin", opts(zip::CompressionMethod::Deflated))
        .unwrap();
    w.write_all(&body).unwrap();
    w.start_file("packed.bz2", opts(zip::CompressionMethod::Bzip2))
        .unwrap();
    w.write_all(&body).unwrap();
    w.start_file("packed.zst", opts(zip::CompressionMethod::Zstd))
        .unwrap();
    w.write_all(&body).unwrap();
    w.add_directory(
        "subdir/",
        opts(zip::CompressionMethod::Stored).unix_permissions(0o755),
    )
    .unwrap();
    w.add_symlink(
        "link.txt",
        "stored.bin",
        opts(zip::CompressionMethod::Stored),
    )
    .unwrap();
    w.start_file("π — unicode.txt", opts(zip::CompressionMethod::Deflated))
        .unwrap();
    w.write_all("naïve × content".as_bytes()).unwrap();
    let image = foreign_finish(w);

    let fs = probe(&image).expect("probe");
    for name in ["stored.bin", "deflated.bin", "packed.bz2", "packed.zst"] {
        let entry = fs.entries.iter().find(|e| e.name == name).unwrap();
        assert_eq!(entry.kind, EntryKind::File, "{}", name);
        assert_eq!(entry.size, body.len() as u64, "{}", name);
        assert_eq!(entry.mode, Some(0o644), "{}", name);
        assert_eq!(
            read_entry(&image, &fs, name, None).unwrap(),
            body,
            "{}",
            name
        );
    }
    let dir = fs.entries.iter().find(|e| e.name == "subdir").unwrap();
    assert_eq!(dir.kind, EntryKind::Dir);
    assert_eq!(dir.mode, Some(0o755));
    let link = fs.entries.iter().find(|e| e.name == "link.txt").unwrap();
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(
        read_entry(&image, &fs, "link.txt", None).unwrap(),
        b"stored.bin"
    );
    assert_eq!(
        read_entry(&image, &fs, "π — unicode.txt", None).unwrap(),
        "naïve × content".as_bytes()
    );
}

/// The `zip` crate no longer writes ZipCrypto, so the container is built by
/// hand: one stored, ZipCrypto-encrypted entry (host DOS, no extras).
fn zipcrypto_zip(password: &[u8], name: &str, payload: &[u8]) -> Vec<u8> {
    fn le16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn le32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let crc = crc32fast::hash(payload);
    let stream = super::decrypt::zipcrypto_encrypt(password, (crc >> 24) as u8, payload);
    let mut out = Vec::new();
    le32(&mut out, 0x0403_4b50);
    le16(&mut out, 20); // version needed
    le16(&mut out, 1); // flags: encrypted
    le16(&mut out, 0); // method: stored
    le16(&mut out, 0x6000); // time
    le16(&mut out, 0x5821); // date
    le32(&mut out, crc);
    le32(&mut out, stream.len() as u32);
    le32(&mut out, payload.len() as u32);
    le16(&mut out, name.len() as u16);
    le16(&mut out, 0);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&stream);

    let cd_off = out.len() as u32;
    le32(&mut out, 0x0201_4b50);
    le16(&mut out, 20); // version made by (DOS host)
    le16(&mut out, 20);
    le16(&mut out, 1);
    le16(&mut out, 0);
    le16(&mut out, 0x6000);
    le16(&mut out, 0x5821);
    le32(&mut out, crc);
    le32(&mut out, stream.len() as u32);
    le32(&mut out, payload.len() as u32);
    le16(&mut out, name.len() as u16);
    le16(&mut out, 0); // extra
    le16(&mut out, 0); // comment
    le16(&mut out, 0); // disk
    le16(&mut out, 0); // internal attrs
    le32(&mut out, 0); // external attrs
    le32(&mut out, 0); // local header offset
    out.extend_from_slice(name.as_bytes());
    let cd_size = out.len() as u32 - cd_off;

    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, 1);
    le16(&mut out, 1);
    le32(&mut out, cd_size);
    le32(&mut out, cd_off);
    le16(&mut out, 0);
    out
}

#[test]
fn foreign_zipcrypto() {
    let image = zipcrypto_zip(b"secret", "locked.txt", b"zipcrypto payload");

    let fs = probe(&image).expect("probe");
    let entry = fs.entries.iter().find(|e| e.name == "locked.txt").unwrap();
    assert!(matches!(entry.encryption, Encryption::ZipCrypto { .. }));
    assert_eq!(
        read_entry(&image, &fs, "locked.txt", Some(b"secret")).unwrap(),
        b"zipcrypto payload"
    );
    assert!(matches!(
        read_entry(&image, &fs, "locked.txt", Some(b"wrong")),
        Err(ZipError::WrongPassword)
    ));
    assert!(matches!(
        read_entry(&image, &fs, "locked.txt", None),
        Err(ZipError::PasswordRequired)
    ));
}

#[test]
fn empty_zip_lists_nothing() {
    // EOCD-only archive, as `zipfile.ZipFile(..., "w").close()` writes it.
    let image: &[u8] = &[
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let fs = probe(image).expect("probe");
    assert!(fs.entries.is_empty());
    assert_eq!(fs.comment, None);
}

#[test]
fn foreign_aes() {
    use std::io::Write;
    let body = patterned(50_000);
    let mut w = foreign_writer();
    w.start_file(
        "aes256.bin",
        opts(zip::CompressionMethod::Deflated).with_aes_encryption(zip::AesMode::Aes256, "pw256"),
    )
    .unwrap();
    w.write_all(&body).unwrap();
    w.start_file(
        "aes128.bin",
        opts(zip::CompressionMethod::Stored).with_aes_encryption(zip::AesMode::Aes128, "pw128"),
    )
    .unwrap();
    w.write_all(&body).unwrap();
    let image = foreign_finish(w);

    let fs = probe(&image).expect("probe");
    let e256 = fs.entries.iter().find(|e| e.name == "aes256.bin").unwrap();
    assert!(matches!(
        e256.encryption,
        Encryption::Aes {
            strength: AesStrength::Aes256,
            ..
        }
    ));
    assert_eq!(e256.method, Method::Deflate);
    assert_eq!(
        read_entry(&image, &fs, "aes256.bin", Some(b"pw256")).unwrap(),
        body
    );
    let e128 = fs.entries.iter().find(|e| e.name == "aes128.bin").unwrap();
    assert!(matches!(
        e128.encryption,
        Encryption::Aes {
            strength: AesStrength::Aes128,
            ..
        }
    ));
    assert_eq!(
        read_entry(&image, &fs, "aes128.bin", Some(b"pw128")).unwrap(),
        body
    );
    assert!(matches!(
        read_entry(&image, &fs, "aes256.bin", Some(b"nope")),
        Err(ZipError::WrongPassword)
    ));

    // Stored + AES supports mid-entry starts via CTR seek.
    assert_eq!(
        read_entry_range(&image, &fs, "aes128.bin", Some(b"pw128"), 10_000, 5_000).unwrap(),
        body[10_000..15_000]
    );
}

#[test]
fn own_writer_aes_round_trip() {
    let body = patterned(80_000);
    let mut out = Vec::new();
    let mut w = crate::ZipWriter::new(None, Some("hunter2"));
    w.begin_file(
        "big.bin",
        Some(body.len() as u64),
        &Default::default(),
        &mut out,
    )
    .unwrap();
    w.write_data(&body, &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let fs = probe(&out).expect("probe");
    // Full read from 0 exercises the HMAC verification path (AE-2).
    assert_eq!(
        read_entry(&out, &fs, "big.bin", Some(b"hunter2")).unwrap(),
        body
    );
}

#[test]
fn own_writer_streaming_descriptor() {
    // Unknown size at begin_file → data-descriptor framing; the CD is
    // still authoritative for the reader.
    let body = patterned(70_000);
    let mut out = Vec::new();
    let mut w = crate::ZipWriter::new(None, None);
    w.begin_file("streamed.bin", None, &Default::default(), &mut out)
        .unwrap();
    w.write_data(&body, &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    let fs = probe(&out).expect("probe");
    assert_eq!(read_entry(&out, &fs, "streamed.bin", None).unwrap(), body);
}

#[test]
fn range_reads_and_cursor_resume() {
    use std::io::Write;
    let body = patterned(300_000);
    let mut w = foreign_writer();
    w.start_file("big.bin", opts(zip::CompressionMethod::Deflated))
        .unwrap();
    w.write_all(&body).unwrap();
    let image = foreign_finish(w);
    let fs = probe(&image).expect("probe");

    for (start, len) in [
        (0u64, 10u64),
        (65_536, 100),
        (299_990, 100),
        (150_000, 65_536),
    ] {
        let want = &body[start as usize..(start + len).min(body.len() as u64) as usize];
        assert_eq!(
            read_entry_range(&image, &fs, "big.bin", None, start, len).unwrap(),
            want,
            "range {}+{}",
            start,
            len
        );
    }

    // A parked reader resumes across a forward seek without restarting.
    let entry = fs.entries.iter().find(|e| e.name == "big.bin").unwrap();
    let mut op = EntryOpenOp::new(entry, fs.file_size).unwrap();
    let mut fetched = Vec::new();
    let open = loop {
        match op.step(fetched).unwrap() {
            Step::Done(open) => break open,
            Step::Need(ranges) => fetched = serve(&image, &ranges),
        }
    };
    let mut reader = EntryReader::new(entry, &open, None, 1_000).unwrap();
    let mut pending = None;
    let mut first = Vec::new();
    while first.len() < 2_000 {
        if reader.buffered() > 0 {
            first.extend(reader.take_output(2_000 - first.len()));
            continue;
        }
        match reader.step(pending.take()).unwrap() {
            ReadStep::Need(r) => pending = Some(serve(&image, &[r]).pop().unwrap()),
            ReadStep::Output | ReadStep::Done => {}
        }
    }
    assert_eq!(first, body[1_000..3_000]);
    reader.seek_forward(200_000).unwrap();
    assert_eq!(reader.position(), 200_000);
    let mut second = Vec::new();
    while second.len() < 1_000 {
        if reader.buffered() > 0 {
            second.extend(reader.take_output(1_000 - second.len()));
            continue;
        }
        match reader.step(pending.take()).unwrap() {
            ReadStep::Need(r) => pending = Some(serve(&image, &[r]).pop().unwrap()),
            ReadStep::Output | ReadStep::Done => {}
        }
    }
    assert_eq!(second, body[200_000..201_000]);
}

#[test]
fn zip64_entries() {
    use std::io::Write;
    let mut w = foreign_writer();
    w.start_file(
        "big64.bin",
        opts(zip::CompressionMethod::Stored).large_file(true),
    )
    .unwrap();
    w.write_all(b"zip64 framed").unwrap();
    let image = foreign_finish(w);
    let fs = probe(&image).expect("probe");
    let entry = fs.entries.iter().find(|e| e.name == "big64.bin").unwrap();
    assert_eq!(entry.size, 12);
    assert_eq!(
        read_entry(&image, &fs, "big64.bin", None).unwrap(),
        b"zip64 framed"
    );
}

#[test]
fn archive_comment_is_decoded() {
    use std::io::Write;
    let mut w = foreign_writer();
    w.set_comment("release notes");
    w.start_file("a.txt", opts(zip::CompressionMethod::Stored))
        .unwrap();
    w.write_all(b"a").unwrap();
    let image = foreign_finish(w);
    let fs = probe(&image).expect("probe");
    assert_eq!(fs.comment.as_deref(), Some("release notes"));
}

#[test]
fn unsupported_method_lists_but_refuses_reads() {
    let mut image = writer_zip();
    // Patch the method field to 98 (PPMd) in both the local header (offset
    // 8 past its signature) and the CD record (offset 10 past its own).
    let patch = |img: &mut Vec<u8>, sig: [u8; 4], off: usize| {
        let pos = img
            .windows(4)
            .position(|w| w == sig)
            .expect("signature present");
        img[pos + off..pos + off + 2].copy_from_slice(&98u16.to_le_bytes());
    };
    patch(&mut image, [0x50, 0x4b, 0x03, 0x04], 8);
    patch(&mut image, [0x50, 0x4b, 0x01, 0x02], 10);

    let fs = probe(&image).expect("listing still works");
    let entry = fs.entries.iter().find(|e| e.name == "hello.txt").unwrap();
    assert_eq!(entry.method, Method::Other(98));
    assert!(matches!(
        read_entry(&image, &fs, "hello.txt", None),
        Err(ZipError::Unsupported(_))
    ));
}

/// Method-14 member as Python's zipfile writes it: LZMA1 with the EOS
/// marker (general-purpose bit 1). A known-size lzma_alone header makes
/// liblzma reject the terminator as trailing garbage — the decoder must
/// declare the size unknown. Fed byte-by-byte to exercise header
/// reassembly across chunk boundaries.
#[test]
fn python_lzma_member_with_eos_marker() {
    let payload: &[u8] = &[
        0x09, 0x04, 0x05, 0x00, 0x5d, 0x00, 0x00, 0x80, 0x00, 0x00, 0x36, 0x1e, 0x89, 0xdd, 0x7d,
        0x49, 0x62, 0x6b, 0xcf, 0xbc, 0xf7, 0xf5, 0x73, 0xbc, 0xfd, 0x93, 0xff, 0xff, 0xbc, 0x66,
        0x00, 0x00,
    ];
    for chunk in [payload.len(), 1, 7] {
        let mut d = super::decompress::Decompressor::new(Method::Lzma).unwrap();
        let mut out = Vec::new();
        let mut pos = 0;
        let mut spins = 0;
        while out.len() < 12 {
            let end = (pos + chunk).min(payload.len());
            let n = d.decompress(&payload[pos..end], &mut out).unwrap();
            pos += n;
            spins += 1;
            assert!(spins < 10_000, "no progress: pos={} out={}", pos, out.len());
        }
        assert_eq!(out, b"lzma packed\n", "chunk size {}", chunk);
    }
}

#[test]
fn corrupted_payload_fails_crc() {
    let body = patterned(10_000);
    let mut out = Vec::new();
    let mut w = crate::ZipWriter::new(Some(0), None);
    w.begin_file(
        "f.bin",
        Some(body.len() as u64),
        &Default::default(),
        &mut out,
    )
    .unwrap();
    w.write_data(&body, &mut out).unwrap();
    w.end_file(&mut out).unwrap();
    w.finish(&mut out).unwrap();

    // Flip one payload byte (stored: the pattern is directly in the image).
    let pos = out
        .windows(8)
        .position(|w| w == &body[100..108])
        .expect("payload present")
        + 3;
    out[pos] ^= 0xFF;

    let fs = probe(&out).expect("probe");
    match read_entry(&out, &fs, "f.bin", None) {
        Err(ZipError::Corrupt(msg)) => assert!(msg.contains("CRC"), "{}", msg),
        other => panic!("expected CRC failure, got {:?}", other.map(|v| v.len())),
    }
}

#[test]
fn sfx_prepended_data_is_skewed_out() {
    let mut image = b"#!/bin/sh\nexit 0\n".to_vec();
    let stub = image.len() as u64;
    image.extend_from_slice(&writer_zip());
    let fs = probe(&image).expect("probe");
    assert_eq!(fs.base_offset, stub);
    assert_eq!(
        read_entry(&image, &fs, "hello.txt", None).unwrap(),
        b"hello world\n"
    );
}

#[test]
fn not_a_zip() {
    assert!(matches!(probe(b"PK"), Err(ZipError::NotAZip)));
    assert!(matches!(probe(&[0u8; 4096]), Err(ZipError::NotAZip)));
}
