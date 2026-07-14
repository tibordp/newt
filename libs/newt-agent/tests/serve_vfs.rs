//! End-to-end tests of the FS-only agent mode (`--serve-vfs`): spawn the
//! real agent binary, speak RPC over its stdio, and drive it through the
//! same `RemoteVfs` proxy arrangement an agent mount uses.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, BufReader};

use newt_common::api::{API_LIST_FILES, PendingVfsReadStreams, VfsReadChunkDispatcher};
use newt_common::connect::{AgentMode, ConnectLog, SpawnSpec, make_stream};
use newt_common::rpc::Communicator;
use newt_common::vfs::agent::AgentConnectionGuard;
use newt_common::vfs::path::PathBuf as VfsPathBuf;
use newt_common::vfs::{PathStyle, RemoteVfs, Vfs, encode_mount_meta_labeled};

fn agent_binary() -> &'static str {
    env!("CARGO_BIN_EXE_newt-agent")
}

fn vfs_path_of(native: &std::path::Path) -> VfsPathBuf {
    newt_common::vfs::local::local_path_from_native(native)
}

/// Wire a spawned serve-vfs agent's stdio into the proxy `RemoteVfs`,
/// exactly like `vfs::agent::mount_inner` does.
fn proxy_for(mut child: tokio::process::Child) -> (Arc<RemoteVfs>, Communicator) {
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stream = make_stream(BufReader::new(stdout), stdin);

    let pending: PendingVfsReadStreams = Default::default();
    let (outbox, inbox) = Communicator::create_outbox();
    let dispatcher = VfsReadChunkDispatcher::new(pending.clone());
    let communicator = Communicator::with_dispatcher_and_outbox(dispatcher, stream, outbox, inbox);

    let guard = AgentConnectionGuard::new(child, None);
    let mount_meta = encode_mount_meta_labeled(PathStyle::Unix, &[], Some("Docker"), Some("test"));
    let vfs = Arc::new(RemoteVfs::for_agent(
        communicator.clone(),
        pending,
        mount_meta,
        guard,
    ));
    (vfs, communicator)
}

#[tokio::test]
async fn serve_vfs_lists_and_reads_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hello from the sub-agent").unwrap();
    std::fs::create_dir(dir.path().join("subdir")).unwrap();

    let child = tokio::process::Command::new(agent_binary())
        .arg("--serve-vfs")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let (vfs, _communicator) = proxy_for(child);

    // Descriptor identity: an agent mount must not masquerade as "remote".
    assert_eq!(vfs.descriptor().type_name(), "agent");
    assert_eq!(
        vfs.descriptor().mount_label(&vfs.mount_meta()),
        Some("test".to_string())
    );
    assert_eq!(
        newt_common::vfs::mount_meta_kind(&vfs.mount_meta()),
        Some("Docker".to_string())
    );

    // list_files through the proxy hits the sub-agent's LocalVfs.
    let listing = vfs
        .list_files(&vfs_path_of(dir.path()), None)
        .await
        .unwrap();
    let names: Vec<&str> = listing.files.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"hello.txt"), "listing: {:?}", names);
    assert!(names.contains(&"subdir"), "listing: {:?}", names);

    // Streamed read through the chunk-notification path.
    let mut reader = vfs
        .open_read_async(&vfs_path_of(&dir.path().join("hello.txt")))
        .await
        .unwrap();
    let mut contents = Vec::new();
    reader.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, b"hello from the sub-agent");
}

#[tokio::test]
async fn serve_vfs_does_not_expose_session_api() {
    let child = tokio::process::Command::new(agent_binary())
        .arg("--serve-vfs")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let (vfs, communicator) = proxy_for(child);

    // Prove the connection is live first, so the assertion below can't
    // pass vacuously on a dead agent.
    vfs.list_files(&VfsPathBuf::root(), None).await.unwrap();

    // A full-session verb (FilesystemDispatcher's list_files) must not be
    // served: an FS-only agent never constructs those dispatchers, and
    // unknown APIs get no response. "No response within a generous window
    // while the same connection answers VFS verbs" is the observable form
    // of that.
    let args = (
        newt_common::vfs::VfsPath::root(newt_common::vfs::VfsId::ROOT),
        newt_common::filesystem::ListFilesOptions { strict: false },
    );
    let session_api = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        communicator.invoke::<_, Result<newt_common::filesystem::FileList, newt_common::Error>>(
            API_LIST_FILES,
            &args,
        ),
    )
    .await;
    assert!(
        session_api.is_err(),
        "serve-vfs agent answered a full-session API: {:?}",
        session_api.map(|r| r.map(|_| ()))
    );

    // The same connection still answers VFS verbs afterwards.
    vfs.list_files(&VfsPathBuf::root(), None).await.unwrap();
}

struct TestResolver;

#[async_trait::async_trait]
impl newt_common::agent_resolver::AgentResolver for TestResolver {
    async fn agent_hash(&self) -> Result<String, newt_common::Error> {
        Ok("cafebabe00112233".to_string())
    }
    fn find_agent_binary(&self, _triple: &str) -> Result<std::path::PathBuf, newt_common::Error> {
        Ok(agent_binary().into())
    }
    fn find_local_agent_binary(&self) -> Result<std::path::PathBuf, newt_common::Error> {
        Ok(agent_binary().into())
    }
}

struct EprintLog;

impl ConnectLog for EprintLog {
    fn log(&self, line: String) {
        eprintln!("connect: {}", line);
    }
}

struct NoAskpass;

