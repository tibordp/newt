//! End-to-end tests for `TarArchiveVfs` over a `MockVfs` upstream.
//!
//! Fixtures (`fixtures/simple.tar`, `.tar.gz`, `.tar.zst`) are
//! committed and regenerated via `fixtures/regenerate.py`. Layout:
//!
//! ```text
//! /hello.txt          "hello world\n"
//! /dir/nested.txt     "nested content\n"
//! /dir/big.bin        200_000 bytes — exercises multi-chunk streaming
//! /links/hard.txt     hardlink -> hello.txt
//! /links/soft.txt     symlink  -> ../hello.txt
//! ```

use std::sync::Arc;

use tokio::io::AsyncReadExt;

use crate::ErrorKind;
use crate::test_support::{FailureSpec, MockVfs, MockVfsConfig};
use crate::vfs::path::PathBuf;
use crate::vfs::{TarArchiveVfs, Vfs, VfsId, VfsPath};

/// Build a VFS path from a wire string (the archive's internal paths are
/// always `/`-rooted).
fn vp(s: &str) -> PathBuf {
    PathBuf::from_wire_str(s)
}

const SIMPLE_TAR: &[u8] = include_bytes!("fixtures/simple.tar");
const SIMPLE_TAR_GZ: &[u8] = include_bytes!("fixtures/simple.tar.gz");
const SIMPLE_TAR_ZST: &[u8] = include_bytes!("fixtures/simple.tar.zst");

const ARCHIVE_PATH: &str = "/archive";

const HELLO: &[u8] = b"hello world\n";
const NESTED: &[u8] = b"nested content\n";

fn big_bytes() -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 251) as u8).collect()
}

/// Build a TarArchiveVfs whose upstream is a MockVfs holding `bytes` at
/// `/archive` (or `/archive.gz`).
fn mount(bytes: &[u8], path: &str, config: MockVfsConfig) -> Arc<TarArchiveVfs> {
    let upstream = MockVfs::builder().config(config).file(path, bytes).build();
    let reporter: Arc<dyn crate::vfs::ProgressReporter> = Arc::new(
        crate::vfs::ScopedReporter::new(Arc::new(crate::vfs::NoopProgressSink), VfsId(1)),
    );
    Arc::new(TarArchiveVfs::new(
        upstream,
        PathBuf::from_wire_str(path),
        VfsPath::root(VfsId(1)),
        Vec::new(),
        reporter,
    ))
}

async fn read_to_vec(vfs: &TarArchiveVfs, path: &str) -> Vec<u8> {
    let mut reader = vfs
        .open_read_async(&vp(path))
        .await
        .expect("open_read_async");
    let mut out = Vec::new();
    reader.read_to_end(&mut out).await.expect("read_to_end");
    out
}

// ---------------------------------------------------------------------------
// Basic listing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_top_level_entries() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    let mut names: Vec<String> = vfs
        .list_files(&vp("/"), None)
        .await
        .expect("list_files")
        .files
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n != "..")
        .collect();
    names.sort();
    assert_eq!(names, vec!["dir", "hello.txt", "links"]);
}

#[tokio::test]
async fn lists_nested_dir() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    let mut names: Vec<String> = vfs
        .list_files(&vp("/dir"), None)
        .await
        .expect("list_files")
        .files
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n != "..")
        .collect();
    names.sort();
    assert_eq!(names, vec!["big.bin", "nested.txt"]);
}

// ---------------------------------------------------------------------------
// open_read_async — streaming behaviour
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_read_small_file() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
    assert_eq!(read_to_vec(&vfs, "/dir/nested.txt").await, NESTED);
}

/// >64 KiB file — exercises multiple `OutputReady` chunks through the
/// streaming reader's mpsc channel and `poll_read` partial-buffer path.
#[tokio::test]
async fn streaming_read_multi_chunk_file() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    assert_eq!(read_to_vec(&vfs, "/dir/big.bin").await, big_bytes());
}

