use newt_common::api::{API_LIST_FILES_BATCH, API_OPERATION_PROGRESS, VfsRegistryManager};
use newt_common::file_reader::FileReader;
use newt_common::filesystem::{
    FileList, Filesystem, LocalShellService, PendingStreams, ShellRemote, ShellService, StreamId,
};
use newt_common::operation::{OperationContext, OperationProgress, OperationsClient};
use newt_common::rpc::Communicator;
use newt_common::terminal::TerminalClient;
use newt_common::vfs::{
    LOCAL_VFS_DESCRIPTOR, LocalVfs, MountedVfsInfo, VfsDescriptor, VfsId, VfsManager,
    VfsManagerRemote, VfsPath, VfsRegistry, VfsRegistryFileReader, VfsRegistryFs,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
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

use newt_common::askpass::AskpassProvider;

// ---------------------------------------------------------------------------
// ConnectionTarget
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum ConnectionTarget {
    /// No subprocess: services run directly in the Tauri process.
    Local,
    /// pkexec-elevated agent on the local machine (Linux only).
    Elevated,
    /// Spawn an agent over an external transport.
    Spawn(SpawnSpec),
}

#[derive(Clone, Debug)]
pub enum SpawnSpec {
    /// Run `transport_cmd` and append the sh-based `bootstrap.sh` script as its
    /// final argument. The bootstrap negotiates arch detection, agent caching,
    /// and upload-if-missing. Requires `sh` + a handful of coreutils on the
    /// target side; in exchange we get a hash-keyed agent cache.
    Bootstrap {
        transport_cmd: Vec<String>,
        /// Human-readable label, used in log lines and the connection log.
        label: String,
        /// Whether this transport supports interactive prompts via SSH_ASKPASS.
        /// `true` for `ssh` so passwords / passphrases can be forwarded;
        /// `false` for daemon-mediated transports (docker / kubectl etc.) where
        /// SSH_ASKPASS is a no-op.
        askpass: bool,
        /// `true` if the transport joins its trailing argv elements into a
        /// single shell command on the far side (this is what `ssh` does, and
        /// it requires us to shell-quote the bootstrap into one argv element).
        /// `false` for transports that `execvp` their args directly
        /// (`docker exec`, `podman exec`, `kubectl exec`, custom), where we
        /// must pass `sh`, `-c`, `<script>` as three separate argv elements.
        shell_join: bool,
    },
    /// Out-of-band copy: detect the target's architecture, `cp` the agent
    /// binary in, then exec it directly. No shell on the target side required.
    /// Re-uploads on every connect (no cache).
    DirectCopy(DirectCopyPlan),
    /// User-supplied shell command run locally. The bootstrap script is exposed
    /// via the `NEWT_BOOTSTRAP` env var; the user references it from inside
    /// their command (`ssh host "$NEWT_BOOTSTRAP"`, `bash -c "$NEWT_BOOTSTRAP"`,
    /// etc.). Gives the most control at the cost of needing the user to write
    /// the splice point themselves.
    CustomShell {
        command: String,
        label: String,
        /// If true, do not run the bootstrap handshake — assume the command
        /// produces a running agent on stdin/stdout itself. Default false.
        skip_bootstrap: bool,
    },
}

/// Recipe for a bootstrapless launch. Each command is a fully-resolved argv
/// except for the `{local}` / `{remote}` / `{agent_path}` placeholders, which
/// are substituted at spawn time. Keeping these as templates lets us share one
/// `spawn_direct_copy` implementation across `docker` / `podman`.
#[derive(Clone, Debug)]
pub struct DirectCopyPlan {
    /// Ordered list of arch-detection commands. The trimmed stdout of each
    /// step is interpolated as `{prev}` into the next; the last step's stdout
    /// must be a `"<OS>/<Arch>"` line.
    ///
    /// (Docker / Podman need two steps — `inspect` the container to get the
    /// image ID, then `image inspect` to get OS/Arch. The container itself
    /// doesn't expose `.Os` / `.Architecture` in its template namespace.)
    pub arch_detect_pipeline: Vec<Vec<String>>,
    /// Substitute `{local}` (host-side path to agent) and `{remote}`
    /// (target-side destination path).
    pub copy_cmd: Vec<String>,
    /// Substitute `{agent_path}` (target-side path). Stdin/stdout become the
    /// RPC channel; stderr is logged. Must produce a bidirectional pipe.
    pub exec_cmd: Vec<String>,
    pub label: String,
}

/// Build a `transport_cmd` for an ssh-based remote session, with
/// application-level keepalive enabled so that idle TCP connections aren't
/// silently killed by NAT / firewalls / load balancers. When `forward_agent`
/// is true, also adds `-A` so SSH agent forwarding is enabled.
pub fn ssh_transport_cmd(host: &str, forward_agent: bool) -> Vec<String> {
    let mut v = vec![
        "ssh".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
        "-o".to_string(),
        "ServerAliveCountMax=3".to_string(),
    ];
    if forward_agent {
        v.push("-A".to_string());
    }
    v.push(host.to_string());
    v
}

/// `docker exec -i [-u <user>] <container>` — the bootstrap script is appended
/// as the final argv element by the caller.
pub fn docker_transport_cmd(container: &str, user: Option<&str>) -> Vec<String> {
    let mut v = vec!["docker".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(u) = user {
        v.push("-u".to_string());
        v.push(u.to_string());
    }
    v.push(container.to_string());
    v
}

/// `podman exec -i [-u <user>] <container>`.
pub fn podman_transport_cmd(container: &str, user: Option<&str>) -> Vec<String> {
    let mut v = vec!["podman".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(u) = user {
        v.push("-u".to_string());
        v.push(u.to_string());
    }
    v.push(container.to_string());
    v
}

/// `kubectl exec -i [-c=…] <pod> [--context=…] [-n=…] --`.
///
/// Global flags (`--context`, `--namespace`) follow the `exec` subcommand
/// rather than precede `kubectl`, because some kubectl wrappers (notably
/// orbstack) reject flags before the plugin name.
pub fn kube_transport_cmd(
    context: Option<&str>,
    namespace: Option<&str>,
    pod: &str,
    container: Option<&str>,
) -> Vec<String> {
    let mut v = vec!["kubectl".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(c) = container {
        v.push(format!("--container={}", c));
    }
    v.push(pod.to_string());
    if let Some(c) = context {
        v.push(format!("--context={}", c));
    }
    if let Some(n) = namespace {
        v.push(format!("--namespace={}", n));
    }
    v.push("--".to_string());
    v
}

/// Direct-copy plan for `docker`. `docker inspect` reports OS and architecture
/// from the image's manifest, which is exactly what we need — no shell in the
/// container required.
pub fn docker_direct_copy_plan(container: &str, user: Option<&str>) -> DirectCopyPlan {
    direct_copy_plan_for("docker", container, user)
}

/// Same shape as `docker_direct_copy_plan`. `podman cp` / `podman inspect` /
/// `podman exec` are CLI-compatible with their docker counterparts.
pub fn podman_direct_copy_plan(container: &str, user: Option<&str>) -> DirectCopyPlan {
    direct_copy_plan_for("podman", container, user)
}

fn direct_copy_plan_for(program: &str, container: &str, user: Option<&str>) -> DirectCopyPlan {
    // Step 1: container → image ID. Step 2: image ID → "<Os>/<Architecture>".
    // Container JSON doesn't expose Os/Architecture; the underlying image does.
    let arch_detect_pipeline = vec![
        vec![
            program.to_string(),
            "inspect".to_string(),
            "--format={{.Image}}".to_string(),
            container.to_string(),
        ],
        vec![
            program.to_string(),
            "image".to_string(),
            "inspect".to_string(),
            "--format={{.Os}}/{{.Architecture}}".to_string(),
            "{prev}".to_string(),
        ],
    ];
    let copy_cmd = vec![
        program.to_string(),
        "cp".to_string(),
        "{local}".to_string(),
        format!("{}:{{remote}}", container),
    ];
    let mut exec_cmd = vec![program.to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(u) = user {
        exec_cmd.push("-u".to_string());
        exec_cmd.push(u.to_string());
    }
    exec_cmd.push(container.to_string());
    exec_cmd.push("{agent_path}".to_string());
    DirectCopyPlan {
        arch_detect_pipeline,
        copy_cmd,
        exec_cmd,
        label: format!("{}:{}", program, container),
    }
}

// ---------------------------------------------------------------------------
// ConnectionStatus (serialized to the frontend via MainWindowState)
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize, specta::Type)]
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

pub use newt_common::agent_resolver::AgentResolver;
use newt_common::agent_resolver::local_agent_triple;

/// Host-side resolver. Searches directories in priority order:
/// 1. `NEWT_AGENT_DIR` env var (runtime dev override)
/// 2. `NEWT_SYSTEM_AGENT_DIR` compile-time path (distro packages)
/// 3. Tauri resource dir (`agents/` inside the bundled app)
/// 4. `agents/` relative fallback (legacy/dev)
pub struct TauriAgentResolver {
    dirs: Vec<PathBuf>,
}

impl TauriAgentResolver {
    pub fn new(app_handle: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        let mut dirs = Vec::new();

        if let Ok(dir) = std::env::var("NEWT_AGENT_DIR") {
            dirs.push(PathBuf::from(dir));
        }

        if let Some(dir) = option_env!("NEWT_SYSTEM_AGENT_DIR") {
            dirs.push(PathBuf::from(dir));
        }

        if let Ok(resource_dir) = app_handle.path().resource_dir() {
            dirs.push(resource_dir.join("agents"));
        }

        dirs.push(PathBuf::from("agents"));

        Self { dirs }
    }
}

impl AgentResolver for TauriAgentResolver {
    /// Compute a hash that changes whenever any agent binary changes.
    fn agent_hash(&self) -> Result<String, newt_common::Error> {
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
            return Err(newt_common::Error::custom(
                "no agent binaries found to compute hash",
            ));
        }

        Ok(hasher.finalize().to_hex()[..16].to_string())
    }

    /// Look up the agent binary for a given target triple.
    fn find_agent_binary(&self, triple: &str) -> Result<PathBuf, newt_common::Error> {
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

        Err(newt_common::Error::custom(format!(
            "agent binary not found for triple: {}. Set NEWT_AGENT_DIR to the directory containing the agent binary.",
            triple
        )))
    }

    /// Find the agent binary on the local machine (for elevated mode).
    /// Maps the compile-time target to the agent triple (always musl on Linux).
    fn find_local_agent_binary(&self) -> Result<PathBuf, newt_common::Error> {
        let triple = local_agent_triple();
        self.find_agent_binary(&triple)
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

pub struct Session {
    pub(super) fs: Arc<dyn Filesystem>,
    pub(super) shell_service: Arc<dyn ShellService>,
    pub(super) vfs_manager: Arc<dyn VfsManager>,
    pub(super) vfs_info: Arc<dyn VfsInfo>,
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

pub trait VfsInfo: Send + Sync {
    fn descriptor(&self, vfs_id: VfsId) -> Option<(&'static dyn VfsDescriptor, Vec<u8>)>;
    fn origin(&self, vfs_id: VfsId) -> Option<VfsPath>;
    /// Whether the given VFS is backed by the host machine's local filesystem.
    fn is_host_local(&self, vfs_id: VfsId) -> bool;
    fn display_name(&self, vfs_id: VfsId) -> Option<&str>;
    /// Returns the VFS ID of a filesystem that is local to the host machine
    /// (the machine running the Tauri process), or `None` if no such VFS is mounted.
    fn host_local_vfs_id(&self) -> Option<VfsId>;

    /// Resolve a `VfsPath` to a directory on the terminal's filesystem
    /// (`VfsId::ROOT` — the local FS in local mode, the agent's FS in
    /// remote/elevated mode), suitable for use as a child-process cwd.
    ///
    /// For paths already on `VfsId::ROOT` this returns the path unchanged.
    /// For VFSes that have an origin (today: archives), it walks to the
    /// enclosing directory of the origin file and recurses. For VFSes with
    /// no origin (S3, SFTP, Kubernetes, Remote) this returns `None`, so
    /// callers can fall back to the spawning process's inherited cwd.
    fn resolve_terminal_cwd(&self, path: &VfsPath) -> Option<PathBuf> {
        let mut current = path.clone();
        loop {
            if current.vfs_id == VfsId::ROOT {
                return Some(current.path);
            }
            let origin = self.origin(current.vfs_id)?;
            let parent = origin.path.parent()?.to_path_buf();
            current = VfsPath::new(origin.vfs_id, parent);
        }
    }
}

struct MountedVfsInfoService {
    mounted_vfs: Arc<RwLock<HashMap<VfsId, MountedVfsInfo>>>,
    remote_session: bool,
}

impl VfsInfo for MountedVfsInfoService {
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

    fn is_host_local(&self, vfs_id: VfsId) -> bool {
        match self
            .mounted_vfs
            .read()
            .get(&vfs_id)
            .map(|info| info.descriptor.type_name())
        {
            Some("local") => !self.remote_session, // Local VFS is host-local in remote sessions, but not in local sessions
            Some("remote") => self.remote_session, // Remote VFS is host-local in remote sessions, but not in local sessions
            _ => false,
        }
    }

    fn display_name(&self, vfs_id: VfsId) -> Option<&str> {
        let descriptor = self
            .mounted_vfs
            .read()
            .get(&vfs_id)
            .map(|info| info.descriptor)?;

        Some(match descriptor.type_name() {
            "local" if !self.remote_session => "Local",
            "local" if self.remote_session => "Remote",
            "remote" if self.remote_session => "Local",
            "remote" if !self.remote_session => "Remote",
            _ => descriptor.display_name(), // For custom VFS types, just show the type name
        })
    }

    fn host_local_vfs_id(&self) -> Option<VfsId> {
        let mounted = self.mounted_vfs.read();
        mounted.keys().copied().find(|id| self.is_host_local(*id))
    }
}

// ---------------------------------------------------------------------------
// Hairpin diversion wrappers — for remote sessions, divert certain calls for
// the RemoteVfs (client-local filesystem) to a local VfsRegistryFs/FileReader
// instead of round-tripping through the agent.
// ---------------------------------------------------------------------------

struct HairpinFs {
    remote_vfs_id: VfsId,
    local_fs: VfsRegistryFs,
    inner: Arc<dyn Filesystem>,
}

#[async_trait::async_trait]
impl Filesystem for HairpinFs {
    async fn poll_changes(&self, path: VfsPath) -> Result<(), newt_common::Error> {
        if path.vfs_id == self.remote_vfs_id {
            self.local_fs
                .poll_changes(VfsPath::new(VfsId::ROOT, path.path))
                .await
        } else {
            self.inner.poll_changes(path).await
        }
    }

    async fn list_files(
        &self,
        path: VfsPath,
        options: newt_common::filesystem::ListFilesOptions,
        batch_tx: Option<tokio::sync::mpsc::Sender<FileList>>,
    ) -> Result<FileList, newt_common::Error> {
        if path.vfs_id == self.remote_vfs_id {
            // Wrap the batch channel to rewrite VFS IDs before forwarding
            let rewriting_tx = batch_tx.map(|outer_tx| {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<FileList>(
                    newt_common::filesystem::LIST_BATCH_CHANNEL_CAPACITY,
                );
                let vfs_id = self.remote_vfs_id;
                tokio::spawn(async move {
                    while let Some(mut batch) = rx.recv().await {
                        batch.rewrite_vfs_id(vfs_id);
                        if outer_tx.send(batch).await.is_err() {
                            break;
                        }
                    }
                });
                tx
            });

            let mut result = self
                .local_fs
                .list_files(VfsPath::new(VfsId::ROOT, path.path), options, rewriting_tx)
                .await?;
            result.rewrite_vfs_id(self.remote_vfs_id);
            Ok(result)
        } else {
            self.inner.list_files(path, options, batch_tx).await
        }
    }

    async fn rename(&self, old_path: VfsPath, new_path: VfsPath) -> Result<(), newt_common::Error> {
        self.inner.rename(old_path, new_path).await
    }

    async fn touch(&self, path: VfsPath) -> Result<(), newt_common::Error> {
        self.inner.touch(path).await
    }

    async fn create_directory(&self, path: VfsPath) -> Result<(), newt_common::Error> {
        self.inner.create_directory(path).await
    }

    async fn revalidate(
        &self,
        vfs_id: VfsId,
    ) -> Result<newt_common::vfs::RevalidationOutcome, newt_common::Error> {
        if vfs_id == self.remote_vfs_id {
            // The hairpin VFS is a hand-back of the host's local FS to
            // the remote agent — local FS doesn't cache external state,
            // so revalidation is trivially fresh.
            Ok(newt_common::vfs::RevalidationOutcome::Fresh)
        } else {
            self.inner.revalidate(vfs_id).await
        }
    }
}

struct HairpinFileReader {
    remote_vfs_id: VfsId,
    local_reader: VfsRegistryFileReader,
    inner: Arc<dyn FileReader>,
}

#[async_trait::async_trait]
impl FileReader for HairpinFileReader {
    async fn file_details(
        &self,
        path: VfsPath,
    ) -> Result<newt_common::file_reader::FileDetails, newt_common::Error> {
        self.inner.file_details(path).await
    }

    async fn read_range(
        &self,
        path: VfsPath,
        offset: u64,
        length: u64,
    ) -> Result<newt_common::file_reader::FileChunk, newt_common::Error> {
        if path.vfs_id == self.remote_vfs_id {
            self.local_reader
                .read_range(VfsPath::new(VfsId::ROOT, path.path), offset, length)
                .await
        } else {
            self.inner.read_range(path, offset, length).await
        }
    }

    async fn read_file(&self, path: VfsPath, max_size: u64) -> Result<Vec<u8>, newt_common::Error> {
        if path.vfs_id == self.remote_vfs_id {
            self.local_reader
                .read_file(VfsPath::new(VfsId::ROOT, path.path), max_size)
                .await
        } else {
            self.inner.read_file(path, max_size).await
        }
    }

    async fn write_file(&self, path: VfsPath, data: Vec<u8>) -> Result<(), newt_common::Error> {
        if path.vfs_id == self.remote_vfs_id {
            self.local_reader
                .write_file(VfsPath::new(VfsId::ROOT, path.path), data)
                .await
        } else {
            self.inner.write_file(path, data).await
        }
    }

    async fn find_in_file(
        &self,
        path: VfsPath,
        offset: u64,
        pattern: newt_common::file_reader::SearchPattern,
        max_length: u64,
    ) -> Result<Option<newt_common::file_reader::SearchMatch>, newt_common::Error> {
        if path.vfs_id == self.remote_vfs_id {
            self.local_reader
                .find_in_file(
                    VfsPath::new(VfsId::ROOT, path.path),
                    offset,
                    pattern,
                    max_length,
                )
                .await
        } else {
            self.inner
                .find_in_file(path, offset, pattern, max_length)
                .await
        }
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
    preferences: crate::preferences::PreferencesHandle,
    /// Shared sink used to apply incoming VFS progress notifications
    /// from the agent into `MainWindowState.vfs_progress`. Same impl
    /// the local-mode `VfsRegistryManager` writes to, so the consumer
    /// side is identical regardless of session mode.
    progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink>,
}

/// Dispatches `API_HOST_ASKPASS` from the agent to an `AskpassProvider`.
/// Used so an agent-side ssh process (e.g. a VFS-mounted SFTP) can prompt
/// the user via the same dialog the host's main SSH transport uses.
struct HostAskpassDispatcher {
    provider: Arc<dyn AskpassProvider>,
}

#[async_trait::async_trait]
impl newt_common::rpc::Dispatcher for HostAskpassDispatcher {
    async fn invoke(
        &self,
        api: newt_common::rpc::Api,
        req: bytes::Bytes,
    ) -> Result<Option<bytes::Bytes>, newt_common::Error> {
        if api == newt_common::api::API_HOST_ASKPASS {
            let request: newt_common::askpass::AskpassRequest = bincode::deserialize(&req[..])
                .map_err(|e| newt_common::Error::custom(e.to_string()))?;
            let response = self.provider.prompt(request).await;
            let bytes = bincode::serialize(&response)
                .map_err(|e| newt_common::Error::custom(e.to_string()))?;
            Ok(Some(bytes::Bytes::from(bytes)))
        } else {
            Ok(None)
        }
    }

    async fn notify(
        &self,
        _api: newt_common::rpc::Api,
        _req: bytes::Bytes,
    ) -> Result<bool, newt_common::Error> {
        Ok(false)
    }
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
            let keep = self.preferences.load().behavior.keep_finished_operations;
            apply_operation_progress(&self.operations, progress, keep);
            let _ = self.publisher.publish();
            Ok(true)
        } else if api == newt_common::api::API_VFS_PROGRESS {
            let (vfs_id, progress): (
                newt_common::vfs::VfsId,
                Option<newt_common::vfs::VfsProgress>,
            ) = bincode::deserialize(&req[..]).unwrap();
            // Sink does both the state update and the publish.
            self.progress_sink.report(vfs_id, progress);
            Ok(true)
        } else if api == API_LIST_FILES_BATCH {
            let (stream_id, file_list): (StreamId, FileList) =
                bincode::deserialize(&req[..]).unwrap();
            let tx = self.pending_streams.lock().get(&stream_id).cloned();
            if let Some(tx) = tx {
                let _ = tx.send(file_list).await;
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
    preferences: &crate::preferences::PreferencesHandle,
    sftp_askpass: Option<newt_common::api::SftpAskpass>,
    askpass_provider: Arc<dyn AskpassProvider>,
    progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink>,
) -> Services {
    let (progress_tx, mut progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<OperationProgress>();

    let registry = Arc::new(VfsRegistry::with_root(Arc::new(LocalVfs::new())));
    let op_context = Arc::new(OperationContext {
        registry: registry.clone(),
    });

    let operations = operations.clone();
    let publisher_clone = publisher.clone();
    let preferences = preferences.clone();
    tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            let keep = preferences.load().behavior.keep_finished_operations;
            apply_operation_progress(&operations, progress, keep);
            let _ = publisher_clone.publish();
        }
    });

    Services {
        fs: Arc::new(VfsRegistryFs::new(registry.clone())),
        shell_service: Arc::new(LocalShellService),
        vfs_manager: Arc::new({
            let mgr = VfsRegistryManager::new(registry.clone())
                .with_askpass_provider(askpass_provider)
                .with_progress_sink(progress_sink);
            if let Some(askpass) = sftp_askpass {
                mgr.with_sftp_askpass(askpass)
            } else {
                mgr
            }
        }),
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
    preferences: &crate::preferences::PreferencesHandle,
    expose_local_fs: bool,
    askpass_provider: Arc<dyn AskpassProvider>,
    progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink>,
) -> (Services, PendingStreams) {
    use newt_common::rpc::DispatcherExt;

    let pending_streams: PendingStreams = Arc::new(parking_lot::Mutex::new(HashMap::new()));

    let host_dispatcher = HostDispatcher {
        operations: operations.clone(),
        publisher: publisher.clone(),
        pending_streams: pending_streams.clone(),
        preferences: preferences.clone(),
        progress_sink,
    };
    let askpass_dispatcher = HostAskpassDispatcher {
        provider: askpass_provider,
    };
    let (outbox, inbox) = Communicator::create_outbox();
    let base = host_dispatcher.chain(askpass_dispatcher);
    let communicator = if expose_local_fs {
        use newt_common::api::VfsDispatcher;
        use newt_common::vfs::LocalVfs;
        let host_vfs = Arc::new(LocalVfs::new());
        let combined_dispatcher = base.chain(VfsDispatcher::new(host_vfs, outbox.clone()));
        Communicator::with_dispatcher_and_outbox(combined_dispatcher, stream, outbox, inbox)
    } else {
        Communicator::with_dispatcher_and_outbox(base, stream, outbox, inbox)
    };
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
    /// Askpass listener tied to the ssh process; dropped when the connection
    /// tears down. `None` for elevated mode (no askpass).
    _askpass: Option<newt_common::askpass::listener::AskpassListener>,
}

fn make_stream(
    reader: BufReader<tokio::process::ChildStdout>,
    stdin: tokio::process::ChildStdin,
) -> DynStream {
    let rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(reader);
    let tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
    tokio_duplex::Duplex::new(rx, tx)
}

/// Spawn a bootstrap-style transport (SSH / docker exec / kubectl exec / …),
/// pipe the embedded `bootstrap.sh` into it, and negotiate agent upload.
/// `enable_askpass` wires up SSH_ASKPASS for transports that may prompt for a
/// password (SSH); daemon-mediated transports (docker / kubectl etc.) skip it.
async fn spawn_bootstrap(
    transport_cmd: &[String],
    enable_askpass: bool,
    shell_join: bool,
    extra_path: &[String],
    agent_resolver: &dyn AgentResolver,
    askpass_provider: Arc<dyn AskpassProvider>,
    conn_log: &ConnectionLog,
) -> Result<ChildConnection, Error> {
    let (program, args) = transport_cmd
        .split_first()
        .ok_or_else(|| Error::Custom("empty transport command".into()))?;
    let program = crate::path_resolver::resolve_program(program, extra_path);

    // The script reads `NEWT_RUST_LOG` from its own environment. Inject the
    // assignment as the first line of the script body so it survives transport
    // boundaries that don't propagate env vars (e.g. `docker exec` without `-e`).
    let mut script_body = BOOTSTRAP_SCRIPT.replace("__NEWT_HASH__", &agent_resolver.agent_hash()?);
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        let escaped_val = rust_log.replace('\'', "'\\''");
        script_body = format!("NEWT_RUST_LOG='{}'\n{}", escaped_val, script_body);
    }

    let askpass_listener = if enable_askpass {
        Some(newt_common::askpass::listener::spawn(askpass_provider)?)
    } else {
        None
    };
    let askpass_binary = if enable_askpass {
        Some(agent_resolver.find_local_agent_binary()?)
    } else {
        None
    };

    conn_log.log(format!(
        "Spawning: {} {}",
        program.display(),
        args.join(" ")
    ));

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if shell_join {
        // SSH joins its trailing argv with spaces and re-runs the result inside
        // a shell on the remote. Quote everything into one argv element so the
        // remote sees `sh -c '<script>'`.
        let escaped = script_body.replace('\'', "'\\''");
        cmd.arg(format!("sh -c '{}'", escaped));
    } else {
        // `docker exec` / `podman exec` / `kubectl exec` / custom transports
        // `execvp` their argv directly. Pass `sh`, `-c`, `<script>` as three
        // separate elements.
        cmd.arg("sh").arg("-c").arg(&script_body);
    }
    if let (Some(askpass_binary), Some(listener)) = (&askpass_binary, &askpass_listener) {
        cmd.env("SSH_ASKPASS", askpass_binary)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("NEWT_ASKPASS_SOCK", &listener.socket_path);
    }
    set_parent_death_signal(&mut cmd);
    let child = cmd.spawn()?;
    perform_bootstrap_handshake(child, askpass_listener, agent_resolver, conn_log).await
}

/// Run the `NEWT:READY` / `NEWT:NEED` negotiation on a freshly-spawned child
/// whose stdin/stdout will become the RPC channel. Shared between the
/// argv-appending bootstrap path and the env-var-based custom-shell path.
async fn perform_bootstrap_handshake(
    mut child: tokio::process::Child,
    askpass_listener: Option<newt_common::askpass::listener::AskpassListener>,
    agent_resolver: &dyn AgentResolver,
    conn_log: &ConnectionLog,
) -> Result<ChildConnection, Error> {
    conn_log.log("Process spawned, waiting for bootstrap...");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Start reading stderr immediately so transport logs appear during connection
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
            _askpass: askpass_listener,
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
            _askpass: askpass_listener,
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

/// Bootstrapless launch: detect the target architecture via the daemon's
/// `inspect` command, copy the matching agent binary in with `<engine> cp`, and
/// exec it directly. Used for distroless / `FROM scratch` containers that have
/// no shell to run `bootstrap.sh`.
async fn spawn_direct_copy(
    plan: &DirectCopyPlan,
    extra_path: &[String],
    agent_resolver: &dyn AgentResolver,
    conn_log: &ConnectionLog,
) -> Result<ChildConnection, Error> {
    // 1. Arch detection — run the pipeline; each step's stdout is piped into
    //    the next as `{prev}`.
    if plan.arch_detect_pipeline.is_empty() {
        return Err(Error::Custom("empty arch_detect_pipeline".into()));
    }
    let mut prev: String = String::new();
    for step in &plan.arch_detect_pipeline {
        let resolved: Vec<String> = step.iter().map(|a| a.replace("{prev}", &prev)).collect();
        let (prog, args) = resolved
            .split_first()
            .ok_or_else(|| Error::Custom("empty arch_detect step".into()))?;
        let prog = crate::path_resolver::resolve_program(prog, extra_path);
        conn_log.log(format!(
            "Detecting target arch: {} {}",
            prog.display(),
            args.join(" ")
        ));
        let out = tokio::process::Command::new(&prog)
            .args(args)
            .output()
            .await
            .map_err(|e| Error::Custom(format!("arch_detect failed to spawn: {}", e)))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(Error::Custom(format!(
                "arch detection failed (exit {:?}): {}",
                out.status.code(),
                stderr
            )));
        }
        prev = String::from_utf8_lossy(&out.stdout).trim().to_string();
    }
    let line = prev.as_str();
    let (os, arch) = line.split_once('/').ok_or_else(|| {
        Error::Custom(format!(
            "could not parse arch_detect output {:?} (expected `<os>/<arch>`)",
            line
        ))
    })?;
    let triple = newt_common::agent_resolver::triple_from_os_arch(os, arch).ok_or_else(|| {
        Error::Custom(format!(
            "unsupported target: os={:?}, arch={:?} (no matching agent triple)",
            os, arch
        ))
    })?;
    conn_log.log(format!("Target reports {}/{} → {}", os, arch, triple));

    // 2. Resolve local binary.
    let local_binary = agent_resolver.find_agent_binary(&triple)?;
    let agent_hash = agent_resolver.agent_hash()?;
    let remote_path = format!("/tmp/newt-agent-{}", agent_hash);

    // The source binary on disk is typically mode 644 (cargo-zigbuild output,
    // or unpacked from a tarball that lost the bit). `docker cp` preserves
    // mode, so a 644 file inside the container can't be exec'd. In the
    // bootstrap path, `bootstrap.sh` `chmod +x`'s the upload before exec;
    // bootstrapless has no such hook in the container, so we stage a +x copy
    // host-side and `docker cp` that.
    let staged = tempfile::Builder::new()
        .prefix("newt-agent-")
        .tempfile()
        .map_err(|e| Error::Custom(format!("could not create temp file: {}", e)))?;
    tokio::fs::copy(&local_binary, staged.path())
        .await
        .map_err(|e| Error::Custom(format!("could not stage agent for copy: {}", e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| Error::Custom(format!("could not chmod staged agent: {}", e)))?;
    }

    // 3. Copy. The destination is always /tmp/… in the target.
    let local_str = staged.path().to_string_lossy().to_string();
    let copy_argv: Vec<String> = plan
        .copy_cmd
        .iter()
        .map(|a| {
            a.replace("{local}", &local_str)
                .replace("{remote}", &remote_path)
        })
        .collect();
    let (copy_program, copy_args) = copy_argv
        .split_first()
        .ok_or_else(|| Error::Custom("empty copy_cmd".into()))?;
    let copy_program = crate::path_resolver::resolve_program(copy_program, extra_path);
    conn_log.log(format!(
        "Copying agent: {} {}",
        copy_program.display(),
        copy_args.join(" ")
    ));
    let copy_status = tokio::process::Command::new(&copy_program)
        .args(copy_args)
        .output()
        .await
        .map_err(|e| Error::Custom(format!("cp failed to spawn: {}", e)))?;
    if !copy_status.status.success() {
        let stderr = String::from_utf8_lossy(&copy_status.stderr)
            .trim()
            .to_string();
        return Err(Error::Custom(format!(
            "agent copy failed (exit {:?}): {}",
            copy_status.status.code(),
            stderr
        )));
    }
    conn_log.log("Agent copied");

    // 4. Exec.
    let exec_argv: Vec<String> = plan
        .exec_cmd
        .iter()
        .map(|a| a.replace("{agent_path}", &remote_path))
        .collect();
    let (exec_program, exec_args) = exec_argv
        .split_first()
        .ok_or_else(|| Error::Custom("empty exec_cmd".into()))?;
    let exec_program = crate::path_resolver::resolve_program(exec_program, extra_path);
    conn_log.log(format!(
        "Exec: {} {}",
        exec_program.display(),
        exec_args.join(" ")
    ));
    let mut cmd = tokio::process::Command::new(&exec_program);
    cmd.args(exec_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    set_parent_death_signal(&mut cmd);
    let mut child = cmd.spawn()?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stream = make_stream(BufReader::new(stdout), stdin);

    Ok(ChildConnection {
        child,
        stream,
        stderr: Some(stderr),
        _askpass: None,
    })
}

fn spawn_spec_label(spec: &SpawnSpec) -> &str {
    match spec {
        SpawnSpec::Bootstrap { label, .. } => label,
        SpawnSpec::DirectCopy(p) => &p.label,
        SpawnSpec::CustomShell { label, .. } => label,
    }
}

/// Run a user-supplied shell command locally via `sh -c <command>`. The
/// bootstrap script is exposed as `NEWT_BOOTSTRAP` so the user can splice it
/// in (`ssh host "$NEWT_BOOTSTRAP"`, `bash -c "$NEWT_BOOTSTRAP"`, etc.).
///
/// If `skip_bootstrap` is false (the default), we still run the bootstrap
/// handshake on the resulting stdin/stdout — so any sane interpolation of
/// `$NEWT_BOOTSTRAP` Just Works. If true, we hand the pipe directly to RPC,
/// assuming the user produced a ready agent out of band.
async fn spawn_custom_shell(
    command: &str,
    skip_bootstrap: bool,
    agent_resolver: &dyn AgentResolver,
    conn_log: &ConnectionLog,
) -> Result<ChildConnection, Error> {
    let script = BOOTSTRAP_SCRIPT.replace("__NEWT_HASH__", &agent_resolver.agent_hash()?);

    conn_log.log(format!(
        "Running custom command (skip_bootstrap={}): sh -c {:?}",
        skip_bootstrap, command
    ));
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .env("NEWT_BOOTSTRAP", &script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        cmd.env("NEWT_RUST_LOG", rust_log);
    }
    set_parent_death_signal(&mut cmd);
    let mut child = cmd.spawn()?;

    if skip_bootstrap {
        // Trust the user — pipe straight to RPC.
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        Ok(ChildConnection {
            child,
            stream: make_stream(BufReader::new(stdout), stdin),
            stderr: Some(stderr),
            _askpass: None,
        })
    } else {
        perform_bootstrap_handshake(child, None, agent_resolver, conn_log).await
    }
}

/// Spawn pkexec + agent binary (elevated mode, Linux only).
async fn spawn_elevated(agent_resolver: &dyn AgentResolver) -> Result<ChildConnection, Error> {
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
        _askpass: None,
    })
}

/// Spawn a background task that waits for the child to exit, then clears the
/// session and sets the connection status to `Disconnected`.
fn spawn_child_watcher(
    mut child: tokio::process::Child,
    stderr: Option<tokio::process::ChildStderr>,
    askpass_listener: Option<newt_common::askpass::listener::AskpassListener>,
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

        // Drop the askpass listener now that ssh is gone (cleans up the
        // socket file). Owned here so it lives at least as long as ssh.
        drop(askpass_listener);

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
    agent_resolver: &dyn AgentResolver,
    state: &MainWindowState,
    publisher: &Arc<UpdatePublisher<MainWindowState>>,
    preferences: crate::preferences::PreferencesHandle,
    session_slot: &Arc<arc_swap::ArcSwap<Option<Session>>>,
    set_status: impl Fn(&str),
    askpass_provider: Arc<dyn AskpassProvider>,
    main_window_ctx: super::MainWindowContext,
) -> Result<(), Error> {
    let (mut services, stderr_log, child) = match connection_target {
        ConnectionTarget::Local => {
            // Build SFTP askpass config from the resolved local agent binary
            // path + the host UI callback. If the agent binary can't be
            // resolved we proceed without askpass support — SFTP mounts will
            // then inherit the process environment and fail loudly if a
            // password is needed.
            let sftp_askpass = match agent_resolver.find_local_agent_binary() {
                Ok(askpass_binary) => Some(newt_common::api::SftpAskpass {
                    askpass_binary,
                    provider: askpass_provider.clone(),
                }),
                Err(e) => {
                    log::warn!(
                        "no local agent binary for SFTP askpass: {} (SFTP mounts that need a password will fail)",
                        e
                    );
                    None
                }
            };
            let progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink> =
                Arc::new(crate::main_window::LocalProgressSink::new(
                    state.vfs_progress.clone(),
                    publisher.clone(),
                ));
            let services = create_local_services(
                &state.operations,
                publisher,
                &preferences,
                sftp_askpass,
                askpass_provider.clone(),
                progress_sink,
            );
            (services, StderrLog::default(), None)
        }
        ConnectionTarget::Spawn(spec) => {
            set_status(&format!("Connecting via {}...", spawn_spec_label(spec)));
            let stderr_log = StderrLog::default();
            let conn_log = ConnectionLog {
                stderr_log: stderr_log.clone(),
                connection_status: state.connection_status.clone(),
                publisher: publisher.clone(),
            };
            let extra_path = preferences.load().environment.extra_path.clone();
            let conn = match spec {
                SpawnSpec::Bootstrap {
                    transport_cmd,
                    askpass,
                    shell_join,
                    ..
                } => {
                    spawn_bootstrap(
                        transport_cmd,
                        *askpass,
                        *shell_join,
                        &extra_path,
                        agent_resolver,
                        askpass_provider.clone(),
                        &conn_log,
                    )
                    .await?
                }
                SpawnSpec::DirectCopy(plan) => {
                    spawn_direct_copy(plan, &extra_path, agent_resolver, &conn_log).await?
                }
                SpawnSpec::CustomShell {
                    command,
                    skip_bootstrap,
                    ..
                } => {
                    spawn_custom_shell(command, *skip_bootstrap, agent_resolver, &conn_log).await?
                }
            };
            conn_log.log("Setting up RPC services...");
            let expose_local_fs = preferences.load().behavior.expose_local_fs;
            let progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink> =
                Arc::new(crate::main_window::LocalProgressSink::new(
                    state.vfs_progress.clone(),
                    publisher.clone(),
                ));
            let (services, _) = create_rpc_services(
                conn.stream,
                &state.operations,
                publisher,
                &preferences,
                expose_local_fs,
                askpass_provider.clone(),
                progress_sink,
            );
            conn_log.log("Connected");
            (
                services,
                stderr_log,
                Some((conn.child, conn.stderr, conn._askpass)),
            )
        }
        ConnectionTarget::Elevated => {
            set_status("Waiting for authorization...");
            let conn = spawn_elevated(agent_resolver).await?;
            let progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink> =
                Arc::new(crate::main_window::LocalProgressSink::new(
                    state.vfs_progress.clone(),
                    publisher.clone(),
                ));
            let (services, _) = create_rpc_services(
                conn.stream,
                &state.operations,
                publisher,
                &preferences,
                false,
                askpass_provider.clone(),
                progress_sink,
            );
            let stderr_log = StderrLog::default();
            (
                services,
                stderr_log,
                Some((conn.child, conn.stderr, conn._askpass)),
            )
        }
    };

    // Resolve initial directory for remote connections
    let default_dir = if matches!(connection_target, ConnectionTarget::Local) {
        services.initial_dir.clone()
    } else {
        services
            .shell_service
            .shell_expand("~".to_string())
            .await
            .map(VfsPath::root)
            .unwrap_or(services.initial_dir.clone())
    };

    // Per-pane CLI overrides (`--cwd-left`, `--cwd-right`). Passed through
    // shell_expand so users can write `--cwd-left ~/projects` and have it
    // resolve correctly on either side of the connection.
    let mut pane_dirs: [VfsPath; 2] = [default_dir.clone(), default_dir.clone()];
    for (slot, override_path) in main_window_ctx.initial_pane_paths().iter().enumerate() {
        if let Some(path) = override_path {
            let raw = path.to_string_lossy().into_owned();
            if let Ok(expanded) = services.shell_service.shell_expand(raw).await {
                pane_dirs[slot] = VfsPath::root(expanded);
            } else {
                log::warn!(
                    "could not resolve --cwd-{} path {:?}; falling back to default",
                    if slot == 0 { "left" } else { "right" },
                    path
                );
            }
        }
    }

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

    // For remote sessions, mount the remote VFS so the user can browse
    // the client-local filesystem from within the remote session.
    // Also set up hairpin diversion so list_files/read/write for the remote
    // VFS are handled locally without round-tripping through the agent.
    if matches!(connection_target, ConnectionTarget::Spawn(_))
        && preferences.load().behavior.expose_local_fs
    {
        match services
            .vfs_manager
            .mount(newt_common::vfs::MountRequest::Remote)
            .await
        {
            Ok(resp) => {
                log::info!("remote VFS mounted as vfs_id={:?}", resp.vfs_id);
                if let Some(desc) = newt_common::vfs::lookup_descriptor(&resp.type_name) {
                    initial_mounted.insert(
                        resp.vfs_id,
                        MountedVfsInfo {
                            vfs_id: resp.vfs_id,
                            descriptor: desc,
                            mount_meta: resp.mount_meta,
                            origin: None,
                        },
                    );
                }

                // Set up hairpin diversion — local VFS calls bypass the agent
                let local_vfs = Arc::new(LocalVfs::new());
                let local_registry = Arc::new(VfsRegistry::with_root(local_vfs));
                services.fs = Arc::new(HairpinFs {
                    remote_vfs_id: resp.vfs_id,
                    local_fs: VfsRegistryFs::new(local_registry.clone()),
                    inner: services.fs,
                });
                services.file_reader = Arc::new(HairpinFileReader {
                    remote_vfs_id: resp.vfs_id,
                    local_reader: VfsRegistryFileReader::new(local_registry),
                    inner: services.file_reader,
                });
            }
            Err(e) => {
                log::warn!("failed to mount remote VFS: {}", e);
            }
        }
    }

    let mounted_vfs = Arc::new(RwLock::new(initial_mounted));

    let vfs_info: Arc<dyn VfsInfo> = Arc::new(MountedVfsInfoService {
        mounted_vfs: mounted_vfs.clone(),
        remote_session: matches!(connection_target, ConnectionTarget::Spawn(_)),
    });

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

    let [left_dir, right_dir] = pane_dirs;
    state.panes.add(super::pane::Pane::new(
        services.fs.clone(),
        left_dir,
        state.display_options.clone(),
        preferences.clone(),
        publisher.clone(),
        vfs_info.clone(),
        Some(event_tx.clone()),
    ));
    state.panes.add(super::pane::Pane::new(
        services.fs.clone(),
        right_dir,
        state.display_options.clone(),
        preferences,
        publisher.clone(),
        vfs_info.clone(),
        Some(event_tx.clone()),
    ));

    set_status("Loading...");
    state.refresh(true).await?;

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
                        if let Err(e) = ctx.cleanup_stale_ephemeral_mounts().await {
                            log::warn!("failed to cleanup stale ephemeral mounts: {}", e);
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
        vfs_info,
        next_operation_id: AtomicU64::new(1),
        file_server_port,
        file_server_token,
        _file_server_handle: file_server_handle,
        _event_loop_handle: event_loop_handle,
    };

    session_slot.store(Arc::new(Some(session)));

    // Spawn process watcher for remote/elevated
    if let Some((child, stderr, askpass_listener)) = child {
        spawn_child_watcher(
            child,
            stderr,
            askpass_listener,
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
