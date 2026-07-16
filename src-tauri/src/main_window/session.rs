use newt_common::api::{
    API_ENRICHMENT_EVENT, API_LIST_FILES_BATCH, API_OPERATION_PROGRESS, VfsRegistryManager,
};
use newt_common::enrich::{
    EnricherClient, Enrichers, PendingEnrichments, du::DuEnricher, git::GitEnricher,
};
use newt_common::file_reader::FileReader;
use newt_common::filesystem::{
    FileList, Filesystem, LocalShellService, PendingStreams, ShellRemote, ShellService, StreamId,
};
use newt_common::operation::{OperationContext, OperationProgress, OperationsClient};
#[cfg(target_os = "linux")]
use newt_common::proc::NoConsoleWindow;
use newt_common::rpc::Communicator;
use newt_common::terminal::TerminalClient;
use newt_common::vfs::{
    LOCAL_VFS_DESCRIPTOR, LocalVfs, MountedVfsInfo, PathStyle, VfsDescriptor, VfsId, VfsManager,
    VfsManagerRemote, VfsPath, VfsRegistry, VfsRegistryFileReader, VfsRegistryFs,
};
use parking_lot::RwLock;
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::common::{Error, UpdatePublisher};

use super::{MainWindowState, Operations, apply_operation_progress};

use newt_common::askpass::AskpassProvider;
#[cfg(target_os = "linux")]
use newt_common::connect::set_parent_death_signal;
use newt_common::connect::{ConnectLog, DynStderr, DynStream, spawn_stderr_reader};
pub use newt_common::connect::{
    DirectCopyPlan, SpawnSpec, docker_direct_copy_plan, docker_transport_cmd, kube_transport_cmd,
    podman_direct_copy_plan, podman_transport_cmd, ssh_transport_cmd,
};

// ---------------------------------------------------------------------------
// ConnectionTarget
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum ConnectionTarget {
    /// No subprocess: services run directly in the Tauri process.
    Local,
    /// pkexec-elevated agent on the local machine (Linux only).
    Elevated,
    /// Agent launched inside a WSL distribution via `wslapi!WslLaunch`
    /// (Windows only). The bundled Linux-musl agent is exec'd directly
    /// from its `/mnt/<drive>/…` path — no bootstrap, no upload.
    #[cfg(windows)]
    Wsl { distro: String },
    /// Spawn an agent over an external transport.
    Spawn(SpawnSpec),
}

