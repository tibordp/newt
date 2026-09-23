//! End-to-end tests for `CompressedFileVfs` over a `MockVfs` upstream. The
//! tar fixtures double as bare compressed files: `simple.tar.gz` mounted
//! this way is one entry, `simple.tar`, whose bytes are the tar itself.

use std::sync::Arc;

use tokio::io::AsyncReadExt;

use crate::ErrorKind;
use crate::test_support::{MockVfs, MockVfsConfig};
use crate::vfs::path::PathBuf;
use crate::vfs::{CompressedFileVfs, Vfs, VfsId, VfsPath};

const SIMPLE_TAR: &[u8] = include_bytes!("fixtures/simple.tar");
const SIMPLE_TAR_GZ: &[u8] = include_bytes!("fixtures/simple.tar.gz");
const SIMPLE_TAR_ZST: &[u8] = include_bytes!("fixtures/simple.tar.zst");

fn vp(s: &str) -> PathBuf {
    PathBuf::from_wire_str(s)
}

fn mount(bytes: &[u8], path: &str) -> Arc<CompressedFileVfs> {
    let upstream = MockVfs::builder()
        .config(MockVfsConfig {
            strict_range_reads: true,
            ..MockVfsConfig::default()
        })
        .file(path, bytes)
        .build();
    let reporter: Arc<dyn crate::vfs::ProgressReporter> = Arc::new(
        crate::vfs::ScopedReporter::new(Arc::new(crate::vfs::NoopProgressSink), VfsId(1)),
    );
    Arc::new(CompressedFileVfs::new(
        upstream,
        PathBuf::from_wire_str(path),
        VfsPath::root(VfsId(1)),
        Vec::new(),
        path.to_string(),
        reporter,
    ))
}

async fn read_to_vec(vfs: &CompressedFileVfs, path: &str) -> Vec<u8> {
    let mut reader = vfs.open_read_async(&vp(path)).await.expect("open");
    let mut out = Vec::new();
    reader.read_to_end(&mut out).await.expect("read_to_end");
    out
}

#[tokio::test]
async fn lists_one_entry_named_without_the_suffix() {
    let vfs = mount(SIMPLE_TAR_GZ, "/dir/simple.tar.gz");
    let list = vfs.list_files(&vp("/"), None).await.unwrap();
    let names: Vec<&str> = list.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["..", "simple.tar"]);
    assert!(!list.partial);
    let entry = &list.files[1];
    assert_eq!(entry.size, Some(SIMPLE_TAR.len() as u64));
    assert!(entry.mode.is_none() && entry.modified.is_none() && entry.user.is_none());

    let details = vfs.file_details(&vp("/simple.tar")).await.unwrap();
    assert_eq!(details.size, SIMPLE_TAR.len() as u64);
    assert!(!details.is_dir);
    assert_eq!(
        vfs.file_details(&vp("/nope")).await.unwrap_err().kind,
        ErrorKind::NotFound
    );
    assert_eq!(
        vfs.list_files(&vp("/simple.tar"), None)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::NotADirectory
    );
}

#[tokio::test]
async fn reads_streaming_and_by_range() {
    for (bytes, path) in [
        (SIMPLE_TAR_GZ, "/simple.tar.gz"),
        (SIMPLE_TAR_ZST, "/simple.tar.zst"),
    ] {
        let vfs = mount(bytes, path);
        assert_eq!(
            read_to_vec(&vfs, "/simple.tar").await,
            SIMPLE_TAR,
            "{}",
            path
        );
        let chunk = vfs
            .read_range(&vp("/simple.tar"), 100_000, 5_000)
            .await
            .unwrap();
        assert_eq!(chunk.total_size, SIMPLE_TAR.len() as u64);
        assert_eq!(chunk.data, &SIMPLE_TAR[100_000..105_000]);
        let tail = vfs
            .read_range(&vp("/simple.tar"), SIMPLE_TAR.len() as u64 - 10, 100)
            .await
            .unwrap();
        assert_eq!(tail.data, &SIMPLE_TAR[SIMPLE_TAR.len() - 10..]);
        assert!(vfs.open_read_async(&vp("/missing")).await.is_err());
    }
}

/// The file server and nested mounts read through positioned handles;
/// an archive mount serves them over `read_range`.
#[tokio::test]
async fn positioned_handles_and_nested_mounts_read_through_ranges() {
    let vfs: Arc<dyn Vfs> = mount(SIMPLE_TAR_GZ, "/simple.tar.gz");
    let mut handle = crate::vfs::open_read_at(&vfs, &vp("/simple.tar"))
        .await
        .unwrap();
    let mut got = Vec::new();
    loop {
        let chunk = handle.read_at(got.len() as u64, 100_000).await.unwrap();
        if chunk.is_empty() {
            break;
        }
        got.extend_from_slice(&chunk);
    }
    assert_eq!(got, SIMPLE_TAR);

    // The tar inside, mounted over the compressed-file mount.
    let reporter: Arc<dyn crate::vfs::ProgressReporter> = Arc::new(
        crate::vfs::ScopedReporter::new(Arc::new(crate::vfs::NoopProgressSink), VfsId(2)),
    );
    let inner = crate::vfs::TarArchiveVfs::new(
        vfs,
        vp("/simple.tar"),
        VfsPath::root(VfsId(2)),
        Vec::new(),
        reporter,
    );
    let mut reader = inner.open_read_async(&vp("/hello.txt")).await.unwrap();
    let mut out = Vec::new();
    reader.read_to_end(&mut out).await.unwrap();
    assert_eq!(out, b"hello world\n");
}

#[tokio::test]
async fn listing_streams_the_entry_before_its_size_is_known() {
    let vfs = mount(SIMPLE_TAR_GZ, "/simple.tar.gz");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let list = vfs.list_files(&vp("/"), Some(tx)).await.unwrap();
    let first = rx.recv().await.expect("early batch");
    assert_eq!(first[1].name, "simple.tar");
    assert_eq!(list.files[1].size, Some(SIMPLE_TAR.len() as u64));
}