#[tokio::test]
async fn streaming_read_gzip_decompresses() {
    let vfs = mount(SIMPLE_TAR_GZ, "/archive.gz", MockVfsConfig::default());
    assert_eq!(read_to_vec(&vfs, "/dir/big.bin").await, big_bytes());
    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
}

#[tokio::test]
async fn read_order_follows_the_stream_and_readers_park() {
    let vfs = mount(SIMPLE_TAR_GZ, "/archive.gz", MockVfsConfig::default());
    let order = vfs
        .read_order(&[
            vp("/dir/big.bin"),
            vp("/hello.txt"),
            vp("/dir/nested.txt"),
            vp("/nope"),
        ])
        .await
        .unwrap()
        .unwrap();
    assert!(order[1] < order[2] && order[2] < order[0], "{:?}", order);
    assert_eq!(order[3], u64::MAX);

    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
    assert_eq!(vfs.pool.parked(), 1);
    // The next entry in stream order resumes the parked reader.
    assert_eq!(read_to_vec(&vfs, "/dir/nested.txt").await, NESTED);
    assert_eq!(vfs.pool.parked(), 1);
    // An entry behind every parked reader restores a checkpoint and
    // parks a second reader.
    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
    assert_eq!(vfs.pool.parked(), 2);
    let chunk = vfs
        .read_range(&vp("/dir/big.bin"), 1_000, 10)
        .await
        .expect("read_range");
    assert_eq!(chunk.data, big_bytes()[1_000..1_010]);
    assert_eq!(vfs.pool.parked(), 2);
}

#[tokio::test]
async fn streaming_read_small_buffers() {
    // Drain via a 17-byte buffer to stress the partial-chunk path in
    // the pipelined reader (chunk shorter than `buf.remaining()`).
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    let mut reader = vfs.open_read_async(&vp("/dir/big.bin")).await.unwrap();
    let mut out = Vec::new();
    let mut tmp = [0u8; 17];
    loop {
        let n = reader.read(&mut tmp).await.unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&tmp[..n]);
    }
    assert_eq!(out, big_bytes());
}

#[tokio::test]
async fn open_read_async_missing_path_errors() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    let err = match vfs.open_read_async(&vp("/nope.txt")).await {
        Ok(_) => panic!("expected NotFound, got Ok"),
        Err(e) => e,
    };
    assert_eq!(err.kind, ErrorKind::NotFound);
}

// ---------------------------------------------------------------------------
// read_range
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_range_returns_correct_slice() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    let big = big_bytes();

    // Mid-file slice across the 64 KiB chunk boundary.
    let chunk = vfs
        .read_range(&vp("/dir/big.bin"), 60_000, 10_000)
        .await
        .expect("read_range");
    assert_eq!(chunk.offset, 60_000);
    assert_eq!(chunk.total_size, big.len() as u64);
    assert_eq!(chunk.data, big[60_000..70_000]);

    // Tail slice — clamped to file end.
    let chunk = vfs
        .read_range(&vp("/dir/big.bin"), 199_900, 1_000)
        .await
        .expect("read_range tail");
    assert_eq!(chunk.data, big[199_900..]);
}

// ---------------------------------------------------------------------------
// Hardlinks and symlinks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hardlink_resolves_to_target_content() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    assert_eq!(read_to_vec(&vfs, "/links/hard.txt").await, HELLO);
}

#[tokio::test]
async fn symlink_resolves_to_target_content() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    assert_eq!(read_to_vec(&vfs, "/links/soft.txt").await, HELLO);
}

#[tokio::test]
async fn file_details_reports_symlink_metadata() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    let details = vfs
        .file_details(&vp("/links/soft.txt"))
        .await
        .expect("file_details");
    assert!(details.is_symlink);
    assert_eq!(details.symlink_target.as_deref(), Some("../hello.txt"));
}