#[async_trait::async_trait]
impl newt_common::askpass::AskpassProvider for NoAskpass {
    async fn prompt(
        &self,
        _req: newt_common::askpass::AskpassRequest,
    ) -> newt_common::askpass::AskpassResponse {
        newt_common::askpass::AskpassResponse(None)
    }
}

/// Full spawn path: bootstrap.sh runs locally (custom-shell transport),
/// negotiates the agent upload, and execs it. Passing this proves the
/// `NEWT_AGENT_MODE` injection reaches the script's exec line — a
/// full-session agent would not answer the host-VFS verbs the proxy uses.
#[tokio::test]
async fn bootstrap_spawns_serve_vfs_agent() {
    let cache = tempfile::tempdir().unwrap();
    let spec = SpawnSpec::CustomShell {
        // Env-assignment prefix keeps the script's agent cache inside the
        // tempdir without mutating this process's environment.
        command: format!(
            "XDG_CACHE_HOME='{}' sh -c \"$NEWT_BOOTSTRAP\"",
            cache.path().display()
        ),
        label: "test".to_string(),
        skip_bootstrap: false,
    };

    let spawned = newt_common::connect::spawn(
        &spec,
        AgentMode::ServeVfs,
        &[],
        &TestResolver,
        Arc::new(NoAskpass),
        Arc::new(EprintLog),
    )
    .await
    .unwrap();

    let pending: PendingVfsReadStreams = Default::default();
    let (outbox, inbox) = Communicator::create_outbox();
    let dispatcher = VfsReadChunkDispatcher::new(pending.clone());
    let communicator =
        Communicator::with_dispatcher_and_outbox(dispatcher, spawned.stream, outbox, inbox);
    let guard = AgentConnectionGuard::new(spawned.child, spawned.askpass);
    let vfs = RemoteVfs::for_agent(
        communicator,
        pending,
        encode_mount_meta_labeled(PathStyle::Unix, &[], Some("Custom"), Some("bootstrap-test")),
        guard,
    );

    let listing = vfs.list_files(&VfsPathBuf::root(), None).await.unwrap();
    assert!(!listing.files.is_empty());
}

// ---------------------------------------------------------------------------
// Full mount() path — startup probe and failure diagnostics
// ---------------------------------------------------------------------------

struct MountHarness {
    registry: Arc<newt_common::vfs::VfsRegistry>,
    host_communicator: Arc<std::sync::OnceLock<Communicator>>,
    pending: PendingVfsReadStreams,
    resolver: Arc<dyn newt_common::agent_resolver::AgentResolver>,
    reporter: Arc<dyn newt_common::vfs::ProgressReporter>,
}

impl MountHarness {
    fn new() -> Self {
        Self {
            registry: Arc::new(newt_common::vfs::VfsRegistry::with_root(Arc::new(
                newt_common::vfs::LocalVfs::new(),
            ))),
            host_communicator: Arc::new(std::sync::OnceLock::new()),
            pending: Default::default(),
            resolver: Arc::new(TestResolver),
            reporter: Arc::new(newt_common::vfs::ScopedReporter::new(
                Arc::new(newt_common::vfs::NoopProgressSink),
                newt_common::vfs::VfsId(1),
            )),
        }
    }

    fn ctx(&self) -> newt_common::api::MountContext<'_> {
        newt_common::api::MountContext {
            registry: &self.registry,
            host_communicator: &self.host_communicator,
            pending_read_streams: &self.pending,
            sftp_askpass: None,
            askpass_provider: None,
            agent_resolver: Some(&self.resolver),
            extra_path: &[],
            progress_reporter: &self.reporter,
        }
    }
}

/// The whole `vfs::agent::mount` path, startup probe included, against a
/// real serve-vfs agent.
#[tokio::test]
async fn agent_mount_succeeds_against_live_agent() {
    let harness = MountHarness::new();
    let spec = SpawnSpec::CustomShell {
        command: format!("'{}' --serve-vfs", agent_binary()),
        label: "test".to_string(),
        skip_bootstrap: true,
    };
    let vfs = newt_common::vfs::agent::mount(
        spec,
        "Custom".to_string(),
        "probe-test".to_string(),
        &harness.ctx(),
    )
    .await
    .unwrap();
    assert_eq!(vfs.descriptor().type_name(), "agent");
    vfs.list_files(&VfsPathBuf::root(), None).await.unwrap();
}

/// An agent that dies on startup (the direct-exec failure mode: wrong arch,
/// missing binary, …) must fail the mount with a diagnostic instead of
/// producing a VFS that fails every operation.
#[tokio::test]
async fn agent_mount_fails_when_agent_dies_on_startup() {
    let harness = MountHarness::new();
    let spec = SpawnSpec::CustomShell {
        // Prints a diagnostic to stderr and exits — an agent never comes up.
        command: "echo 'exec format error' >&2; true".to_string(),
        label: "test".to_string(),
        skip_bootstrap: true,
    };
    let err = newt_common::vfs::agent::mount(
        spec,
        "Custom".to_string(),
        "dead-agent".to_string(),
        &harness.ctx(),
    )
    .await
    .err()
    .expect("mount of a dead agent must fail");
    // Either race arm is a correct failure: the probe invoke erroring on
    // the closed channel, or the closed() branch winning.
    assert!(
        err.message.contains("exited during startup")
            || err.message.contains("startup probe failed"),
        "unexpected error: {}",
        err.message
    );
    // The stderr diagnostic must ride along in the connection log.
    assert!(
        err.message.contains("exec format error"),
        "stderr diagnostic missing from error: {}",
        err.message
    );
}
