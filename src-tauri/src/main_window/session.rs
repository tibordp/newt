use newt_common::api::{API_LIST_FILES_BATCH, API_OPERATION_PROGRESS, VfsRegistryManager};
use newt_common::file_reader::FileReader;
use newt_common::filesystem::{
    FileList, Filesystem, LocalShellService, PendingStreams, ShellRemote, ShellService, StreamId,
};
use newt_common::operation::{OperationContext, OperationProgress, OperationsClient};
use newt_common::rpc::Communicator;
use newt_common::terminal::TerminalClient;
use newt_common::vfs::{
    LOCAL_VFS_DESCRIPTOR, LocalVfs, MountedVfsInfo, VfsId, VfsManager, VfsManagerRemote, VfsPath,
    VfsRegistry, VfsRegistryFileReader, VfsRegistryFs,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::common::{Error, UpdatePublisher};

use super::{MainWindowState, Operations, apply_operation_progress};

/// On Linux, arrange for the child to receive SIGTERM when the parent exits.
/// This ensures SSH/agent processes don't linger if Newt is killed.
/// On other platforms this is a no-op.
fn set_parent_death_signal(cmd: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_PDEATHSIG) is async-signal-safe and this is
        // the only thing we do in the pre_exec closure.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = cmd;
    }
}

/// Callback invoked when SSH needs user input (password, passphrase, host key
/// confirmation). Returns `Some(response)` on success, `None` if the user
/// cancelled.
pub type AskpassCallback = Box<
    dyn Fn(String, bool) -> Pin<Box<dyn Future<Output = Option<String>> + Send>>
        + Send
        + Sync
        + 'static,
>;

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
    Connecting { message: String, log: Vec<String> },
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
            log: Vec::new(),
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
    _event_loop_handle: tokio::task::JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self._file_server_handle.abort();
        self._event_loop_handle.abort();
    }
}

// ---------------------------------------------------------------------------
// VfsInfoService implementation
// ---------------------------------------------------------------------------

struct MountedVfsInfoService {
    mounted_vfs: Arc<RwLock<HashMap<VfsId, MountedVfsInfo>>>,
}

impl super::pane::VfsInfoService for MountedVfsInfoService {
    fn descriptor(
        &self,
        vfs_id: VfsId,
    ) -> Option<(&'static dyn newt_common::vfs::VfsDescriptor, Vec<u8>)> {
        self.mounted_vfs
            .read()
            .get(&vfs_id)
            .map(|info| (info.descriptor, info.mount_meta.clone()))
    }

    fn origin(&self, vfs_id: VfsId) -> Option<VfsPath> {
        self.mounted_vfs
            .read()
            .get(&vfs_id)
            .and_then(|info| info.origin.clone())
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
        let mut status = self.0.write();
        let existing_log = match &*status {
            ConnectionStatus::Connecting { log, .. } => log.clone(),
            _ => Vec::new(),
        };
        *status = ConnectionStatus::Connecting {
            message: message.to_string(),
            log: existing_log,
        };
    }

    pub fn set_failed(&self, error: String) {
        let mut status = self.0.write();
        let existing_log = match &*status {
            ConnectionStatus::Connecting { log, .. } => log.clone(),
            _ => Vec::new(),
        };
        *status = ConnectionStatus::Failed {
            log: existing_log,
            error,
        };
    }

    /// Append a line to the connection log.
    fn append_log(&self, line: String) {
        let mut status = self.0.write();
        match &mut *status {
            ConnectionStatus::Connecting { log, .. } | ConnectionStatus::Connected { log } => {
                log.push(line);
            }
            _ => {}
        }
    }
}

/// Helper that pushes a line to both the stderr log and the connection status,
/// then publishes so the frontend sees it immediately.
struct ConnectionLog {
    stderr_log: StderrLog,
    connection_status: ConnectionState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
}

impl ConnectionLog {
    fn log(&self, line: impl Into<String>) {
        let line = line.into();
        self.stderr_log.push(line.clone());
        self.connection_status.append_log(line);
        let _ = self.publisher.publish();
    }
}

/// Spawn a task that reads lines from `stderr` and appends them to `log`.
/// Publishes after each line so the frontend can see logs in real-time.
fn spawn_stderr_reader(stderr: tokio::process::ChildStderr, conn_log: ConnectionLog) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    conn_log.log(line.trim_end());
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
    stderr: Option<tokio::process::ChildStderr>,
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

