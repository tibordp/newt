//! End-to-end tests for `ZipArchiveVfs` mounted via `archive::mount`.
//!
//! Focus: encrypted-archive lazy unlock UX. Mount never prompts (the
//! ZIP central directory is always cleartext); reads of encrypted
//! entries trigger the askpass prompt on demand, validating the
//! password against the actual entry being read and caching it for
//! subsequent reads.
//!
//! Fixture (`fixtures/encrypted.zip`, regenerated via `regenerate.py`):
//!
//! ```text
//! plain.txt   "unencrypted\n"   not encrypted
//! secret.txt  "top secret\n"    ZipCrypto-encrypted, password "secret"
//! ```

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::sync::Notify;

use async_trait::async_trait;

use crate::ErrorKind;
use crate::api::MountContext;
use crate::askpass::{AskpassProvider, AskpassRequest, AskpassResponse};
use crate::test_support::{MockVfs, MockVfsConfig};
use crate::vfs::path::PathBuf;
use crate::vfs::{VfsId, VfsPath, VfsRegistry};

/// Build a VFS path from a wire string.
fn vp(s: &str) -> PathBuf {
    PathBuf::from_wire_str(s)
}

const ENCRYPTED_ZIP: &[u8] = include_bytes!("fixtures/encrypted.zip");

const ARCHIVE_PATH: &str = "/archive.zip";

const PLAIN_CONTENT: &[u8] = b"unencrypted\n";
const SECRET_CONTENT: &[u8] = b"top secret\n";

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

struct Harness {
    registry: Arc<VfsRegistry>,
    archive_origin: VfsPath,
    pending_read_streams: crate::api::PendingVfsReadStreams,
    host_communicator: Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
    progress_reporter: Arc<dyn crate::vfs::ProgressReporter>,
}

impl Harness {
    fn new(zip_bytes: &[u8]) -> Self {
        let upstream = MockVfs::builder()
            .config(MockVfsConfig {
                can_read_sync: true,
                can_read_async: true,
                ..MockVfsConfig::default()
            })
            .file(ARCHIVE_PATH, zip_bytes)
            .build();

        let registry = Arc::new(VfsRegistry::with_root(upstream));
        let archive_origin = VfsPath::from_wire_str(VfsId::ROOT, ARCHIVE_PATH);

        Self {
            registry,
            archive_origin,
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
    let mut reader = vfs.open_read_sync(&vp(path)).await?;
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).map_err(crate::Error::from)?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Mount + listing always succeed
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
    // Second read uses the cached password — StubAskpass would panic
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

    // Give the tasks time to all enter extract_zip_file and queue up.
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
    // One canned response: dismiss. If a second prompt fires (the bug
    // we're fixing), the stub will panic with "ran out of canned
    // responses".
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
