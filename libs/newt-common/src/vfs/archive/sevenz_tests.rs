//! End-to-end tests for `SevenZArchiveVfs` mounted via `archive::mount`,
//! over the fixtures `newt-archive` generates (see its
//! `sevenz/fixtures/regenerate.py` for the layouts).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use async_trait::async_trait;

use crate::ErrorKind;
use crate::askpass::{AskpassProvider, AskpassRequest, AskpassResponse};
use crate::test_support::{MockVfs, MockVfsConfig};
use crate::vfs::File;
use crate::vfs::mount::MountContext;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{FileChunk, FileDetails};
use crate::vfs::{Vfs, VfsDescriptor, VfsFileList, VfsId, VfsPath, VfsRegistry};

fn vp(s: &str) -> PathBuf {
    PathBuf::from_wire_str(s)
}

const BASIC: &[u8] = include_bytes!("../../../../newt-archive/src/sevenz/fixtures/basic.7z");
const SOLID: &[u8] = include_bytes!("../../../../newt-archive/src/sevenz/fixtures/solid.7z");
const X86: &[u8] = include_bytes!("../../../../newt-archive/src/sevenz/fixtures/x86.7z");
const BLOCKS: &[u8] = include_bytes!("../../../../newt-archive/src/sevenz/fixtures/blocks.7z");
const NONSOLID: &[u8] = include_bytes!("../../../../newt-archive/src/sevenz/fixtures/nonsolid.7z");
const PPMD: &[u8] = include_bytes!("../../../../newt-archive/src/sevenz/fixtures/ppmd.7z");
const BCJ2: &[u8] = include_bytes!("../../../../newt-archive/src/sevenz/fixtures/bcj2.7z");
const BCJ2_ENCRYPTED: &[u8] =
    include_bytes!("../../../../newt-archive/src/sevenz/fixtures/bcj2_encrypted.7z");
const ENCRYPTED: &[u8] =
    include_bytes!("../../../../newt-archive/src/sevenz/fixtures/encrypted.7z");
const ENCRYPTED_HEADER: &[u8] =
    include_bytes!("../../../../newt-archive/src/sevenz/fixtures/encrypted_header.7z");

const ARCHIVE_PATH: &str = "/archive.7z";

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

fn solid_set() -> Vec<(String, Vec<u8>)> {
    (0..24)
        .map(|i| {
            if i % 3 == 0 {
                (
                    format!("/solid/noise{:02}.bin", i),
                    noise(i as u64 + 1, 40_000 + i * 1_000),
                )
            } else {
                (
                    format!("/solid/pat{:02}.bin", i),
                    pattern(i as u32, 50_000 + i * 2_000),
                )
            }
        })
        .collect()
}

/// Canned prompt responses, one per prompt.
struct StubAskpass {
    responses: StdMutex<Vec<Option<&'static str>>>,
    prompts: StdMutex<Vec<String>>,
}

impl StubAskpass {
    fn new(responses: Vec<Option<&'static str>>) -> Arc<Self> {
        Arc::new(Self {
            responses: StdMutex::new(responses.into_iter().rev().collect()),
            prompts: StdMutex::new(Vec::new()),
        })
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl AskpassProvider for StubAskpass {
    async fn prompt(&self, req: AskpassRequest) -> AskpassResponse {
        self.prompts.lock().unwrap().push(req.prompt);
        // A user takes time to answer; let concurrent callers reach their
        // own prompt gates while this prompt is open.
        tokio::task::yield_now().await;
        let next = self.responses.lock().unwrap().pop().flatten();
        AskpassResponse(next.map(str::to_owned))
    }
}

/// Upstream that counts range reads, to assert on read efficiency.
struct CountingVfs {
    inner: Arc<dyn Vfs>,
    read_ranges: Arc<AtomicUsize>,
}

impl CountingVfs {
    fn new(inner: Arc<dyn Vfs>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            read_ranges: Arc::new(AtomicUsize::new(0)),
        })
    }
}

struct CountingRandomReader {
    inner: Box<dyn crate::vfs::VfsRandomReader>,
    reads: Arc<AtomicUsize>,
}

#[async_trait]
impl crate::vfs::VfsRandomReader for CountingRandomReader {
    async fn read_at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, crate::Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_at(offset, len).await
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

