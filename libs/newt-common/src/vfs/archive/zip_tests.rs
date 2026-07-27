//! End-to-end tests for `ZipArchiveVfs` mounted via `archive::mount`.
//!
//! Fixtures (regenerated via `fixtures/regenerate.py`):
//!
//! `encrypted.zip` — lazy-unlock UX (mount never prompts; the central
//! directory is always cleartext):
//!
//! ```text
//! plain.txt   "unencrypted\n"   not encrypted
//! secret.txt  "top secret\n"    ZipCrypto-encrypted, password "secret"
//! ```
//!
//! `varied.zip` — the structural gamut (Python zipfile):
//!
//! ```text
//! hello.txt            stored, mode 0644
//! dir/                 explicit directory, mode 0755
//! dir/nested.txt       deflated
//! dir/big.bin          deflated, 200_000 patterned bytes
//! implicit/deep.txt    bzip2 (no entry for implicit/)
//! packed.lzma          lzma
//! links/soft.txt       symlink -> ../hello.txt
//! π — unicode.txt      deflated, UTF-8-flagged name
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::{Notify, mpsc};

use async_trait::async_trait;

use crate::ErrorKind;
use crate::api::MountContext;
use crate::askpass::{AskpassProvider, AskpassRequest, AskpassResponse};
use crate::test_support::{MockVfs, MockVfsConfig};
use crate::vfs::File;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{FileChunk, FileDetails};
use crate::vfs::{Vfs, VfsDescriptor, VfsFileList, VfsId, VfsPath, VfsRegistry};

/// Build a VFS path from a wire string.
fn vp(s: &str) -> PathBuf {
    PathBuf::from_wire_str(s)
}

const ENCRYPTED_ZIP: &[u8] = include_bytes!("fixtures/encrypted.zip");
const VARIED_ZIP: &[u8] = include_bytes!("fixtures/varied.zip");

const ARCHIVE_PATH: &str = "/archive.zip";

const PLAIN_CONTENT: &[u8] = b"unencrypted\n";
const SECRET_CONTENT: &[u8] = b"top secret\n";

fn big_bytes() -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 251) as u8).collect()
}

/// Stub provider that hands out a queue of canned responses, one per
/// prompt. Useful for simulating "wrong, then right" sequences and
/// cancellation.
struct StubAskpass {
    responses: StdMutex<Vec<Option<&'static str>>>,
    prompts: StdMutex<Vec<String>>,
    /// If set, each prompt waits on this Notify before reading its
    /// response. Lets tests pile up concurrent reads behind a single
    /// pending prompt before unblocking it.
    gate: Option<Arc<Notify>>,
}

impl StubAskpass {
    fn new(responses: Vec<Option<&'static str>>) -> Arc<Self> {
        Arc::new(Self {
            responses: StdMutex::new(responses.into_iter().rev().collect()),
            prompts: StdMutex::new(Vec::new()),
            gate: None,
        })
    }

