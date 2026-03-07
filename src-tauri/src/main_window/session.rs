use newt_common::api::{VfsRegistryManager, API_LIST_FILES_BATCH, API_OPERATION_PROGRESS};
use newt_common::file_reader::FileReader;
use newt_common::filesystem::{
    FileList, Filesystem, LocalShellService, PendingStreams, ShellRemote, ShellService, StreamId,
};
use newt_common::operation::{OperationContext, OperationProgress, OperationsClient};
use newt_common::rpc::Communicator;
use newt_common::terminal::TerminalClient;
use newt_common::vfs::{
    LocalVfs, MountedVfsInfo, VfsId, VfsManager, VfsManagerRemote, VfsPath, VfsRegistry,
    VfsRegistryFileReader, VfsRegistryFs, LOCAL_VFS_DESCRIPTOR,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::common::{Error, UpdatePublisher};

use super::{apply_operation_progress, MainWindowState, Operations};

// ---------------------------------------------------------------------------
// ConnectionTarget
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum ConnectionTarget {
    Local,
    Remote { transport_cmd: Vec<String> },
    Elevated,
}

// ---------------------------------------------------------------------------
// ConnectionStatus (serialized to the frontend via MainWindowState)
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ConnectionStatus {
    Connecting { message: String },
    Connected { log: Vec<String> },
    Disconnected { log: Vec<String>, error: String },
    Failed { log: Vec<String>, error: String },
}

#[derive(Clone)]
pub struct ConnectionState(pub Arc<RwLock<ConnectionStatus>>);

impl Default for ConnectionState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(ConnectionStatus::Connecting {
            message: "Loading...".into(),
        })))
    }
}

impl serde::Serialize for ConnectionState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

// ---------------------------------------------------------------------------
// AgentResolver
// ---------------------------------------------------------------------------

const BOOTSTRAP_SCRIPT: &str = include_str!("../../../scripts/bootstrap.sh");

/// Resolves agent binary locations. Searches directories in priority order:
/// 1. `NEWT_AGENT_DIR` env var (dev override)
/// 2. Tauri resource dir (`agents/` inside the bundled app)
/// 3. `agents/` relative fallback (legacy/dev)
pub struct AgentResolver {
    dirs: Vec<PathBuf>,
}

impl AgentResolver {
    pub fn new(app_handle: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        let mut dirs = Vec::new();

        if let Ok(dir) = std::env::var("NEWT_AGENT_DIR") {
            dirs.push(PathBuf::from(dir));
        }

        if let Ok(resource_dir) = app_handle.path().resource_dir() {
            dirs.push(resource_dir.join("agents"));
        }

        dirs.push(PathBuf::from("agents"));

        Self { dirs }
    }

    /// Compute a hash that changes whenever any agent binary changes.
    pub fn agent_hash(&self) -> Result<String, Error> {
        let mut hasher = blake3::Hasher::new();
        let mut found = false;

        for dir in &self.dirs {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path().join("newt-agent");
                    if path.is_file() {
                        hasher.update(&std::fs::read(&path)?);
                        found = true;
                    }
                    // Also check flat layout (dir/newt-agent)
                    let flat = entry.path();
                    if flat.is_file() && flat.file_name().is_some_and(|n| n == "newt-agent") {
                        hasher.update(&std::fs::read(&flat)?);
                        found = true;
                    }
                }
            }
            // Flat layout: dir/newt-agent directly
            let flat = dir.join("newt-agent");
            if flat.is_file() {
                hasher.update(&std::fs::read(&flat)?);
                found = true;
            }
        }

        if !found {
            return Err(Error::Custom(
                "no agent binaries found to compute hash".into(),
            ));
        }

        Ok(hasher.finalize().to_hex()[..16].to_string())
    }

    /// Look up the agent binary for a given target triple.
    pub fn find_agent_binary(&self, triple: &str) -> Result<PathBuf, Error> {
        for dir in &self.dirs {
            let path = dir.join(triple).join("newt-agent");
            if path.exists() {
                return Ok(path);
            }
            let path = dir.join("newt-agent");
            if path.exists() {
                return Ok(path);
            }
        }

        Err(Error::Custom(format!(
            "agent binary not found for triple: {}. Set NEWT_AGENT_DIR to the directory containing the agent binary.",
            triple
        )))
    }

    /// Find the agent binary on the local machine (for elevated mode).
    /// Maps the compile-time target to the agent triple (always musl on Linux).
    pub fn find_local_agent_binary(&self) -> Result<PathBuf, Error> {
        let triple = local_agent_triple();
        self.find_agent_binary(&triple)
    }
}