    async fn fs_stats(&self, path: &Path) -> Result<Option<crate::vfs::FsStats>, crate::Error> {
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

    async fn open_read_at(
        &self,
        path: &Path,
    ) -> Result<Box<dyn crate::vfs::VfsRandomReader>, crate::Error> {
        Ok(Box::new(CountingRandomReader {
            inner: self.inner.open_read_at(path).await?,
            reads: self.read_ranges.clone(),
        }))
    }
}

struct Harness {
    registry: Arc<VfsRegistry>,
    archive_origin: VfsPath,
    counter: Arc<CountingVfs>,
    pending_read_streams: crate::api::PendingVfsReadStreams,
    host_communicator: Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
    progress_reporter: Arc<dyn crate::vfs::ProgressReporter>,
}

impl Harness {
    fn new(bytes: &[u8]) -> Self {
        let upstream = MockVfs::builder()
            .config(MockVfsConfig {
                strict_range_reads: true,
                ..MockVfsConfig::default()
            })
            .file(ARCHIVE_PATH, bytes)
            .build();
        let counter = CountingVfs::new(upstream);
        let archive_origin = VfsPath::from_wire_str(VfsId::ROOT, ARCHIVE_PATH);
        Self {
            registry: Arc::new(VfsRegistry::with_root(counter.clone())),
            archive_origin,
            counter,
            pending_read_streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            host_communicator: Arc::new(std::sync::OnceLock::new()),
            progress_reporter: Arc::new(crate::vfs::ScopedReporter::new(
                Arc::new(crate::vfs::NoopProgressSink),
                crate::vfs::VfsId(0),
            )),
        }
    }

    fn ctx<'a>(&'a self, askpass: Option<&'a Arc<dyn AskpassProvider>>) -> MountContext<'a> {
        MountContext {
            registry: &self.registry,
            host_communicator: &self.host_communicator,
            pending_read_streams: &self.pending_read_streams,
            sftp_askpass: None,
            askpass_provider: askpass,
            agent_resolver: None,
            extra_path: &[],
            progress_reporter: &self.progress_reporter,
        }
    }