impl ConnectionTarget {
    /// A "remote-style" session: the agent's FS is a single Unix root and
    /// the client-local FS is exposed via the Remote VFS. True for every
    /// spawned transport and for WSL; false for Local / Elevated.
    pub fn is_remote(&self) -> bool {
        match self {
            ConnectionTarget::Spawn(_) => true,
            #[cfg(windows)]
            ConnectionTarget::Wsl { .. } => true,
            _ => false,
        }
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

pub use newt_common::agent_resolver::AgentResolver;
use newt_common::agent_resolver::{agent_file_name, local_agent_triple};

/// Host-side resolver. Searches directories in priority order:
/// 1. `NEWT_AGENT_DIR` env var (runtime dev override)
/// 2. `NEWT_SYSTEM_AGENT_DIR` compile-time path (distro packages)
/// 3. Tauri resource dir (`agents/` inside the bundled app)
/// 4. `agents/` relative fallback (legacy/dev)
pub struct TauriAgentResolver {
    dirs: Vec<std::path::PathBuf>,
}

impl TauriAgentResolver {
    pub fn new(app_handle: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        let mut dirs = Vec::new();

        if let Ok(dir) = std::env::var("NEWT_AGENT_DIR") {
            dirs.push(std::path::PathBuf::from(dir));
        }

        if let Some(dir) = option_env!("NEWT_SYSTEM_AGENT_DIR") {
            dirs.push(std::path::PathBuf::from(dir));
        }

        if let Ok(resource_dir) = app_handle.path().resource_dir() {
            dirs.push(resource_dir.join("agents"));
        }

        dirs.push(std::path::PathBuf::from("agents"));

        Self { dirs }
    }
}

#[async_trait::async_trait]
impl AgentResolver for TauriAgentResolver {
    /// Compute a hash that changes whenever any agent binary changes.
    async fn agent_hash(&self) -> Result<String, newt_common::Error> {
        let mut hasher = blake3::Hasher::new();
        let mut found = false;

        for dir in &self.dirs {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    // Triple-subdir layout: <dir>/<triple>/newt-agent[.exe]
                    // (the agent's extension follows the triple's OS, not
                    // the host's).
                    if let Some(triple) = p.file_name().and_then(|n| n.to_str()) {
                        let nested = p.join(agent_file_name(triple));
                        if nested.is_file() {
                            hasher.update(&std::fs::read(&nested)?);
                            found = true;
                        }
                    }
                    // Flat layout: <dir>/newt-agent[.exe]
                    if p.is_file()
                        && p.file_name()
                            .is_some_and(|n| n == "newt-agent" || n == "newt-agent.exe")
                    {
                        hasher.update(&std::fs::read(&p)?);
                        found = true;
                    }
                }
            }
            // Flat layout, direct: <dir>/newt-agent[.exe]
            for name in ["newt-agent", "newt-agent.exe"] {
                let flat = dir.join(name);
                if flat.is_file() {
                    hasher.update(&std::fs::read(&flat)?);
                    found = true;
                }
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
    fn find_agent_binary(&self, triple: &str) -> Result<std::path::PathBuf, newt_common::Error> {
        let name = agent_file_name(triple);
        for dir in &self.dirs {
            let path = dir.join(triple).join(name);
            if path.exists() {
                return Ok(path);
            }
            let path = dir.join(name);
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
    fn find_local_agent_binary(&self) -> Result<std::path::PathBuf, newt_common::Error> {
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
    pub(super) discovery_provider: Arc<dyn newt_common::discovery::DiscoveryProvider>,
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
    fn display_name(&self, vfs_id: VfsId) -> Option<String>;
    /// Returns the VFS ID of a filesystem that is local to the host machine
    /// (the machine running the Tauri process), or `None` if no such VFS is mounted.
    fn host_local_vfs_id(&self) -> Option<VfsId>;

    /// Resolve a `VfsPath` to a directory on the terminal's filesystem
    /// (`VfsId::ROOT` — the local FS in local mode, the agent's FS in
    /// remote/elevated mode), suitable for use as a child-process cwd.
    ///
    /// For paths already on `VfsId::ROOT` this returns the path unchanged.
    /// For VFSes that have an origin it steps to the origin and recurses:
    /// the enclosing directory for an entry origin (archives), the origin
    /// directory itself for a directory origin (searches). For VFSes with
    /// no origin (S3, SFTP, Kubernetes, Remote) this returns `None`, so
    /// callers can fall back to the spawning process's inherited cwd.
    fn resolve_terminal_cwd(&self, path: &VfsPath) -> Option<newt_common::vfs::path::PathBuf> {
        let mut current = path.clone();
        loop {
            if current.vfs_id == VfsId::ROOT {
                // Return the VFS path; the terminal client converts it to
                // a native path on the side that spawns the PTY (the
                // agent in a remote session), in that OS — no `std::path`
                // crosses the RPC boundary here.
                return Some(current.path);
            }
            let (desc, _) = self.descriptor(current.vfs_id)?;
            let origin = self.origin(current.vfs_id)?;
            current = match desc.origin_kind() {
                newt_common::vfs::OriginKind::Directory => origin,
                _ => origin.parent()?,
            };
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

    fn display_name(&self, vfs_id: VfsId) -> Option<String> {
        let (descriptor, mount_meta) = self
            .mounted_vfs
            .read()
            .get(&vfs_id)
            .map(|info| (info.descriptor, info.mount_meta.clone()))?;

        // Agent mounts display their transport kind ("Docker", "SSH", …)
        // — a plain "Remote" is ambiguous next to the session's own root.
        if descriptor.type_name() == "agent" {
            return Some(
                newt_common::vfs::mount_meta_kind(&mount_meta)
                    .unwrap_or_else(|| descriptor.display_name().to_string()),
            );
        }

        Some(
            match descriptor.type_name() {
                "local" if !self.remote_session => "Local",
                "local" if self.remote_session => "Remote",
                "remote" if self.remote_session => "Local",
                "remote" if !self.remote_session => "Remote",
                _ => descriptor.display_name(), // For custom VFS types, just show the type name
            }
            .to_string(),
        )
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

    async fn get_property_sheet(
        &self,
        path: VfsPath,
    ) -> Result<newt_common::vfs::PropertySheet, newt_common::Error> {
        self.inner.get_property_sheet(path).await
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

impl ConnectLog for ConnectionLog {
    fn log(&self, line: String) {
        ConnectionLog::log(self, line);
    }
}

// ---------------------------------------------------------------------------
// RPC dispatcher — receives notifications from the agent
// ---------------------------------------------------------------------------

struct HostDispatcher {
    operations: Operations,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
    pending_streams: PendingStreams,
    pending_enrichments: PendingEnrichments,
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
        } else if api == API_ENRICHMENT_EVENT {
            let (id, event): (
                newt_common::enrich::EnrichmentId,
                newt_common::enrich::EnrichmentEvent,
            ) = bincode::deserialize(&req[..]).unwrap();
            let tx = self.pending_enrichments.lock().get(&id).cloned();
            if let Some(tx) = tx {
                let _ = tx.send(event).await;
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
    enricher_client: Arc<dyn EnricherClient>,
    hot_paths_provider: Arc<dyn newt_common::hot_paths::HotPathsProvider>,
    discovery_provider: Arc<dyn newt_common::discovery::DiscoveryProvider>,
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
    agent_resolver: Arc<dyn AgentResolver>,
) -> Services {
    let (progress_tx, mut progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<OperationProgress>();

    let registry = Arc::new(VfsRegistry::with_root(Arc::new(LocalVfs::new())));
    let op_context = Arc::new(OperationContext {
        registry: registry.clone(),
    });

    let operations = operations.clone();
    let publisher_clone = publisher.clone();
    let preferences_progress = preferences.clone();
    tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            let keep = preferences_progress
                .load()
                .behavior
                .keep_finished_operations;
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
                .with_progress_sink(progress_sink)
                .with_agent_resolver(agent_resolver)
                .with_extra_path(preferences.load().environment.extra_path.clone());
            if let Some(askpass) = sftp_askpass {
                mgr.with_sftp_askpass(askpass)
            } else {
                mgr
            }
        }),
        terminal_client: Arc::new(newt_common::terminal::Local::new()),
        file_reader: Arc::new(VfsRegistryFileReader::new(registry.clone())),
        operations_client: Arc::new(newt_common::operation::Local::new(progress_tx, op_context)),
        enricher_client: Arc::new(newt_common::enrich::Local::new(Arc::new(
            Enrichers::new(registry.clone())
                .with(Arc::new(GitEnricher::new(
                    preferences.load().environment.extra_path.clone(),
                )))
                .with(Arc::new(DuEnricher)),
        ))),
        hot_paths_provider: Arc::new(newt_common::hot_paths::Local::new()),
        discovery_provider: Arc::new(newt_common::discovery::Local::new(
            preferences.load().environment.extra_path.clone(),
        )),
        initial_dir: VfsPath::new(
            VfsId::ROOT,
            newt_common::vfs::local::local_path_from_native(&std::env::current_dir().unwrap()),
        ),
    }
}

/// Build remote proxy services from a communicator.
fn create_remote_services(
    communicator: Communicator,
    pending_streams: PendingStreams,
    pending_enrichments: PendingEnrichments,
) -> Services {
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
        enricher_client: Arc::new(newt_common::enrich::Remote::new(
            communicator.clone(),
            pending_enrichments,
        )),
        hot_paths_provider: Arc::new(newt_common::hot_paths::Remote::new(communicator.clone())),
        discovery_provider: Arc::new(newt_common::discovery::Remote::new(communicator)),
        initial_dir: VfsPath::root(VfsId::ROOT),
    }
}

/// Set up the communicator + host dispatcher over a bidirectional stream,
/// and return the services + communicator.
#[allow(clippy::too_many_arguments)]
fn create_rpc_services(
    stream: impl AsyncRead + AsyncWrite + Send + Unpin + 'static,
    operations: &Operations,
    publisher: &Arc<UpdatePublisher<MainWindowState>>,
    preferences: &crate::preferences::PreferencesHandle,
    expose_local_fs: bool,
    askpass_provider: Arc<dyn AskpassProvider>,
    progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink>,
    agent_resolver: Arc<dyn AgentResolver>,
) -> (Services, PendingStreams) {
    use newt_common::rpc::DispatcherExt;

    let pending_streams: PendingStreams = Arc::new(parking_lot::Mutex::new(HashMap::new()));
    let pending_enrichments: PendingEnrichments = Arc::new(parking_lot::Mutex::new(HashMap::new()));

    let host_dispatcher = HostDispatcher {
        operations: operations.clone(),
        publisher: publisher.clone(),
        pending_streams: pending_streams.clone(),
        pending_enrichments: pending_enrichments.clone(),
        preferences: preferences.clone(),
        progress_sink,
    };
    let askpass_dispatcher = HostAskpassDispatcher {
        provider: askpass_provider,
    };
    let (outbox, inbox) = Communicator::create_outbox();
    let fetch_dispatcher =
        newt_common::api::AgentFetchDispatcher::new(agent_resolver, outbox.clone());
    let base = host_dispatcher
        .chain(askpass_dispatcher)
        .chain(fetch_dispatcher);
    let communicator = if expose_local_fs {
        use newt_common::api::VfsDispatcher;
        use newt_common::vfs::LocalVfs;
        let host_vfs = Arc::new(LocalVfs::new());
        let combined_dispatcher = base.chain(VfsDispatcher::new(host_vfs, outbox.clone()));
        Communicator::with_dispatcher_and_outbox(combined_dispatcher, stream, outbox, inbox)
    } else {
        Communicator::with_dispatcher_and_outbox(base, stream, outbox, inbox)
    };
    let services =
        create_remote_services(communicator, pending_streams.clone(), pending_enrichments);
    (services, pending_streams)
}

// ---------------------------------------------------------------------------
// Child process spawning
// ---------------------------------------------------------------------------

/// The agent's host-side process, unified so `spawn_child_watcher` can
/// `.wait()` regardless of transport. The variant size disparity is fine:
/// one short-lived owner per session, boxing would just add an allocation.
#[cfg_attr(windows, allow(clippy::large_enum_variant))]
enum AgentProcess {
    Child(tokio::process::Child),
    /// Raw Win32 process handle (WSL `WslLaunch` / elevated `ShellExecuteEx`),
    /// wrapped because it can't be adopted into a `tokio::process::Child`.
    #[cfg(windows)]
    Win(super::win_proc::WinProcess),
}

impl AgentProcess {
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            AgentProcess::Child(c) => c.wait().await,
            #[cfg(windows)]
            AgentProcess::Win(w) => w.wait().await,
        }
    }
}

/// A spawned agent connection adapted to the host's process handle type
/// (WSL / elevated use raw Win32 handles that aren't `tokio::process::Child`).
struct ChildConnection {
    child: AgentProcess,
    stderr: Option<DynStderr>,
    stream: DynStream,
    /// Askpass listener tied to the ssh process; dropped when the connection
    /// tears down. `None` for elevated / WSL mode (no askpass).
    _askpass: Option<newt_common::askpass::listener::AskpassListener>,
}

impl From<newt_common::connect::SpawnedAgent> for ChildConnection {
    fn from(conn: newt_common::connect::SpawnedAgent) -> Self {
        Self {
            child: AgentProcess::Child(conn.child),
            stderr: conn.stderr,
            stream: conn.stream,
            _askpass: conn.askpass,
        }
    }
}

/// Spawn pkexec + agent binary (elevated mode, Linux).
#[cfg(target_os = "linux")]
async fn spawn_elevated(agent_resolver: &dyn AgentResolver) -> Result<ChildConnection, Error> {
    let agent_path = agent_resolver.find_local_agent_binary()?;
    let mut cmd = tokio::process::Command::new("pkexec");
    cmd.no_console_window();
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
        child: AgentProcess::Child(child),
        stream,
        stderr: Some(Box::new(stderr)),
        _askpass: None,
    })
}

/// Spawn an elevated agent via UAC (`ShellExecuteEx "runas"`) and connect to
/// it over a named pipe (stdio can't cross the UAC boundary).
#[cfg(windows)]
async fn spawn_elevated(agent_resolver: &dyn AgentResolver) -> Result<ChildConnection, Error> {
    let spawn = super::elevate::spawn_elevated_windows(agent_resolver).await?;
    let stream = tokio_duplex::Duplex::new(spawn.stdout, spawn.stdin);
    Ok(ChildConnection {
        child: AgentProcess::Win(spawn.process),
        stream,
        stderr: None,
        _askpass: None,
    })
}

/// Elevated mode is unsupported on this platform (e.g. macOS).
#[cfg(not(any(target_os = "linux", windows)))]
async fn spawn_elevated(_agent_resolver: &dyn AgentResolver) -> Result<ChildConnection, Error> {
    Err(Error::Custom(
        "elevated mode is not supported on this platform".into(),
    ))
}

/// Spawn a background task that waits for the child to exit, then clears the
/// session and sets the connection status to `Disconnected`.
fn spawn_child_watcher(
    mut child: AgentProcess,
    stderr: Option<DynStderr>,
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
            Arc::new(ConnectionLog {
                stderr_log: stderr_log.clone(),
                connection_status: connection_status.clone(),
                publisher: publisher.clone(),
            }),
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
    agent_resolver: Arc<dyn AgentResolver>,
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
                    state.mount_log.clone(),
                    publisher.clone(),
                ));
            let services = create_local_services(
                &state.operations,
                publisher,
                &preferences,
                sftp_askpass,
                askpass_provider.clone(),
                progress_sink,
                agent_resolver.clone(),
            );
            (services, StderrLog::default(), None)
        }
        ConnectionTarget::Spawn(spec) => {
            set_status(&format!("Connecting via {}...", spec.label()));
            let stderr_log = StderrLog::default();
            let conn_log = Arc::new(ConnectionLog {
                stderr_log: stderr_log.clone(),
                connection_status: state.connection_status.clone(),
                publisher: publisher.clone(),
            });
            let extra_path = preferences.load().environment.extra_path.clone();
            let conn: ChildConnection = newt_common::connect::spawn(
                spec,
                newt_common::connect::AgentMode::Session,
                &extra_path,
                agent_resolver.as_ref(),
                askpass_provider.clone(),
                conn_log.clone(),
            )
            .await?
            .into();
            conn_log.log("Setting up RPC services...");
            let expose_local_fs = preferences.load().behavior.expose_local_fs;
            let progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink> =
                Arc::new(crate::main_window::LocalProgressSink::new(
                    state.vfs_progress.clone(),
                    state.mount_log.clone(),
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
                agent_resolver.clone(),
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
            let conn = spawn_elevated(agent_resolver.as_ref()).await?;
            let progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink> =
                Arc::new(crate::main_window::LocalProgressSink::new(
                    state.vfs_progress.clone(),
                    state.mount_log.clone(),
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
                agent_resolver.clone(),
            );
            let stderr_log = StderrLog::default();
            (
                services,
                stderr_log,
                Some((conn.child, conn.stderr, conn._askpass)),
            )
        }
        #[cfg(windows)]
        ConnectionTarget::Wsl { distro } => {
            set_status(&format!("Connecting to WSL [{}]...", distro));
            let stderr_log = StderrLog::default();
            let conn_log = Arc::new(ConnectionLog {
                stderr_log: stderr_log.clone(),
                connection_status: state.connection_status.clone(),
                publisher: publisher.clone(),
            });
            let spawn = super::wsl_launch::spawn_wsl(distro, agent_resolver.as_ref()).await?;
            // Stream WSL stderr into the connection log right away.
            spawn_stderr_reader(spawn.stderr, conn_log.clone());
            conn_log.log("Setting up RPC services...");
            let stream: DynStream = tokio_duplex::Duplex::new(spawn.stdout, spawn.stdin);
            let expose_local_fs = preferences.load().behavior.expose_local_fs;
            let progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink> =
                Arc::new(crate::main_window::LocalProgressSink::new(
                    state.vfs_progress.clone(),
                    state.mount_log.clone(),
                    publisher.clone(),
                ));
            let (services, _) = create_rpc_services(
                stream,
                &state.operations,
                publisher,
                &preferences,
                expose_local_fs,
                askpass_provider.clone(),
                progress_sink,
                agent_resolver.clone(),
            );
            conn_log.log("Connected");
            (
                services,
                stderr_log,
                Some((AgentProcess::Win(spawn.process), None, None)),
            )
        }
    };

    // Resolve initial directory for remote connections
    let default_dir = if matches!(connection_target, ConnectionTarget::Local) {
        services.initial_dir.clone()
    } else {
        match services.shell_service.shell_expand("~".to_string()).await {
            Ok(Some(home)) => VfsPath::new(VfsId::ROOT, home),
            _ => services.initial_dir.clone(),
        }
    };

    // Per-pane CLI overrides (`--cwd-left`, `--cwd-right`). Passed through
    // shell_expand so users can write `--cwd-left ~/projects` and have it
    // resolve correctly on either side of the connection.
    let mut pane_dirs: [VfsPath; 2] = [default_dir.clone(), default_dir.clone()];
    for (slot, override_path) in main_window_ctx.initial_pane_paths().iter().enumerate() {
        if let Some(path) = override_path {
            let raw = path.to_string_lossy().into_owned();
            match services.shell_service.shell_expand(raw).await {
                Ok(Some(expanded)) => {
                    pane_dirs[slot] = VfsPath::new(VfsId::ROOT, expanded);
                }
                _ => {
                    log::warn!(
                        "could not resolve --cwd-{} path {:?}; falling back to default",
                        if slot == 0 { "left" } else { "right" },
                        path
                    );
                }
            }
        }
    }

    // The root `LocalVfs` isn't mounted via the registry, so `mount_meta()`
    // isn't consulted — stamp the path style here. Remote root = the agent's
    // Unix FS (style-only, single `/`). Local root = this host's FS, with
    // drives enumerated so a Windows host lands on a drive instead of the
    // unlistable `/`.
    let root_meta = if connection_target.is_remote() {
        PathStyle::Unix.encode()
    } else {
        newt_common::vfs::encode_mount_meta(
            PathStyle::host(),
            &newt_common::vfs::local::local_roots(),
        )
    };
    let mut initial_mounted = HashMap::new();
    initial_mounted.insert(
        VfsId::ROOT,
        MountedVfsInfo {
            vfs_id: VfsId::ROOT,
            descriptor: &LOCAL_VFS_DESCRIPTOR,
            mount_meta: root_meta,
            origin: None,
        },
    );

    // For remote sessions, mount the remote VFS so the user can browse
    // the client-local filesystem from within the remote session.
    // Also set up hairpin diversion so list_files/read/write for the remote
    // VFS are handled locally without round-tripping through the agent.
    if connection_target.is_remote() && preferences.load().behavior.expose_local_fs {
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
                            // This VFS surfaces *this host's* local FS into
                            // the remote session: host path style, and the
                            // host's drives so a Windows client lands on a
                            // drive instead of the unlistable `/`.
                            mount_meta: newt_common::vfs::encode_mount_meta(
                                PathStyle::host(),
                                &newt_common::vfs::local::local_roots(),
                            ),
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
        remote_session: connection_target.is_remote(),
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
        services.enricher_client.clone(),
        Some(event_tx.clone()),
    ));
    state.panes.add(super::pane::Pane::new(
        services.fs.clone(),
        right_dir,
        state.display_options.clone(),
        preferences,
        publisher.clone(),
        vfs_info.clone(),
        services.enricher_client.clone(),
        Some(event_tx.clone()),
    ));

    set_status("Loading...");
    state.refresh(true).await?;

    for pane in state.panes.all() {
        tauri::async_runtime::spawn(pane.clone().enrichment_loop());
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
        discovery_provider: services.discovery_provider,
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