/// Map the compile-time target to the agent binary triple.
/// Agents are always musl on Linux, so e.g. `x86_64-unknown-linux-gnu` → `x86_64-unknown-linux-musl`.
fn local_agent_triple() -> String {
    let target = env!("NEWT_TARGET_TRIPLE");
    if target.contains("-linux-") {
        // Replace the environment suffix with musl
        let arch = target.split('-').next().unwrap_or("x86_64");
        format!("{}-unknown-linux-musl", arch)
    } else {
        target.to_string()
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

pub struct Session {
    pub(super) fs: Arc<dyn Filesystem>,
    pub(super) shell_service: Arc<dyn ShellService>,
    pub(super) vfs_manager: Arc<dyn VfsManager>,
    pub(super) terminal_client: Arc<dyn TerminalClient>,
    pub(super) file_reader: Arc<dyn FileReader>,
    pub(super) operations_client: Arc<dyn OperationsClient>,
    pub(super) hot_paths_provider: Arc<dyn newt_common::hot_paths::HotPathsProvider>,
    pub(super) mounted_vfs: Arc<RwLock<HashMap<VfsId, MountedVfsInfo>>>,
    pub(super) next_operation_id: AtomicU64,
    pub(super) file_server_port: u16,
    pub(super) file_server_token: String,
    _file_server_handle: tokio::task::JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self._file_server_handle.abort();
    }
}

// ---------------------------------------------------------------------------
// Stderr log buffer — shared between the background reader and the context
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub(super) struct StderrLog(Arc<RwLock<Vec<String>>>);

impl StderrLog {
    pub fn lines(&self) -> Vec<String> {
        self.0.read().clone()
    }

    fn push(&self, line: String) {
        self.0.write().push(line);
    }
}

impl ConnectionState {
    pub fn set_connecting(&self, message: &str) {
        *self.0.write() = ConnectionStatus::Connecting {
            message: message.to_string(),
        };
    }

    pub fn set_failed(&self, error: String) {
        *self.0.write() = ConnectionStatus::Failed {
            log: Vec::new(),
            error,
        };
    }
}

/// Spawn a task that reads lines from `stderr` and appends them to `log`.
/// Publishes after each line so the frontend can see logs in real-time.
fn spawn_stderr_reader(
    stderr: tokio::process::ChildStderr,
    log: StderrLog,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    log.push(line.trim_end().to_string());
                    let _ = publisher.publish();
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// RPC dispatcher — receives notifications from the agent
// ---------------------------------------------------------------------------

struct HostDispatcher {
    operations: Operations,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
    pending_streams: PendingStreams,
}

#[async_trait::async_trait]
impl newt_common::rpc::Dispatcher for HostDispatcher {
    async fn invoke(
        &self,
        _api: newt_common::rpc::Api,
        _req: bytes::Bytes,
    ) -> Result<Option<bytes::Bytes>, newt_common::Error> {
        Ok(None)
    }

    async fn notify(
        &self,
        api: newt_common::rpc::Api,
        req: bytes::Bytes,
    ) -> Result<bool, newt_common::Error> {
        if api == API_OPERATION_PROGRESS {
            let progress: OperationProgress = bincode::deserialize(&req[..]).unwrap();
            apply_operation_progress(&self.operations, progress);
            let _ = self.publisher.publish();
            Ok(true)
        } else if api == API_LIST_FILES_BATCH {
            let (stream_id, file_list): (StreamId, FileList) =
                bincode::deserialize(&req[..]).unwrap();
            if let Some(tx) = self.pending_streams.lock().get(&stream_id) {
                let _ = tx.send(file_list);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// Service construction helpers
// ---------------------------------------------------------------------------

struct Services {
    fs: Arc<dyn Filesystem>,
    shell_service: Arc<dyn ShellService>,
    vfs_manager: Arc<dyn VfsManager>,
    terminal_client: Arc<dyn TerminalClient>,
    file_reader: Arc<dyn FileReader>,
    operations_client: Arc<dyn OperationsClient>,
    hot_paths_provider: Arc<dyn newt_common::hot_paths::HotPathsProvider>,
    initial_dir: VfsPath,
}

/// Build local (in-process) services.
fn create_local_services(
    operations: &Operations,
    publisher: &Arc<UpdatePublisher<MainWindowState>>,
) -> Services {
    let (progress_tx, mut progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<OperationProgress>();

    let registry = Arc::new(VfsRegistry::with_root(Arc::new(LocalVfs::new())));
    let op_context = Arc::new(OperationContext {
        registry: registry.clone(),
    });

    let operations = operations.clone();
    let publisher_clone = publisher.clone();
    tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            apply_operation_progress(&operations, progress);
            let _ = publisher_clone.publish();
        }
    });

    Services {
        fs: Arc::new(VfsRegistryFs::new(registry.clone())),
        shell_service: Arc::new(LocalShellService),
        vfs_manager: Arc::new(VfsRegistryManager::new(registry.clone())),
        terminal_client: Arc::new(newt_common::terminal::Local::new()),
        file_reader: Arc::new(VfsRegistryFileReader::new(registry.clone())),
        operations_client: Arc::new(newt_common::operation::Local::new(progress_tx, op_context)),
        hot_paths_provider: Arc::new(newt_common::hot_paths::Local::new()),
        initial_dir: VfsPath::root(std::env::current_dir().unwrap()),
    }
}

/// Build remote proxy services from a communicator.
fn create_remote_services(communicator: Communicator, pending_streams: PendingStreams) -> Services {
    Services {
        fs: Arc::new(newt_common::filesystem::Remote::new_with_streams(
            communicator.clone(),
            pending_streams,
        )),
        shell_service: Arc::new(ShellRemote::new(communicator.clone())),
        vfs_manager: Arc::new(VfsManagerRemote::new(communicator.clone())),
        terminal_client: Arc::new(newt_common::terminal::Remote::new(communicator.clone())),
        file_reader: Arc::new(newt_common::file_reader::Remote::new(communicator.clone())),
        operations_client: Arc::new(newt_common::operation::Remote::new(communicator.clone())),
        hot_paths_provider: Arc::new(newt_common::hot_paths::Remote::new(communicator)),
        initial_dir: VfsPath::root("/"),
    }
}

/// Set up the communicator + host dispatcher over a bidirectional stream,
/// and return the services + communicator.
fn create_rpc_services(
    stream: impl AsyncRead + AsyncWrite + Send + Unpin + 'static,
    operations: &Operations,
    publisher: &Arc<UpdatePublisher<MainWindowState>>,
) -> (Services, PendingStreams) {
    let pending_streams: PendingStreams = Arc::new(parking_lot::Mutex::new(HashMap::new()));

    let host_dispatcher = HostDispatcher {
        operations: operations.clone(),
        publisher: publisher.clone(),
        pending_streams: pending_streams.clone(),
    };
    let communicator = Communicator::with_dispatcher(host_dispatcher, stream);
    let services = create_remote_services(communicator, pending_streams.clone());
    (services, pending_streams)
}

// ---------------------------------------------------------------------------
// Child process spawning
// ---------------------------------------------------------------------------

type DynStream =
    tokio_duplex::Duplex<Box<dyn AsyncRead + Send + Unpin>, Box<dyn AsyncWrite + Send + Unpin>>;

/// Result of spawning a child process with a bidirectional RPC stream.
struct ChildConnection {
    child: tokio::process::Child,
    stderr: tokio::process::ChildStderr,
    stream: DynStream,
}

fn make_stream(
    reader: BufReader<tokio::process::ChildStdout>,
    stdin: tokio::process::ChildStdin,
) -> DynStream {
    let rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(reader);
    let tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
    tokio_duplex::Duplex::new(rx, tx)
}

/// Spawn SSH + bootstrap script, negotiate agent upload if needed.
async fn spawn_remote(
    transport_cmd: &[String],
    agent_resolver: &AgentResolver,
) -> Result<ChildConnection, Error> {
    let (program, args) = transport_cmd
        .split_first()
        .ok_or_else(|| Error::Custom("empty transport command".into()))?;

    let script = BOOTSTRAP_SCRIPT.replace("__NEWT_HASH__", &agent_resolver.agent_hash()?);
    let escaped = script.replace('\'', "'\\''");
    let sh_cmd = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        let escaped_val = rust_log.replace('\'', "'\\''");
        format!("NEWT_RUST_LOG='{}' sh -c '{}'", escaped_val, escaped)
    } else {
        format!("sh -c '{}'", escaped)
    };

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .arg(&sh_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Read status line, skipping any noise from .bashrc etc.
    let mut reader = BufReader::new(stdout);
    let status_line = loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // Connection closed — drain stderr for context
            let mut buf_stderr = BufReader::new(stderr);
            let mut stderr_lines = Vec::new();
            let mut stderr_line = String::new();
            while let Ok(n) = buf_stderr.read_line(&mut stderr_line).await {
                if n == 0 {
                    break;
                }
                stderr_lines.push(stderr_line.trim_end().to_string());
                stderr_line.clear();
            }
            let detail = stderr_lines
                .last()
                .map(|l| format!(": {}", l))
                .unwrap_or_default();
            return Err(Error::Custom(format!(
                "remote connection closed before bootstrap completed{}",
                detail
            )));
        }
        let trimmed = line.trim();
        if trimmed.starts_with("NEWT:") {
            break trimmed.to_string();
        }
        log::debug!("bootstrap noise: {}", trimmed);
    };
    let status_line = status_line.as_str();

    if status_line == "NEWT:READY" {
        Ok(ChildConnection {
            stream: make_stream(reader, stdin),
            child,
            stderr,
        })
    } else if let Some(triple) = status_line.strip_prefix("NEWT:NEED:") {
        let binary_path = agent_resolver.find_agent_binary(triple)?;
        let binary_data = tokio::fs::read(&binary_path).await?;
        let size = binary_data.len();

        stdin.write_all(format!("{}\n", size).as_bytes()).await?;
        stdin.write_all(&binary_data).await?;
        stdin.flush().await?;

        Ok(ChildConnection {
            stream: make_stream(reader, stdin),
            child,
            stderr,
        })
    } else if let Some(error) = status_line.strip_prefix("NEWT:ERROR:") {
        Err(Error::Custom(format!("remote bootstrap error: {}", error)))
    } else {
        Err(Error::Custom(format!(
            "unexpected bootstrap response: {}",
            status_line
        )))
    }
}

/// Spawn pkexec + agent binary (elevated mode, Linux only).
async fn spawn_elevated(agent_resolver: &AgentResolver) -> Result<ChildConnection, Error> {
    if cfg!(not(target_os = "linux")) {
        return Err(Error::Custom(
            "elevated mode is not supported on this platform".into(),
        ));
    }

    let agent_path = agent_resolver.find_local_agent_binary()?;
    let mut cmd = tokio::process::Command::new("pkexec");
    cmd.arg(&agent_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        cmd.env("RUST_LOG", rust_log);
    }
    let mut child = cmd.spawn()?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(stdout);
    let tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
    let stream = tokio_duplex::Duplex::new(rx, tx);

    Ok(ChildConnection {
        child,
        stream,
        stderr,
    })
}

/// Spawn a background task that waits for the child to exit, then clears the
/// session and sets the connection status to `Disconnected`.
fn spawn_child_watcher(
    mut child: tokio::process::Child,
    stderr: tokio::process::ChildStderr,
    stderr_log: StderrLog,
    session_slot: Arc<arc_swap::ArcSwap<Option<Session>>>,
    connection_status: ConnectionState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
) {
    // Start capturing stderr immediately
    spawn_stderr_reader(stderr, stderr_log.clone(), publisher.clone());

    tokio::spawn(async move {
        let status = child.wait().await;
        log::info!("child process exited: {:?}", status);

        // Drop the session — this aborts the file server, etc.
        session_slot.store(Arc::new(None));

        let log = stderr_log.lines();
        let error = match status {
            Ok(s) if s.success() => "process exited".to_string(),
            Ok(s) => format!("process exited with {}", s),
            Err(e) => format!("failed to wait for process: {}", e),
        };

        *connection_status.0.write() = ConnectionStatus::Disconnected { log, error };
        let _ = publisher.publish();
    });
}

// ---------------------------------------------------------------------------
// Public connect entry point
// ---------------------------------------------------------------------------

/// Establish a session according to the connection target. On success, stores
/// the session and sets status to `Connected`. On child-process exit, sets
/// status to `Disconnected` and clears the session.
pub(super) async fn connect(
    connection_target: &ConnectionTarget,
    agent_resolver: &AgentResolver,
    state: &MainWindowState,
    publisher: &Arc<UpdatePublisher<MainWindowState>>,
    session_slot: &Arc<arc_swap::ArcSwap<Option<Session>>>,
    set_status: impl Fn(&str),
) -> Result<(), Error> {
    let (services, stderr_log, child) = match connection_target {
        ConnectionTarget::Local => {
            let services = create_local_services(&state.operations, publisher);
            (services, StderrLog::default(), None)
        }
        ConnectionTarget::Remote { transport_cmd } => {
            set_status("Connecting to remote host...");
            let conn = spawn_remote(transport_cmd, agent_resolver).await?;
            let (services, _) = create_rpc_services(conn.stream, &state.operations, publisher);
            let stderr_log = StderrLog::default();
            (services, stderr_log, Some((conn.child, conn.stderr)))
        }
        ConnectionTarget::Elevated => {
            set_status("Waiting for authorization...");
            let conn = spawn_elevated(agent_resolver).await?;
            let (services, _) = create_rpc_services(conn.stream, &state.operations, publisher);
            let stderr_log = StderrLog::default();
            (services, stderr_log, Some((conn.child, conn.stderr)))
        }
    };

    // Resolve initial directory for remote connections
    let initial_dir = if matches!(connection_target, ConnectionTarget::Local) {
        services.initial_dir.clone()
    } else {
        services
            .shell_service
            .shell_expand("~".to_string())
            .await
            .unwrap_or(services.initial_dir.clone())
    };

    // Set up VFS
    let mut initial_mounted = HashMap::new();
    initial_mounted.insert(
        VfsId::ROOT,
        MountedVfsInfo {
            vfs_id: VfsId::ROOT,
            descriptor: &LOCAL_VFS_DESCRIPTOR,
            mount_meta: Vec::new(),
        },
    );
    let mounted_vfs = Arc::new(RwLock::new(initial_mounted));

    let lookup_ref = mounted_vfs.clone();
    let descriptor_lookup: super::pane::DescriptorLookup =
        Arc::new(move |vfs_id| lookup_ref.read().get(&vfs_id).map(|info| info.descriptor));

    state.panes.add(super::pane::Pane::new(
        services.fs.clone(),
        initial_dir.clone(),
        state.display_options.clone(),
        publisher.clone(),
        descriptor_lookup.clone(),
    ));
    state.panes.add(super::pane::Pane::new(
        services.fs.clone(),
        initial_dir,
        state.display_options.clone(),
        publisher.clone(),
        descriptor_lookup,
    ));

    set_status("Loading...");
    state.refresh().await?;

    for pane in state.panes.all() {
        tauri::async_runtime::spawn(async move {
            pane.watch_changes().await;
        });
    }

    let file_server_token = uuid::Uuid::new_v4().to_string();
    let (file_server_port, file_server_handle) =
        crate::file_server::start(services.file_reader.clone(), file_server_token.clone());

    let session = Session {
        fs: services.fs,
        shell_service: services.shell_service,
        vfs_manager: services.vfs_manager,
        terminal_client: services.terminal_client,
        file_reader: services.file_reader,
        operations_client: services.operations_client,
        hot_paths_provider: services.hot_paths_provider,
        mounted_vfs,
        next_operation_id: AtomicU64::new(1),
        file_server_port,
        file_server_token,
        _file_server_handle: file_server_handle,
    };

    session_slot.store(Arc::new(Some(session)));

    // Spawn process watcher for remote/elevated
    if let Some((child, stderr)) = child {
        spawn_child_watcher(
            child,
            stderr,
            stderr_log.clone(),
            Arc::clone(session_slot),
            state.connection_status.clone(),
            publisher.clone(),
        );
    }

    let log = stderr_log.lines();
    *state.connection_status.0.write() = ConnectionStatus::Connected { log };
    publisher.publish()?;

    Ok(())
}