    fn gated(responses: Vec<Option<&'static str>>, gate: Arc<Notify>) -> Arc<Self> {
        Arc::new(Self {
            responses: StdMutex::new(responses.into_iter().rev().collect()),
            prompts: StdMutex::new(Vec::new()),
            gate: Some(gate),
        })
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl AskpassProvider for StubAskpass {
    async fn prompt(&self, req: AskpassRequest) -> AskpassResponse {
        self.prompts.lock().unwrap().push(req.prompt.clone());
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop()
            .expect("StubAskpass: ran out of canned responses");
        AskpassResponse(next.map(|s| s.to_string()))
    }
}

/// Delegating wrapper that counts upstream reads (`read_range` one-shots
/// and per-handle `read_at`s) — the point of the sans-IO reader is read
/// efficiency on high-latency upstreams.
struct CountingVfs {
    inner: Arc<dyn Vfs>,
    read_ranges: Arc<AtomicUsize>,
}

impl CountingVfs {
    fn new(inner: Arc<dyn Vfs>) -> Arc<Self> {
        Arc::new(CountingVfs {
            inner,
            read_ranges: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn reads(&self) -> usize {
        self.read_ranges.load(Ordering::SeqCst)
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
    fn new(zip_bytes: &[u8]) -> Self {
        let upstream = MockVfs::builder()
            .config(MockVfsConfig {
                // Model object stores: a range read at/past EOF errors.
                strict_range_reads: true,
                ..MockVfsConfig::default()
            })
            .file(ARCHIVE_PATH, zip_bytes)
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
}

async fn mount_with(
    h: &Harness,
    askpass: Option<&Arc<dyn AskpassProvider>>,
) -> Arc<dyn crate::vfs::Vfs> {
    super::super::mount(h.archive_origin.clone(), &h.ctx(askpass))
        .await
        .expect("mount")
}

async fn read_to_vec(vfs: &Arc<dyn crate::vfs::Vfs>, path: &str) -> Result<Vec<u8>, crate::Error> {
    let mut reader = vfs.open_read_async(&vp(path)).await?;
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .await
        .map_err(crate::Error::from)?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Structure: listings, metadata, name decoding, symlinks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_root_with_implicit_dirs() {
    let h = Harness::new(VARIED_ZIP);
    let vfs = mount_with(&h, None).await;

    let files = vfs.list_files(&vp("/"), None).await.expect("list_files");
    let mut names: Vec<String> = files.files.iter().map(|f| f.name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "..",
            "dir",
            "hello.txt",
            "implicit",
            "links",
            "packed.lzma",
            "π — unicode.txt",
        ]
    );

    let by_name = |n: &str| files.files.iter().find(|f| f.name == n).unwrap();
    assert!(by_name("implicit").is_dir);
    let dir = by_name("dir");
    assert!(dir.is_dir);
    assert_eq!(dir.mode.as_ref().map(|m| m.0), Some(0o755));
    let hello = by_name("hello.txt");
    assert_eq!(hello.size, Some(12));
    assert_eq!(hello.mode.as_ref().map(|m| m.0), Some(0o644));
    assert!(hello.modified.is_some());
}

#[tokio::test]
async fn symlink_lists_with_target_and_reads_through() {
    let h = Harness::new(VARIED_ZIP);
    let vfs = mount_with(&h, None).await;

    let files = vfs.list_files(&vp("/links"), None).await.expect("list");
    let link = files.files.iter().find(|f| f.name == "soft.txt").unwrap();
    assert!(link.is_symlink);
    assert_eq!(link.symlink_target.as_deref(), Some("../hello.txt"));
    // lstat+stat mirror: target metadata fills is_dir/size.
    assert_eq!(link.size, Some(12));

    // Reads follow the link.
    assert_eq!(
        read_to_vec(&vfs, "/links/soft.txt").await.unwrap(),
        b"hello world\n"
    );
    let chunk = vfs
        .read_range(&vp("/links/soft.txt"), 6, 5)
        .await
        .expect("read_range");
    assert_eq!(chunk.data, b"world");
}

#[tokio::test]
async fn decompresses_every_fixture_method() {
    let h = Harness::new(VARIED_ZIP);
    let vfs = mount_with(&h, None).await;

    assert_eq!(
        read_to_vec(&vfs, "/dir/nested.txt").await.unwrap(),
        b"nested content\n"
    );
    assert_eq!(
        read_to_vec(&vfs, "/dir/big.bin").await.unwrap(),
        big_bytes()
    );
    assert_eq!(
        read_to_vec(&vfs, "/implicit/deep.txt").await.unwrap(),
        b"deep\n"
    );
    assert_eq!(
        read_to_vec(&vfs, "/packed.lzma").await.unwrap(),
        b"lzma packed\n"
    );
    assert_eq!(
        read_to_vec(&vfs, "/π — unicode.txt").await.unwrap(),
        b"unicode name\n"
    );
}

#[tokio::test]
async fn file_details_and_metadata() {
    let h = Harness::new(VARIED_ZIP);
    let vfs = mount_with(&h, None).await;

    let details = vfs.file_details(&vp("/dir/nested.txt")).await.unwrap();
    assert_eq!(details.size, 15);
    assert!(!details.is_dir);
    assert_eq!(details.mode.as_ref().map(|m| m.0), Some(0o644));

    // Metadata preservation input comes via the `get_metadata` default.
    let meta = vfs.get_metadata(&vp("/dir/nested.txt")).await.unwrap();
    assert_eq!(meta.permissions, Some(0o644));
    assert!(meta.mtime.is_some());
}

#[tokio::test]
async fn read_range_slices_match_streaming() {
    let h = Harness::new(VARIED_ZIP);
    let vfs = mount_with(&h, None).await;
    let big = big_bytes();

    for (offset, len) in [
        (0u64, 100u64),
        (64 * 1024, 4096),
        (199_900, 200),
        (150_000, 1),
    ] {
        let chunk = vfs
            .read_range(&vp("/dir/big.bin"), offset, len)
            .await
            .expect("read_range");
        let end = (offset + len).min(big.len() as u64) as usize;
        assert_eq!(chunk.data, big[offset as usize..end], "{}+{}", offset, len);
        assert_eq!(chunk.total_size, big.len() as u64);
    }

    // Past-EOF read returns an empty chunk, not an error.
    let past = vfs
        .read_range(&vp("/dir/big.bin"), 300_000, 10)
        .await
        .unwrap();
    assert!(past.data.is_empty());
}

#[tokio::test]
async fn not_a_zip_errors_on_first_use() {
    let h = Harness::new(b"this is not a zip archive at all");
    let vfs = mount_with(&h, None).await;
    let err = vfs.list_files(&vp("/"), None).await.unwrap_err();
    assert!(err.message.contains("not a ZIP"), "{}", err.message);
}

// ---------------------------------------------------------------------------
// Read efficiency — the reason this backend exists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_efficiency() {
    let h = Harness::new(VARIED_ZIP);
    let vfs = mount_with(&h, None).await;

    // Mounting is lazy — no reads yet.
    assert_eq!(h.counter.reads(), 0);

    // Probe (tail + central directory) plus the one symlink-target read
    // (local header + payload).
    vfs.list_files(&vp("/"), None).await.unwrap();
    let after_first_list = h.counter.reads();
    assert!(
        after_first_list <= 5,
        "probe + root listing took {} upstream reads",
        after_first_list
    );

    // Everything below is served from the parsed index: zero further reads.
    vfs.list_files(&vp("/dir"), None).await.unwrap();
    vfs.list_files(&vp("/implicit"), None).await.unwrap();
    vfs.file_details(&vp("/dir/big.bin")).await.unwrap();
    assert_eq!(h.counter.reads(), after_first_list);

    // First touch of a stored entry: local header + payload = 2 reads.
    vfs.read_range(&vp("/hello.txt"), 0, 5).await.unwrap();
    let after_stored = h.counter.reads();
    assert!(after_stored - after_first_list <= 2);

    // Subsequent range reads on it: exactly 1 pass-through read each.
    vfs.read_range(&vp("/hello.txt"), 6, 5).await.unwrap();
    assert_eq!(h.counter.reads(), after_stored + 1);
}

/// The F3 viewer pattern: sequential chunked range reads over a compressed
/// entry must reuse the decompression cursor, costing at most one upstream
/// read per chunk (and usually zero — read-ahead covers small chunks).
#[tokio::test]
async fn sequential_chunks_reuse_the_cursor() {
    let h = Harness::new(VARIED_ZIP);
    let vfs = mount_with(&h, None).await;
    let big = big_bytes();

    // Prime: open + first chunk.
    let first = vfs.read_range(&vp("/dir/big.bin"), 0, 8192).await.unwrap();
    assert_eq!(first.data, big[..8192]);
    let after_first = h.counter.reads();

    // 23 more sequential chunks; the whole compressed stream was already
    // fetched by the first read (it's tiny), so no further upstream reads
    // and no re-decompression from the start.
    for i in 1..24u64 {
        let chunk = vfs
            .read_range(&vp("/dir/big.bin"), i * 8192, 8192)
            .await
            .unwrap();
        let end = ((i + 1) * 8192).min(big.len() as u64) as usize;
        assert_eq!(chunk.data, big[(i * 8192) as usize..end], "chunk {}", i);
    }
    assert_eq!(
        h.counter.reads(),
        after_first,
        "sequential chunks must be served from the parked cursor"
    );
}

// ---------------------------------------------------------------------------
// Mount + listing always succeed for encrypted archives
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mount_succeeds_without_askpass_even_for_encrypted_archive() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let vfs = mount_with(&h, None).await;

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
    assert_eq!(names, vec!["plain.txt", "secret.txt"]);
}

#[tokio::test]
async fn cleartext_entry_reads_without_password() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let vfs = mount_with(&h, None).await;
    assert_eq!(
        read_to_vec(&vfs, "/plain.txt").await.unwrap(),
        PLAIN_CONTENT
    );
}

// ---------------------------------------------------------------------------
// Encrypted reads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn encrypted_entry_prompts_and_unlocks() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let stub = StubAskpass::new(vec![Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await;

    assert_eq!(
        read_to_vec(&vfs, "/secret.txt").await.unwrap(),
        SECRET_CONTENT
    );
    assert_eq!(stub.prompts().len(), 1);
}

#[tokio::test]
async fn cached_password_skips_subsequent_prompts() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let stub = StubAskpass::new(vec![Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await;

    // First read prompts.
    assert_eq!(
        read_to_vec(&vfs, "/secret.txt").await.unwrap(),
        SECRET_CONTENT
    );
    // Second read uses the cached key — StubAskpass would panic
    // ("ran out of canned responses") if a second prompt were issued.
    assert_eq!(
        read_to_vec(&vfs, "/secret.txt").await.unwrap(),
        SECRET_CONTENT
    );
    assert_eq!(stub.prompts().len(), 1);
}

#[tokio::test]
async fn wrong_password_re_prompts_with_hint() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let stub = StubAskpass::new(vec![Some("wrong"), Some("alsowrong"), Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await;

    assert_eq!(
        read_to_vec(&vfs, "/secret.txt").await.unwrap(),
        SECRET_CONTENT
    );

    let prompts = stub.prompts();
    assert_eq!(prompts.len(), 3, "expected 3 prompts, got {:?}", prompts);
    assert!(!prompts[0].contains("Incorrect"));
    assert!(prompts[1].contains("Incorrect password"));
    assert!(prompts[2].contains("Incorrect password"));
}

#[tokio::test]
async fn cancelled_prompt_returns_cancelled_and_allows_retry() {
    let h = Harness::new(ENCRYPTED_ZIP);
    // First read: user cancels. Second read: provides correct password.
    let stub = StubAskpass::new(vec![None, Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await;

    let err = read_to_vec(&vfs, "/secret.txt").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);

    // Second attempt re-prompts (cache wasn't populated by the cancel).
    assert_eq!(
        read_to_vec(&vfs, "/secret.txt").await.unwrap(),
        SECRET_CONTENT
    );
    assert_eq!(stub.prompts().len(), 2);
}

#[tokio::test]
async fn cancel_after_wrong_password_returns_cancelled() {
    let h = Harness::new(ENCRYPTED_ZIP);
    // Wrong password, then dismiss the "Incorrect password" re-prompt.
    let stub = StubAskpass::new(vec![Some("wrong"), None]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await;

    let err = read_to_vec(&vfs, "/secret.txt").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);

    let prompts = stub.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(!prompts[0].contains("Incorrect"));
    assert!(prompts[1].contains("Incorrect password"));
}

// ---------------------------------------------------------------------------
// Concurrent reads — the F3 / file-viewer scenario where a single user
// action fans out into N parallel range reads against the same encrypted
// entry. We must show *one* prompt (not N), and a dismissal must cancel
// the whole batch (not leave N more queued behind it).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_reads_share_a_single_prompt_on_success() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let gate = Arc::new(Notify::new());
    let stub = StubAskpass::gated(vec![Some("secret")], gate.clone());
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = Arc::new(mount_with(&h, Some(&askpass)).await);

    // Fan out 5 parallel reads. The first one to acquire the lock will
    // prompt and block on the gate; the others queue up.
    let mut handles = Vec::new();
    for _ in 0..5 {
        let vfs = vfs.clone();
        handles.push(tokio::spawn(async move {
            read_to_vec(&vfs, "/secret.txt").await
        }));
    }

    // Give the tasks time to all reach the password lock and queue up.
    tokio::time::sleep(Duration::from_millis(50)).await;
    gate.notify_one();

    for h in handles {
        assert_eq!(h.await.unwrap().unwrap(), SECRET_CONTENT);
    }
    assert_eq!(
        stub.prompts().len(),
        1,
        "expected exactly one prompt for 5 concurrent reads"
    );
}

#[tokio::test]
async fn concurrent_reads_dismiss_cancels_whole_batch() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let gate = Arc::new(Notify::new());
    // One canned response: dismiss. If a second prompt fires, the stub
    // will panic with "ran out of canned responses".
    let stub = StubAskpass::gated(vec![None], gate.clone());
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = Arc::new(mount_with(&h, Some(&askpass)).await);

    let mut handles = Vec::new();
    for _ in 0..5 {
        let vfs = vfs.clone();
        handles.push(tokio::spawn(async move {
            read_to_vec(&vfs, "/secret.txt").await
        }));
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    gate.notify_one();

    for h in handles {
        let err = h.await.unwrap().unwrap_err();
        assert_eq!(err.kind, ErrorKind::Cancelled);
    }
    assert_eq!(
        stub.prompts().len(),
        1,
        "dismissal of one prompt must not leave more queued"
    );
}

#[tokio::test]
async fn dismissal_does_not_block_a_subsequent_fresh_attempt() {
    let h = Harness::new(ENCRYPTED_ZIP);
    // Sequence: first attempt's prompt is dismissed; a *fresh* read
    // (started after the dismissal) gets its own prompt and succeeds.
    let stub = StubAskpass::new(vec![None, Some("secret")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await;

    let err = read_to_vec(&vfs, "/secret.txt").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);

    assert_eq!(
        read_to_vec(&vfs, "/secret.txt").await.unwrap(),
        SECRET_CONTENT
    );
    assert_eq!(stub.prompts().len(), 2);
}

#[tokio::test]
async fn encrypted_entry_without_askpass_errors_with_permission_denied() {
    let h = Harness::new(ENCRYPTED_ZIP);
    let vfs = mount_with(&h, None).await;

    let err = read_to_vec(&vfs, "/secret.txt").await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::PermissionDenied);
}

// ---------------------------------------------------------------------------
// WinZip AES — archive authored by our own ZipWriter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aes_encrypted_archive_round_trips() {
    let body = big_bytes();
    let mut bytes = Vec::new();
    let mut w = newt_archive::ZipWriter::new(None, Some("hunter2"));
    w.begin_file(
        "sealed.bin",
        Some(body.len() as u64),
        &Default::default(),
        &mut bytes,
    )
    .unwrap();
    w.write_data(&body, &mut bytes).unwrap();
    w.end_file(&mut bytes).unwrap();
    w.finish(&mut bytes).unwrap();

    let h = Harness::new(&bytes);
    let stub = StubAskpass::new(vec![Some("hunter2")]);
    let askpass: Arc<dyn AskpassProvider> = stub.clone();
    let vfs = mount_with(&h, Some(&askpass)).await;

    // Full streaming read verifies the HMAC (AE-2) at the end.
    assert_eq!(read_to_vec(&vfs, "/sealed.bin").await.unwrap(), body);
    // Range reads inside the AES stream work (CTR is seekable).
    let chunk = vfs
        .read_range(&vp("/sealed.bin"), 100_000, 1_000)
        .await
        .unwrap();
    assert_eq!(chunk.data, body[100_000..101_000]);
    assert_eq!(stub.prompts().len(), 1);
}