/// Tar has no `get_metadata` override — the trait default derives it from
/// the indexed listing entry, feeding metadata preservation on copy-out.
#[tokio::test]
async fn get_metadata_derives_from_the_index() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    let meta = vfs
        .get_metadata(&vp("/hello.txt"))
        .await
        .expect("get_metadata");
    assert_eq!(meta.permissions, Some(0o644));
    assert_eq!(meta.uid, Some(1000));
    assert_eq!(meta.gid, Some(1000));
    assert_eq!(
        meta.mtime,
        Some(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))
    );
    assert_eq!(meta.atime, None);
}

// ---------------------------------------------------------------------------
// Strict-range upstreams (object stores)
// ---------------------------------------------------------------------------

/// S3-style upstreams reject range reads starting at/past the object size
/// instead of returning an empty chunk. The zstd decoder asks for more
/// input after consuming the final frame (probing for a concatenated one),
/// so the indexer must stop at the known file size rather than read past
/// the end.
#[tokio::test]
async fn zstd_indexes_over_strict_upstream() {
    let vfs = mount(
        SIMPLE_TAR_ZST,
        "/archive.zst",
        MockVfsConfig {
            strict_range_reads: true,
            ..MockVfsConfig::default()
        },
    );
    let mut names: Vec<String> = vfs
        .list_files(&vp("/"), None)
        .await
        .expect("list_files")
        .files
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n != "..")
        .collect();
    names.sort();
    assert_eq!(names, vec!["dir", "hello.txt", "links"]);
}

#[tokio::test]
async fn zstd_reads_over_strict_upstream() {
    let vfs = mount(
        SIMPLE_TAR_ZST,
        "/archive.zst",
        MockVfsConfig {
            strict_range_reads: true,
            ..MockVfsConfig::default()
        },
    );
    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
    assert_eq!(read_to_vec(&vfs, "/dir/big.bin").await, big_bytes());
}

// ---------------------------------------------------------------------------
// Error propagation
// ---------------------------------------------------------------------------

/// Indexing failure propagates: if the upstream `read_range` errors
/// during the async indexing path, the tar VFS surfaces it (rather than
/// hanging waiting for an index that will never arrive).
#[tokio::test]
async fn upstream_read_range_failure_during_indexing_surfaces() {
    let upstream = MockVfs::builder()
        .config(MockVfsConfig::default())
        .file(ARCHIVE_PATH, SIMPLE_TAR)
        .failure(FailureSpec {
            path: PathBuf::from_wire_str(ARCHIVE_PATH),
            operation: "read_at",
            error: crate::Error {
                kind: ErrorKind::Connection,
                message: "simulated upstream failure".into(),
            },
            remaining: None,
        })
        .build();

    let reporter: Arc<dyn crate::vfs::ProgressReporter> = Arc::new(
        crate::vfs::ScopedReporter::new(Arc::new(crate::vfs::NoopProgressSink), VfsId(1)),
    );
    let vfs = TarArchiveVfs::new(
        upstream,
        PathBuf::from_wire_str(ARCHIVE_PATH),
        VfsPath::root(VfsId(1)),
        Vec::new(),
        reporter,
    );

    let err = match vfs.open_read_async(&vp("/hello.txt")).await {
        Ok(_) => panic!("indexing should fail, got Ok"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("simulated upstream failure"),
        "unexpected error: {}",
        err.message
    );
}

/// A link resolves through the archive's own tree, `..` included.
#[tokio::test]
async fn resolve_link_follows_the_archive_tree() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, MockVfsConfig::default());
    vfs.list_files(&vp("/"), None).await.expect("list_files");
    assert_eq!(
        vfs.resolve_link(&vp("/links/soft.txt")).await.unwrap(),
        vp("/hello.txt")
    );
}

// ---------------------------------------------------------------------------
// ar archives
// ---------------------------------------------------------------------------