/// Spawn an askpass listener on a temporary Unix domain socket. Each askpass
/// invocation by SSH connects, sends two lines (prompt type + prompt text),
/// and reads one line (response). Returns the socket path for the env var.
fn spawn_askpass_listener(askpass_callback: Arc<AskpassCallback>) -> Result<PathBuf, Error> {
    let sock_path = std::env::temp_dir().join(format!("newt-askpass-{}.sock", std::process::id()));

    // Clean up any stale socket
    let _ = std::fs::remove_file(&sock_path);

    let listener = std::os::unix::net::UnixListener::bind(&sock_path)?;
    listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(listener)?;

    let cleanup_path = sock_path.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => break,
            };

            let askpass_callback = askpass_callback.clone();
            tokio::spawn(async move {
                use newt_common::askpass::{self, AskpassResponse, PromptType};

                let (mut reader, mut writer) = stream.into_split();

                let request: askpass::AskpassRequest =
                    match askpass::tokio::read_msg(&mut reader).await {
                        Ok(r) => r,
                        Err(_) => return,
                    };

                // When SSH_ASKPASS_PROMPT is unset (mapped to Secret), fall
                // back to prompt text heuristics for host-key confirmations.
                // OpenSSH doesn't set SSH_ASKPASS_PROMPT for host key prompts.
                let is_secret = match request.prompt_type {
                    PromptType::Confirm | PromptType::Info => false,
                    PromptType::Secret => !request.prompt.contains("(yes/no/[fingerprint])"),
                };

                let response = askpass_callback(request.prompt, is_secret).await;
                let _ = askpass::tokio::write_msg(&mut writer, &AskpassResponse(response)).await;
                // If None (cancelled), just drop the connection — askpass gets EOF
            });
        }

        let _ = std::fs::remove_file(&cleanup_path);
    });

    Ok(sock_path)
}

/// Spawn SSH + bootstrap script, negotiate agent upload if needed.
async fn spawn_remote(
    transport_cmd: &[String],
    agent_resolver: &AgentResolver,
    askpass_callback: Arc<AskpassCallback>,
    conn_log: &ConnectionLog,
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

    // Set up askpass via a named Unix domain socket
    let askpass_binary = agent_resolver.find_local_agent_binary()?;
    let askpass_sock = spawn_askpass_listener(askpass_callback)?;

    conn_log.log(format!("Spawning: {} {}", program, args.join(" ")));

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .arg(&sh_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("SSH_ASKPASS", &askpass_binary)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("NEWT_ASKPASS_SOCK", &askpass_sock)
        .kill_on_drop(true);
    set_parent_death_signal(&mut cmd);
    let mut child = cmd.spawn()?;

    conn_log.log("Process spawned, waiting for bootstrap...");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Start reading stderr immediately so SSH logs appear during connection
    spawn_stderr_reader(
        stderr,
        ConnectionLog {
            stderr_log: conn_log.stderr_log.clone(),
            connection_status: conn_log.connection_status.clone(),
            publisher: conn_log.publisher.clone(),
        },
    );

    // Read status line, skipping any noise from .bashrc etc.
    let mut reader = BufReader::new(stdout);
    let status_line = loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // Connection closed — use stderr log for context (already being
            // captured by the stderr reader task).
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let lines = conn_log.stderr_log.lines();
            let detail = lines.last().map(|l| format!(": {}", l)).unwrap_or_default();
            return Err(Error::Custom(format!(
                "remote connection closed before bootstrap completed{}",
                detail
            )));
        }
        let trimmed = line.trim();
        if trimmed.starts_with("NEWT:") {
            break trimmed.to_string();
        }
        conn_log.log(format!("bootstrap: {}", trimmed));
    };
    let status_line = status_line.as_str();

    if status_line == "NEWT:READY" {
        conn_log.log("Agent ready");
        Ok(ChildConnection {
            stream: make_stream(reader, stdin),
            child,
            stderr: None,
        })
    } else if let Some(need_rest) = status_line.strip_prefix("NEWT:NEED:") {
        // Format: NEWT:NEED:<triple>:<caps> where caps is comma-separated
        let (triple, caps_str) = need_rest.split_once(':').unwrap_or((need_rest, ""));
        let caps: Vec<&str> = caps_str.split(',').filter(|s| !s.is_empty()).collect();
        let has_gzip = caps.contains(&"gzip");

        conn_log.log(format!(
            "Agent needs upload for {} (caps: {})",
            triple,
            if caps.is_empty() { "none" } else { caps_str }
        ));
        let binary_path = agent_resolver.find_agent_binary(triple)?;
        let binary_data = tokio::fs::read(&binary_path).await?;
        let raw_size = binary_data.len();

        let (upload_data, encoding) = if has_gzip {
            use flate2::Compression;
            use flate2::write::GzEncoder;
            use std::io::Write;

            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&binary_data)?;
            let compressed = encoder.finish()?;
            conn_log.log(format!(
                "Compressed {} → {} bytes ({:.0}%)",
                raw_size,
                compressed.len(),
                compressed.len() as f64 / raw_size as f64 * 100.0
            ));
            (compressed, "gzip")
        } else {
            (binary_data, "raw")
        };

        conn_log.log(format!(
            "Uploading agent ({} bytes, {})...",
            upload_data.len(),
            encoding
        ));
        stdin
            .write_all(format!("{} {}\n", upload_data.len(), encoding).as_bytes())
            .await?;
        stdin.write_all(&upload_data).await?;
        stdin.flush().await?;
        conn_log.log("Agent uploaded");

        Ok(ChildConnection {
            stream: make_stream(reader, stdin),
            child,
            stderr: None,
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
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    set_parent_death_signal(&mut cmd);
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
        stderr: Some(stderr),
    })
}

