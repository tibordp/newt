//! End-to-end tests for `DiscVfs` mounted via `disc::mount` on top of a
//! `MockVfs` configured with object-store semantics (`strict_range_reads`
//! — like S3, reads at/past EOF error instead of returning empty chunks).
//! Fixture images are shared with the `newt-disc` parser tests.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::ErrorKind;
use crate::api::MountContext;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::File;
use crate::test_support::{MockVfs, MockVfsConfig};
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{Vfs, VfsDescriptor, VfsFileList, VfsId, VfsPath, VfsRegistry};

const IMAGE_PATH: &str = "/image.iso";
const HELLO: &[u8] = b"Hello from the disc image!\n";

fn vp(s: &str) -> PathBuf {
    PathBuf::from_wire_str(s)
}

fn fixture(name: &str) -> Vec<u8> {
    let compressed: &[u8] = match name {
        "joliet" => include_bytes!("../../../newt-disc/fixtures/joliet.iso.gz"),
        "rockridge" => include_bytes!("../../../newt-disc/fixtures/rockridge.iso.gz"),
        "udf250" => include_bytes!("../../../newt-disc/fixtures/udf250.iso.gz"),
        _ => panic!("unknown fixture {}", name),
    };
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(compressed)
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// Delegating wrapper that counts upstream `read_range` calls — the whole
/// point of this backend is read efficiency on high-latency upstreams.
struct CountingVfs {
    inner: Arc<dyn Vfs>,
    read_ranges: AtomicUsize,
}

impl CountingVfs {
    fn new(inner: Arc<dyn Vfs>) -> Arc<Self> {
        Arc::new(CountingVfs {
            inner,
            read_ranges: AtomicUsize::new(0),
        })
    }

    fn reads(&self) -> usize {
        self.read_ranges.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Vfs for CountingVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        self.inner.descriptor()
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<VfsFileList, crate::Error> {
        self.inner.list_files(path, batch_tx).await
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), crate::Error> {
        self.inner.poll_changes(path).await
    }

    async fn fs_stats(
        &self,
        path: &Path,
    ) -> Result<Option<crate::filesystem::FsStats>, crate::Error> {
        self.inner.fs_stats(path).await
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, crate::Error> {
        self.inner.file_details(path).await
    }

    async fn read_range(
        &self,
        path: &Path,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, crate::Error> {
        self.read_ranges.fetch_add(1, Ordering::SeqCst);
        self.inner.read_range(path, offset, length).await
    }
}

struct Harness {
    registry: Arc<VfsRegistry>,
    counter: Arc<CountingVfs>,
    origin: VfsPath,
    pending_read_streams: crate::api::PendingVfsReadStreams,
    host_communicator: Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
    progress_reporter: Arc<dyn crate::vfs::ProgressReporter>,
}

impl Harness {
    fn new(image: &[u8]) -> Self {
        let upstream = MockVfs::builder()
            .config(MockVfsConfig {
                can_read_sync: false,
                can_read_async: true,
                strict_range_reads: true,
                ..MockVfsConfig::default()
            })
            .file(IMAGE_PATH, image)
            .build();
        let counter = CountingVfs::new(upstream);

        Self {
            registry: Arc::new(VfsRegistry::with_root(counter.clone())),
            counter,
            origin: VfsPath::from_wire_str(VfsId::ROOT, IMAGE_PATH),
            pending_read_streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            host_communicator: Arc::new(std::sync::OnceLock::new()),
            progress_reporter: Arc::new(crate::vfs::ScopedReporter::new(
                Arc::new(crate::vfs::NoopProgressSink),
                VfsId(0),
            )),
        }
    }

    fn ctx(&self) -> MountContext<'_> {
        MountContext {
            registry: &self.registry,
            host_communicator: &self.host_communicator,
            pending_read_streams: &self.pending_read_streams,
            sftp_askpass: None,
            askpass_provider: None,
            agent_resolver: None,
            extra_path: &[],
            progress_reporter: &self.progress_reporter,
        }
    }

    async fn mount(&self) -> Arc<dyn Vfs> {
        super::mount(self.origin.clone(), &self.ctx())
            .await
            .expect("disc mount failed")
    }
}

async fn list_names(vfs: &Arc<dyn Vfs>, path: &str) -> Vec<(String, bool)> {
    let listing = vfs.list_files(&vp(path), None).await.unwrap();
    listing
        .files
        .iter()
        .map(|f| (f.name.clone(), f.is_dir))
        .collect()
}

fn names(listing: &[(String, bool)]) -> Vec<&str> {
    listing.iter().map(|(n, _)| n.as_str()).collect()
}

#[tokio::test]
async fn joliet_listing_and_reads() {
    let harness = Harness::new(&fixture("joliet"));
    let vfs = harness.mount().await;

    let root = list_names(&vfs, "/").await;
    assert_eq!(root[0].0, "..");
    assert!(names(&root).contains(&"hello.txt"));
    assert!(names(&root).contains(&"Ünïcødé nämé.txt"));
    let sub = root.iter().find(|(n, _)| n == "sub").unwrap();
    assert!(sub.1);

    // Nested listing.
    let deeper = list_names(&vfs, "/sub/deeper").await;
    assert!(names(&deeper).contains(&"deep.txt"));

    // read_range: exact, sliced, past-EOF (POSIX semantics regardless of
    // the strict object-store upstream).
    let chunk = vfs.read_range(&vp("/hello.txt"), 0, 1024).await.unwrap();
    assert_eq!(chunk.data, HELLO);
    assert_eq!(chunk.total_size, HELLO.len() as u64);
    let chunk = vfs.read_range(&vp("/hello.txt"), 6, 4).await.unwrap();
    assert_eq!(chunk.data, b"from");
    let chunk = vfs.read_range(&vp("/hello.txt"), 1000, 10).await.unwrap();
    assert!(chunk.data.is_empty());
    assert_eq!(chunk.total_size, HELLO.len() as u64);

    // Hidden dotfile flag.
    let listing = vfs.list_files(&vp("/"), None).await.unwrap();
    let hidden = listing
        .files
        .iter()
        .find(|f| f.name == ".hidden.txt")
        .unwrap();
    assert!(hidden.is_hidden);

    // file_details reports size + timestamps.
    let details = vfs.file_details(&vp("/big.bin")).await.unwrap();
    assert_eq!(details.size, 65536);
    assert!(details.modified.is_some());

    // Missing entries are NotFound.
    let err = vfs.read_range(&vp("/nope.txt"), 0, 4).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[tokio::test]
async fn streaming_read() {
    let harness = Harness::new(&fixture("joliet"));
    let vfs = harness.mount().await;

    let mut reader = vfs.open_read_async(&vp("/big.bin")).await.unwrap();
    let mut out = Vec::new();
    reader.read_to_end(&mut out).await.unwrap();
    assert_eq!(out.len(), 65536);
    // FNV-1a fingerprint of the deterministic fixture payload.
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in &out {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    assert_eq!(h, 0x4155a52457fe334);
}

#[tokio::test]
async fn udf_metadata_partition_and_symlinks() {
    let harness = Harness::new(&fixture("udf250"));
    let vfs = harness.mount().await;

    let root = list_names(&vfs, "/").await;
    assert!(names(&root).contains(&"hello.txt"));

    // Symlink rows carry the target and mirror its directory-ness.
    let listing = vfs.list_files(&vp("/"), None).await.unwrap();
    let link = listing
        .files
        .iter()
        .find(|f| f.name == "link_to_deeper")
        .unwrap();
    assert!(link.is_symlink);
    assert_eq!(link.symlink_target.as_deref(), Some("sub/deeper"));
    assert!(link.is_dir, "symlink to a directory lists as enterable");

    // Paths resolve through symlinks.
    let through = list_names(&vfs, "/link_to_deeper").await;
    assert!(names(&through).contains(&"deep.txt"));
    let chunk = vfs
        .read_range(&vp("/link_to_hello"), 0, 1024)
        .await
        .unwrap();
    assert_eq!(chunk.data, HELLO);

    // file_details follows the final symlink (viewer semantics).
    let details = vfs.file_details(&vp("/link_to_hello")).await.unwrap();
    assert_eq!(details.size, HELLO.len() as u64);

    // file_info does not (listing semantics) but mirrors target dir-ness.
    let info = vfs.file_info(&vp("/link_to_deeper")).await.unwrap();
    assert!(info.is_symlink);
    assert!(info.is_dir);
}

#[tokio::test]
async fn rock_ridge_symlinks_and_deep_paths() {
    let harness = Harness::new(&fixture("rockridge"));
    let vfs = harness.mount().await;

    let chunk = vfs.read_range(&vp("/link_abs"), 0, 1024).await.unwrap();
    assert_eq!(chunk.data, b"nested\n");

    let deep = vfs
        .read_range(&vp("/sub/deeper/deep.txt"), 0, 1024)
        .await
        .unwrap();
    assert_eq!(chunk.total_size, 7);
    assert_eq!(deep.data, b"deep file content\n");
}

#[tokio::test]
async fn read_efficiency() {
    let harness = Harness::new(&fixture("udf250"));
    let vfs = harness.mount().await;

    // Mounting is lazy — no reads yet.
    assert_eq!(harness.counter.reads(), 0);

    // Probe + root listing: the block cache coalesces the structure walk
    // into a handful of upstream range reads.
    vfs.list_files(&vp("/"), None).await.unwrap();
    let after_first_list = harness.counter.reads();
    assert!(
        after_first_list <= 12,
        "probe + root listing took {} upstream reads",
        after_first_list
    );

    // Re-listing is fully cached: zero further upstream reads.
    vfs.list_files(&vp("/"), None).await.unwrap();
    assert_eq!(harness.counter.reads(), after_first_list);

    // Listing a subdirectory reuses cached blocks (metadata clusters).
    vfs.list_files(&vp("/sub"), None).await.unwrap();
    let after_sub = harness.counter.reads();
    assert!(
        after_sub - after_first_list <= 2,
        "subdirectory listing took {} extra reads",
        after_sub - after_first_list
    );

    // A file-content read is exactly one pass-through upstream read.
    vfs.read_range(&vp("/big.bin"), 4096, 8192).await.unwrap();
    assert_eq!(harness.counter.reads(), after_sub + 1);
}

#[tokio::test]
async fn not_a_disc_image() {
    let harness = Harness::new(&vec![0u8; 500_000]);
    let vfs = harness.mount().await;
    // Mount itself is lazy; the first operation surfaces the error.
    let err = vfs.list_files(&vp("/"), None).await.unwrap_err();
    assert!(
        err.message.contains("not an ISO 9660 or UDF disc image"),
        "unexpected error: {}",
        err.message
    );
}

#[tokio::test]
async fn writes_rejected_by_capabilities() {
    let harness = Harness::new(&fixture("joliet"));
    let vfs = harness.mount().await;
    let desc = vfs.descriptor();
    assert!(!desc.can_overwrite_sync());
    assert!(!desc.can_overwrite_async());
    assert!(!desc.can_remove());
    assert!(!desc.can_rename());
    assert!(desc.is_ephemeral());
    let err = vfs.remove_file(&vp("/hello.txt")).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotSupported);
}