/// A GNU-style ar archive of `(name, data)` members, in order.
fn ar_archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            format!("{name}/"),
            0,
            0,
            0,
            "100644",
            data.len()
        );
        assert_eq!(header.len(), 60);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

#[tokio::test]
async fn ar_archive_lists_and_reads() {
    let bytes = ar_archive(&[("foo.o", b"foo object"), ("bar.o", b"odd")]);
    let vfs = mount(&bytes, ARCHIVE_PATH, MockVfsConfig::default());
    let mut names: Vec<String> = vfs
        .list_files(&vp("/"), None)
        .await
        .expect("list_files")
        .files
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n != "..")
        .collect();
    names.sort();
    assert_eq!(names, ["bar.o", "foo.o"]);
    assert_eq!(read_to_vec(&vfs, "/foo.o").await, b"foo object");
    assert_eq!(read_to_vec(&vfs, "/bar.o").await, b"odd");
}

/// A static library can hold two members of one name; the listing shows
/// one row, and it's the last member, which is also what reads return.
#[tokio::test]
async fn duplicate_members_list_once_as_the_last() {
    let bytes = ar_archive(&[("dup.o", b"first"), ("dup.o", b"second!")]);
    let vfs = mount(&bytes, ARCHIVE_PATH, MockVfsConfig::default());
    let files: Vec<_> = vfs
        .list_files(&vp("/"), None)
        .await
        .expect("list_files")
        .files
        .into_iter()
        .filter(|f| f.name != "..")
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].size, Some(7));
    assert_eq!(read_to_vec(&vfs, "/dup.o").await, b"second!");
}

/// An archive cut off inside a member still mounts: everything before the
/// cut lists and reads, the member it lands in lists and fails its read,
/// and the listing is flagged partial.
#[tokio::test]
async fn truncated_archive_mounts_what_came_before_the_cut() {
    let cut = &SIMPLE_TAR[..SIMPLE_TAR.len() / 2];
    let vfs = mount(cut, ARCHIVE_PATH, MockVfsConfig::default());
    let listing = vfs.list_files(&vp("/dir"), None).await.expect("list_files");
    assert_eq!(listing.partial.as_deref(), Some("archive truncated"));
    assert!(listing.files.iter().any(|f| f.name == "big.bin"));

    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
    let mut reader = vfs
        .open_read_async(&vp("/dir/big.bin"))
        .await
        .expect("open_read_async");
    let mut out = Vec::new();
    let err = reader
        .read_to_end(&mut out)
        .await
        .expect_err("the cut member must not read as complete");
    assert!(
        err.to_string().contains("truncated"),
        "unexpected error: {err}"
    );
}

/// Leaving the archive root while it indexes — entering one of its folders,
/// or backing out and coming in again — pauses indexing, and the next
/// listing or read picks it up rather than finding it stuck partial.
#[tokio::test]
async fn indexing_resumes_after_the_last_consumer_leaves() {
    let vfs = mount(SIMPLE_TAR_GZ, "/archive.gz", MockVfsConfig::default());
    for _ in 0..2 {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let root = vp("/");
        let listing = vfs.list_files(&root, Some(tx));
        tokio::pin!(listing);
        assert!(futures::poll!(listing.as_mut()).is_pending());
    }

    let deadline = std::time::Duration::from_secs(5);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let listing = tokio::time::timeout(deadline, vfs.list_files(&vp("/dir"), Some(tx)))
        .await
        .expect("listing finishes")
        .expect("list_files");
    assert_eq!(listing.partial, None);
    let mut names: Vec<String> = listing
        .files
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n != "..")
        .collect();
    names.sort();
    assert_eq!(names, ["big.bin", "nested.txt"]);

    let hello = tokio::time::timeout(deadline, read_to_vec(&vfs, "/hello.txt"))
        .await
        .expect("read finishes");
    assert_eq!(hello, HELLO);
}