/// Spawn a background task that waits for the child to exit, then clears the
/// session and sets the connection status to `Disconnected`.
fn spawn_child_watcher(
    mut child: tokio::process::Child,
    stderr: Option<tokio::process::ChildStderr>,
    stderr_log: StderrLog,
    session_slot: Arc<arc_swap::ArcSwap<Option<Session>>>,
    connection_status: ConnectionState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
) {
    // Start capturing stderr if not already being read
    if let Some(stderr) = stderr {
        spawn_stderr_reader(
            stderr,
            ConnectionLog {
                stderr_log: stderr_log.clone(),
                connection_status: connection_status.clone(),
                publisher: publisher.clone(),
            },
        );
    }

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
#[allow(clippy::too_many_arguments)]
pub(super) async fn connect(
    connection_target: &ConnectionTarget,
    agent_resolver: &AgentResolver,
    state: &MainWindowState,
    publisher: &Arc<UpdatePublisher<MainWindowState>>,
    preferences: crate::preferences::PreferencesHandle,
    session_slot: &Arc<arc_swap::ArcSwap<Option<Session>>>,
    set_status: impl Fn(&str),
    askpass_callback: impl Fn(String, bool) -> Pin<Box<dyn Future<Output = Option<String>> + Send>>
    + Send
    + Sync
    + 'static,
    main_window_ctx: super::MainWindowContext,
) -> Result<(), Error> {
    let askpass_callback: Arc<AskpassCallback> = Arc::new(Box::new(askpass_callback));
    let (services, stderr_log, child) = match connection_target {
        ConnectionTarget::Local => {
            let services = create_local_services(&state.operations, publisher);
            (services, StderrLog::default(), None)
        }
        ConnectionTarget::Remote { transport_cmd } => {
            set_status("Connecting to remote host...");
            let stderr_log = StderrLog::default();
            let conn_log = ConnectionLog {
                stderr_log: stderr_log.clone(),
                connection_status: state.connection_status.clone(),
                publisher: publisher.clone(),
            };
            let conn = spawn_remote(
                transport_cmd,
                agent_resolver,
                askpass_callback.clone(),
                &conn_log,
            )
            .await?;
            conn_log.log("Setting up RPC services...");
            let (services, _) = create_rpc_services(conn.stream, &state.operations, publisher);
            conn_log.log("Connected");
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
            .map(VfsPath::root)
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
            origin: None,
        },
    );
    let mounted_vfs = Arc::new(RwLock::new(initial_mounted));

    let vfs_info: Arc<dyn super::pane::VfsInfoService> = Arc::new(MountedVfsInfoService {
        mounted_vfs: mounted_vfs.clone(),
    });

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

    state.panes.add(super::pane::Pane::new(
        services.fs.clone(),
        initial_dir.clone(),
        state.display_options.clone(),
        preferences.clone(),
        publisher.clone(),
        vfs_info.clone(),
        Some(event_tx.clone()),
    ));
    state.panes.add(super::pane::Pane::new(
        services.fs.clone(),
        initial_dir,
        state.display_options.clone(),
        preferences,
        publisher.clone(),
        vfs_info,
        Some(event_tx.clone()),
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

    let event_loop_handle = tokio::spawn({
        let ctx = main_window_ctx;
        async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    super::MainWindowEvent::PaneNavigated => {
                        if let Err(e) = ctx.cleanup_stale_archive_mounts().await {
                            log::warn!("failed to cleanup stale archive mounts: {}", e);
                        }
                    }
                }
            }
        }
    });

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
        _event_loop_handle: event_loop_handle,
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
