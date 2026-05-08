//! End-to-end tests for `TarArchiveVfs` over a `MockVfs` upstream.
//!
//! Fixtures (`fixtures/simple.tar` and `fixtures/simple.tar.gz`) are
//! committed and regenerated via `fixtures/regenerate.py`. Layout:
//!
//! ```text
//! /hello.txt          "hello world\n"
//! /dir/nested.txt     "nested content\n"
//! /dir/big.bin        200_000 bytes — exercises multi-chunk streaming
//! /links/hard.txt     hardlink -> hello.txt
//! /links/soft.txt     symlink  -> ../hello.txt
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncReadExt;

use crate::ErrorKind;
use crate::test_support::{FailureSpec, MockVfs, MockVfsConfig};
use crate::vfs::{TarArchiveVfs, Vfs, VfsId, VfsPath};

const SIMPLE_TAR: &[u8] = include_bytes!("fixtures/simple.tar");
const SIMPLE_TAR_GZ: &[u8] = include_bytes!("fixtures/simple.tar.gz");

const ARCHIVE_PATH: &str = "/archive";

const HELLO: &[u8] = b"hello world\n";
const NESTED: &[u8] = b"nested content\n";

fn big_bytes() -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 251) as u8).collect()
}

/// Build a TarArchiveVfs whose upstream is a MockVfs holding `bytes` at
/// `/archive` (or `/archive.gz`). The upstream advertises sync/async
/// capability per `config`.
fn mount(bytes: &[u8], path: &str, config: MockVfsConfig) -> Arc<TarArchiveVfs> {
    let upstream = MockVfs::builder().config(config).file(path, bytes).build();
    Arc::new(TarArchiveVfs::new(
        upstream,
        PathBuf::from(path),
        VfsPath::new(VfsId(1), "/"),
        Vec::new(),
    ))
}

fn sync_only_config() -> MockVfsConfig {
    MockVfsConfig {
        can_read_sync: true,
        can_read_async: false,
        ..MockVfsConfig::default()
    }
}

fn async_only_config() -> MockVfsConfig {
    MockVfsConfig {
        can_read_sync: false,
        can_read_async: true,
        ..MockVfsConfig::default()
    }
}

async fn read_to_vec(vfs: &TarArchiveVfs, path: &str) -> Vec<u8> {
    let mut reader = vfs
        .open_read_async(Path::new(path))
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
async fn lists_top_level_entries_sync_upstream() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    let mut names: Vec<String> = vfs
        .list_files(Path::new("/"), None)
        .await
        .expect("list_files")
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n != "..")
        .collect();
    names.sort();
    assert_eq!(names, vec!["dir", "hello.txt", "links"]);
}

#[tokio::test]
async fn lists_top_level_entries_async_upstream() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, async_only_config());
    let mut names: Vec<String> = vfs
        .list_files(Path::new("/"), None)
        .await
        .expect("list_files")
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n != "..")
        .collect();
    names.sort();
    assert_eq!(names, vec!["dir", "hello.txt", "links"]);
}

#[tokio::test]
async fn lists_nested_dir() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    let mut names: Vec<String> = vfs
        .list_files(Path::new("/dir"), None)
        .await
        .expect("list_files")
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
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
    assert_eq!(read_to_vec(&vfs, "/dir/nested.txt").await, NESTED);
}

/// >64 KiB file — exercises multiple `OutputReady` chunks through the
/// streaming reader's mpsc channel and `poll_read` partial-buffer path.
#[tokio::test]
async fn streaming_read_multi_chunk_file() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    assert_eq!(read_to_vec(&vfs, "/dir/big.bin").await, big_bytes());
}

#[tokio::test]
async fn streaming_read_works_with_async_only_upstream() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, async_only_config());
    assert_eq!(read_to_vec(&vfs, "/dir/big.bin").await, big_bytes());
}

#[tokio::test]
async fn streaming_read_gzip_decompresses() {
    let vfs = mount(SIMPLE_TAR_GZ, "/archive.gz", sync_only_config());
    assert_eq!(read_to_vec(&vfs, "/dir/big.bin").await, big_bytes());
    assert_eq!(read_to_vec(&vfs, "/hello.txt").await, HELLO);
}

#[tokio::test]
async fn streaming_read_small_buffers() {
    // Drain via a 17-byte buffer to stress the partial-chunk path in
    // TarStreamingReader::poll_read (chunk shorter than `buf.remaining()`).
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    let mut reader = vfs
        .open_read_async(Path::new("/dir/big.bin"))
        .await
        .unwrap();
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
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    let err = match vfs.open_read_async(Path::new("/nope.txt")).await {
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
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    let big = big_bytes();

    // Mid-file slice across the 64 KiB chunk boundary.
    let chunk = vfs
        .read_range(Path::new("/dir/big.bin"), 60_000, 10_000)
        .await
        .expect("read_range");
    assert_eq!(chunk.offset, 60_000);
    assert_eq!(chunk.total_size, big.len() as u64);
    assert_eq!(chunk.data, big[60_000..70_000]);

    // Tail slice — clamped to file end.
    let chunk = vfs
        .read_range(Path::new("/dir/big.bin"), 199_900, 1_000)
        .await
        .expect("read_range tail");
    assert_eq!(chunk.data, big[199_900..]);
}

// ---------------------------------------------------------------------------
// Hardlinks and symlinks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hardlink_resolves_to_target_content() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    assert_eq!(read_to_vec(&vfs, "/links/hard.txt").await, HELLO);
}

#[tokio::test]
async fn symlink_resolves_to_target_content() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    assert_eq!(read_to_vec(&vfs, "/links/soft.txt").await, HELLO);
}

#[tokio::test]
async fn file_details_reports_symlink_metadata() {
    let vfs = mount(SIMPLE_TAR, ARCHIVE_PATH, sync_only_config());
    let details = vfs
        .file_details(Path::new("/links/soft.txt"))
        .await
        .expect("file_details");
    assert!(details.is_symlink);
    assert_eq!(
        details.symlink_target.as_deref(),
        Some(Path::new("../hello.txt"))
    );
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
        .config(async_only_config())
        .file(ARCHIVE_PATH, SIMPLE_TAR)
        .failure(FailureSpec {
            path: PathBuf::from(ARCHIVE_PATH),
            operation: "read_range",
            error: crate::Error {
                kind: ErrorKind::Connection,
                message: "simulated upstream failure".into(),
            },
            remaining: None,
        })
        .build();

    let vfs = TarArchiveVfs::new(
        upstream,
        PathBuf::from(ARCHIVE_PATH),
        VfsPath::new(VfsId(1), "/"),
        Vec::new(),
    );

    let err = match vfs.open_read_async(Path::new("/hello.txt")).await {
        Ok(_) => panic!("indexing should fail, got Ok"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("simulated upstream failure"),
        "unexpected error: {}",
        err.message
    );
}