    fn reads(&self) -> usize {
        self.counter.read_ranges.load(Ordering::SeqCst)
    }
}

async fn mount_with(
    h: &Harness,
    askpass: Option<&Arc<dyn AskpassProvider>>,
) -> Result<Arc<dyn Vfs>, crate::Error> {
    super::super::mount(h.archive_origin.clone(), &h.ctx(askpass)).await
}

async fn mount(h: &Harness) -> Arc<dyn Vfs> {
    mount_with(h, None).await.expect("mount")
}

async fn read_to_vec(vfs: &Arc<dyn Vfs>, path: &str) -> Result<Vec<u8>, crate::Error> {
    let mut reader = vfs.open_read_async(&vp(path)).await?;
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .await
        .map_err(crate::Error::from)?;
    Ok(buf)
}

fn names(list: &VfsFileList) -> Vec<String> {
    let mut v: Vec<String> = list.files.iter().map(|f| f.name.clone()).collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_root_dirs_and_symlink() {
    let h = Harness::new(BASIC);
    let vfs = mount(&h).await;
    let root = vfs.list_files(&vp("/"), None).await.unwrap();
    assert_eq!(
        names(&root),
        vec![
            "..",
            "dir",
            "empty.txt",
            "emptydir",
            "hello.txt",
            "links",
            "π — unicode.txt"
        ]
    );
    let dir = root.files.iter().find(|f| f.name == "dir").unwrap();
    assert!(dir.is_dir);
    let empty = root.files.iter().find(|f| f.name == "empty.txt").unwrap();
    assert!(!empty.is_dir);
    assert_eq!(empty.size, Some(0));
    assert_eq!(empty.modified, Some(1_700_000_000_000));

    let links = vfs.list_files(&vp("/links"), None).await.unwrap();
    let soft = links.files.iter().find(|f| f.name == "soft.txt").unwrap();
    assert!(soft.is_symlink);
    assert_eq!(soft.symlink_target.as_deref(), Some("../hello.txt"));
    // Resolved through the link: the target's size.
    assert_eq!(soft.size, Some(12));
    assert_eq!(
        read_to_vec(&vfs, "/links/soft.txt").await.unwrap(),
        b"hello world\n"
    );
}

#[tokio::test]
async fn reads_every_entry_streaming_and_by_range() {
    let h = Harness::new(BASIC);
    let vfs = mount(&h).await;
    let big = pattern(3, 300_000);
    assert_eq!(
        read_to_vec(&vfs, "/hello.txt").await.unwrap(),
        b"hello world\n"
    );
    assert_eq!(
        read_to_vec(&vfs, "/dir/nested.txt").await.unwrap(),
        b"nested content\n"
    );
    assert_eq!(read_to_vec(&vfs, "/dir/big.bin").await.unwrap(), big);
    assert!(read_to_vec(&vfs, "/empty.txt").await.unwrap().is_empty());
    assert_eq!(
        read_to_vec(&vfs, "/π — unicode.txt").await.unwrap(),
        "unicode ☃\n".as_bytes()
    );

    let chunk = vfs
        .read_range(&vp("/dir/big.bin"), 123_456, 5000)
        .await
        .unwrap();
    assert_eq!(chunk.total_size, 300_000);
    assert_eq!(chunk.data, &big[123_456..128_456]);
    let tail = vfs
        .read_range(&vp("/dir/big.bin"), 299_990, 5000)
        .await
        .unwrap();
    assert_eq!(tail.data, &big[299_990..]);
    let past = vfs
        .read_range(&vp("/dir/big.bin"), 300_000, 10)
        .await
        .unwrap();
    assert!(past.data.is_empty());

    let details = vfs.file_details(&vp("/dir/big.bin")).await.unwrap();
    assert_eq!(details.size, 300_000);
    assert!(details.mode.is_some());
}

#[tokio::test]
async fn sequential_chunks_reuse_a_parked_reader() {
    let h = Harness::new(BASIC);
    let vfs = mount(&h).await;
    let big = pattern(3, 300_000);
    // Warm the mount, then count reads for a chunked fan-out.
    vfs.read_range(&vp("/dir/big.bin"), 0, 1).await.unwrap();
    let before = h.reads();
    let mut got = Vec::new();
    for off in (0..300_000u64).step_by(64 * 1024) {
        got.extend(
            vfs.read_range(&vp("/dir/big.bin"), off, 64 * 1024)
                .await
                .unwrap()
                .data,
        );
    }
    assert_eq!(got, big);
    // The packed folder is under 3 KiB: one slice fetch serves every chunk
    // once the reader is parked; without reuse each chunk would refetch.
    assert!(
        h.reads() - before <= 2,
        "expected a parked reader to serve sequential chunks, saw {} reads",
        h.reads() - before
    );
}

#[tokio::test]
async fn solid_folder_reads_out_of_order_through_checkpoints() {
    let h = Harness::new(SOLID);
    let vfs = mount(&h).await;
    let files = solid_set();
    // Back to front: each read lands before every parked reader, so it
    // must come from a checkpoint restore, never a re-decode from zero
    // once the index is built.
    for (name, data) in files.iter().rev() {
        assert_eq!(&read_to_vec(&vfs, name).await.unwrap(), data, "{}", name);
    }
    let (name, data) = &files[23];
    let chunk = vfs.read_range(&vp(name), 20_000, 1_000).await.unwrap();
    assert_eq!(chunk.data, &data[20_000..21_000]);
    let (name, data) = &files[5];
    let chunk = vfs.read_range(&vp(name), 0, 100).await.unwrap();
    assert_eq!(chunk.data, &data[..100]);
}

/// Solid folder streamed entry by entry in `read_order`: the reader parked
/// by each entry serves the next, so the packed bytes are fetched once.
#[tokio::test]
async fn streaming_in_read_order_fetches_the_folder_once() {
    let h = Harness::new(SOLID);
    let vfs = mount(&h).await;
    let mut files = solid_set();
    let paths = files.iter().map(|(n, _)| vp(n)).collect::<Vec<_>>();
    let order = vfs.read_order(&paths).await.unwrap().unwrap();
    assert_eq!(order.len(), files.len());
    assert!(order.iter().all(|&o| o != u64::MAX));
    let missing = vfs.read_order(&[vp("/solid/none")]).await.unwrap();
    assert_eq!(missing, Some(vec![u64::MAX]));

    let mut keyed = order.into_iter().zip(files.drain(..)).collect::<Vec<_>>();
    keyed.sort_by_key(|(k, _)| *k);
    let first = &keyed[0].1;
    assert_eq!(&read_to_vec(&vfs, &first.0).await.unwrap(), &first.1);
    let before = h.reads();
    for (_, (name, data)) in &keyed[1..] {
        assert_eq!(&read_to_vec(&vfs, name).await.unwrap(), data, "{}", name);
    }
    assert!(
        h.reads() - before <= 4,
        "expected the parked reader to carry every entry, saw {} reads",
        h.reads() - before
    );

    // Streams dropped part-way park their reader too.
    let before = h.reads();
    for (_, (name, _)) in &keyed {
        let mut reader = vfs.open_read_async(&vp(name)).await.unwrap();
        let mut head = [0u8; 100];
        reader.read_exact(&mut head).await.unwrap();
    }
    assert!(
        h.reads() - before <= 4,
        "expected dropped streams to park their reader, saw {} reads",
        h.reads() - before
    );
}

#[tokio::test]
async fn bcj_folder_reads() {
    let h = Harness::new(X86);
    let vfs = mount(&h).await;
    let got = read_to_vec(&vfs, "/prog.bin").await.unwrap();
    assert_eq!(got.len(), 60_000);
    assert_eq!(
        read_to_vec(&vfs, "/readme.txt").await.unwrap(),
        b"code-like payload\n"
    );
}

/// A BCJ2 folder fetches and decodes its side streams once, then reads
/// like any other; with a password, one prompt unlocks all four streams.
#[tokio::test]
async fn bcj2_folder_reads() {
    let h = Harness::new(BCJ2);
    let vfs = mount(&h).await;
    let prog = read_to_vec(&vfs, "/prog.bin").await.unwrap();
    assert_eq!(prog.len(), 60_000);
    let chunk = vfs
        .read_range(&vp("/prog.bin"), 30_000, 1_000)
        .await
        .unwrap();
    assert_eq!(chunk.data, &prog[30_000..31_000]);
    assert_eq!(
        read_to_vec(&vfs, "/readme.txt").await.unwrap(),
        b"code-like payload\n"
    );

    let h = Harness::new(BCJ2_ENCRYPTED);
    let stub = StubAskpass::new(vec![Some("wrong"), Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await.unwrap();
    assert_eq!(read_to_vec(&vfs, "/prog.bin").await.unwrap(), prog);
    assert_eq!(stub.prompts().len(), 2);
    assert_eq!(
        read_to_vec(&vfs, "/readme.txt").await.unwrap(),
        b"code-like payload\n"
    );
    assert_eq!(stub.prompts().len(), 2);
}

#[tokio::test]
async fn several_folders_read_independently() {
    let h = Harness::new(BLOCKS);
    let vfs = mount(&h).await;
    for (name, data) in solid_set().iter().rev() {
        assert_eq!(&read_to_vec(&vfs, name).await.unwrap(), data, "{}", name);
    }
    let h = Harness::new(NONSOLID);
    let vfs = mount(&h).await;
    assert_eq!(
        read_to_vec(&vfs, "/dir/big.bin").await.unwrap(),
        pattern(3, 100_000)
    );
    assert_eq!(
        read_to_vec(&vfs, "/hello.txt").await.unwrap(),
        b"hello world\n"
    );
    assert!(read_to_vec(&vfs, "/empty.txt").await.unwrap().is_empty());
}

#[tokio::test]
async fn ppmd_lists_but_read_is_not_supported() {
    let h = Harness::new(PPMD);
    let vfs = mount(&h).await;
    let root = vfs.list_files(&vp("/"), None).await.unwrap();
    assert!(names(&root).contains(&"hello.txt".to_string()));
    let err = read_to_vec(&vfs, "/hello.txt").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotSupported, "{}", err.message);
    let err = vfs.read_range(&vp("/hello.txt"), 0, 5).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotSupported);
}

#[tokio::test]
async fn not_a_7z_errors_on_first_use() {
    let h = Harness::new(b"definitely not a seven zip archive, just some bytes here");
    let vfs = mount(&h).await;
    let err = vfs.list_files(&vp("/"), None).await.unwrap_err();
    assert!(err.message.contains("not a 7z"), "{}", err.message);
}

// ---------------------------------------------------------------------------
// Encryption
// ---------------------------------------------------------------------------

#[tokio::test]
async fn encrypted_data_mounts_silently_and_prompts_on_read() {
    let h = Harness::new(ENCRYPTED);
    let stub = StubAskpass::new(vec![Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await.unwrap();
    let root = vfs.list_files(&vp("/"), None).await.unwrap();
    assert!(stub.prompts().is_empty(), "listing must not prompt");
    assert!(names(&root).contains(&"hello.txt".to_string()));

    assert_eq!(
        read_to_vec(&vfs, "/hello.txt").await.unwrap(),
        b"hello world\n"
    );
    assert_eq!(stub.prompts().len(), 1);
    // The password is remembered for the rest of the mount.
    assert_eq!(
        read_to_vec(&vfs, "/dir/big.bin").await.unwrap(),
        pattern(3, 100_000)
    );
    assert_eq!(stub.prompts().len(), 1);
}

#[tokio::test]
async fn wrong_password_re_prompts_then_unlocks() {
    let h = Harness::new(ENCRYPTED);
    let stub = StubAskpass::new(vec![Some("nope"), Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await.unwrap();
    assert_eq!(
        read_to_vec(&vfs, "/hello.txt").await.unwrap(),
        b"hello world\n"
    );
    let prompts = stub.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts[1].starts_with("Incorrect password"),
        "{}",
        prompts[1]
    );
}

#[tokio::test]
async fn dismissed_prompt_cancels_and_a_later_read_prompts_again() {
    let h = Harness::new(ENCRYPTED);
    let stub = StubAskpass::new(vec![None, Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await.unwrap();
    // A viewer fires several reads of one entry at once; they queue on
    // the folder and all fail with the one dismissed prompt.
    let hello = vp("/hello.txt");
    let (a, b, c) = tokio::join!(
        read_to_vec(&vfs, "/hello.txt"),
        vfs.read_range(&hello, 0, 5),
        vfs.read_range(&hello, 5, 5),
    );
    assert_eq!(a.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(b.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(c.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(stub.prompts().len(), 1);
    assert_eq!(
        read_to_vec(&vfs, "/hello.txt").await.unwrap(),
        b"hello world\n"
    );
    assert_eq!(stub.prompts().len(), 2);
}

#[tokio::test]
async fn encrypted_without_askpass_is_permission_denied() {
    let h = Harness::new(ENCRYPTED);
    let vfs = mount(&h).await;
    let err = read_to_vec(&vfs, "/hello.txt").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn encrypted_header_prompts_at_mount() {
    let h = Harness::new(ENCRYPTED_HEADER);
    let stub = StubAskpass::new(vec![Some("wrong"), Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await.unwrap();
    // The probe runs on first use and needs the password to list at all.
    let root = vfs.list_files(&vp("/"), None).await.unwrap();
    assert!(names(&root).contains(&"hello.txt".to_string()));
    assert_eq!(stub.prompts().len(), 2);
    // Data folders reuse the remembered password: no third prompt.
    assert_eq!(
        read_to_vec(&vfs, "/dir/big.bin").await.unwrap(),
        pattern(3, 100_000)
    );
    assert_eq!(stub.prompts().len(), 2);
}

#[tokio::test]
async fn encrypted_header_dismissed_fails_the_listing() {
    let h = Harness::new(ENCRYPTED_HEADER);
    let stub = StubAskpass::new(vec![None]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await.unwrap();
    // Everything queued behind the dismissed prompt fails with it: one
    // prompt, not one per caller.
    let (root, hello, dir) = (vp("/"), vp("/hello.txt"), vp("/dir"));
    let (a, b, c) = tokio::join!(
        vfs.list_files(&root, None),
        vfs.file_details(&hello),
        vfs.list_files(&dir, None),
    );
    assert_eq!(a.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(b.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(c.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(stub.prompts().len(), 1);
    // A later attempt prompts afresh.
    let err = vfs.list_files(&vp("/"), None).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert_eq!(stub.prompts().len(), 2);
}

// ---------------------------------------------------------------------------
// Through the real local filesystem
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reads_through_local_vfs_with_timeout() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("solid.7z");
    std::fs::write(&path, SOLID).unwrap();
    let local: Arc<dyn Vfs> = Arc::new(crate::vfs::local::LocalVfs::new());
    let registry = Arc::new(VfsRegistry::with_root(local));
    let origin = VfsPath::new(VfsId::ROOT, PathBuf::from_native(&path));
    let pending: crate::api::PendingVfsReadStreams =
        Arc::new(parking_lot::Mutex::new(HashMap::new()));
    let host = Arc::new(std::sync::OnceLock::new());
    let reporter: Arc<dyn crate::vfs::ProgressReporter> = Arc::new(
        crate::vfs::ScopedReporter::new(Arc::new(crate::vfs::NoopProgressSink), VfsId(0)),
    );
    let ctx = MountContext {
        registry: &registry,
        host_communicator: &host,
        pending_read_streams: &pending,
        sftp_askpass: None,
        askpass_provider: None,
        agent_resolver: None,
        extra_path: &[],
        progress_reporter: &reporter,
    };
    let vfs = super::super::mount(origin, &ctx).await.unwrap();
    let files = solid_set();
    let t = Duration::from_secs(10);
    let root = tokio::time::timeout(t, vfs.list_files(&vp("/"), None))
        .await
        .expect("list hung")
        .unwrap();
    assert!(names(&root).contains(&"solid".to_string()));
    let (name, data) = &files[23];
    let got = tokio::time::timeout(t, read_to_vec(&vfs, name))
        .await
        .expect("streaming read hung")
        .unwrap();
    assert_eq!(&got, data);
    let chunk = tokio::time::timeout(t, vfs.read_range(&vp(&files[0].0), 0, 4096))
        .await
        .expect("range read hung")
        .unwrap();
    assert_eq!(chunk.data, &files[0].1[..4096]);
}
