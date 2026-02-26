pub mod pane;
pub mod terminal;

use newt_common::api::{VfsRegistryManager, API_LIST_FILES_BATCH, API_OPERATION_PROGRESS};
use newt_common::file_reader::FileReader;
use newt_common::filesystem::{
    File, Filesystem, LocalShellService, PendingStreams, ShellRemote, ShellService, StreamId,
    UserGroup,
};
use newt_common::operation::{OperationId, OperationProgress, OperationsClient};
use newt_common::rpc::Communicator;

use newt_common::operation::OperationContext;
use newt_common::terminal::TerminalClient;
use newt_common::terminal::TerminalHandle;
use newt_common::vfs::{
    lookup_descriptor, LocalVfs, MountedVfsInfo, VfsId, VfsManager,
    VfsManagerRemote, VfsRegistry, VfsRegistryFileReader, VfsRegistryFs, LOCAL_VFS_DESCRIPTOR,
};
use parking_lot::RwLock;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use newt_common::vfs::VfsPath;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;

use std::process::Stdio;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::Manager;
use tauri::State;
use tauri::WebviewWindow;
use tauri::Wry;

use crate::common::Error;
use crate::common::UpdatePublisher;
use crate::GlobalContext;

use self::pane::Pane;
use self::terminal::Terminal;

#[derive(Clone, Debug)]
pub enum ConnectionTarget {
    Local,
    Remote { transport_cmd: Vec<String> },
    Elevated,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "event", content = "data")]
pub enum InitEvent {
    Status { message: String },
}

fn send_init_status(channel: Option<&Channel<InitEvent>>, message: &str) {
    if let Some(ch) = channel {
        let _ = ch.send(InitEvent::Status {
            message: message.to_string(),
        });
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayOptionsInner {
    pub show_hidden: bool,
    pub active_pane: PaneHandle,
    pub active_terminal: Option<TerminalHandle>,
    pub panes_focused: bool,
}

#[derive(Default, Clone)]
pub struct DisplayOptions(pub Arc<RwLock<DisplayOptionsInner>>);

impl Default for DisplayOptionsInner {
    fn default() -> Self {
        Self {
            show_hidden: false,
            active_pane: PaneHandle(0),
            active_terminal: None,
            panes_focused: true,
        }
    }
}

impl serde::Serialize for DisplayOptions {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(
    Default,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Clone,
    Copy,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct PaneHandle(usize);

#[derive(Clone)]
pub struct Panes(Arc<RwLock<Vec<Arc<Pane>>>>);

impl Default for Panes {
    fn default() -> Self {
        Self::new()
    }
}

impl Panes {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Vec::new())))
    }

    pub fn add(&self, pane: Pane) {
        let mut lock = self.0.write();
        lock.push(Arc::new(pane));
    }

    pub fn get(&self, handle: PaneHandle) -> Option<Arc<Pane>> {
        self.0.read().get(handle.0).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Pane>> {
        self.0.read().clone()
    }
}

impl serde::Serialize for Panes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let locked = self.0.read();
        let mut seq = serializer.serialize_seq(Some(locked.len()))?;
        for e in locked.iter() {
            seq.serialize_element(&**e)?;
        }
        seq.end()
    }
}

#[derive(Clone)]
pub struct Terminals(Arc<RwLock<HashMap<TerminalHandle, Arc<Terminal>>>>);

impl Default for Terminals {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminals {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }

    pub fn len(&self) -> usize {
        self.0.read().len()
    }

    pub fn get(&self, handle: TerminalHandle) -> Option<Arc<Terminal>> {
        self.0.read().get(&handle).cloned()
    }

    pub fn insert(&self, handle: TerminalHandle, terminal: Terminal) -> Arc<Terminal> {
        let mut lock = self.0.write();
        let term = Arc::new(terminal);
        lock.insert(handle, term.clone());
        term
    }

    pub fn remove(&self, handle: TerminalHandle) -> Option<Arc<Terminal>> {
        self.0.write().remove(&handle)
    }
}

impl serde::Serialize for Terminals {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let lock = self.0.read();

        let mut seq = serializer.serialize_map(Some(lock.len()))?;
        for e in lock.iter() {
            seq.serialize_entry(&e.0, &**e.1)?;
        }
        seq.end()
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Scanning,
    Running,
    Completed,
    Failed,
    Cancelled,
    WaitingForInput,
}

#[derive(Clone, serde::Serialize)]
pub struct OperationIssueInfo {
    pub issue_id: u64,
    pub kind: String,
    pub message: String,
    pub detail: Option<String>,
    pub actions: Vec<String>,
}

#[derive(Clone, serde::Serialize)]
pub struct OperationState {
    pub id: OperationId,
    pub kind: String,
    pub description: String,
    pub total_bytes: Option<u64>,
    pub total_items: Option<u64>,
    pub bytes_done: u64,
    pub items_done: u64,
    pub current_item: String,
    pub status: OperationStatus,
    pub error: Option<String>,
    pub issue: Option<OperationIssueInfo>,
    pub backgrounded: bool,
}

#[derive(Clone)]
pub struct Operations(pub Arc<RwLock<HashMap<OperationId, OperationState>>>);

impl Default for Operations {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }
}

impl Operations {
    pub fn foreground_operation_id(&self) -> Option<OperationId> {
        self.0
            .read()
            .values()
            .filter(|op| !op.backgrounded)
            .min_by_key(|op| op.id)
            .map(|op| op.id)
    }
}

impl serde::Serialize for Operations {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let lock = self.0.read();
        let mut map = serializer.serialize_map(Some(lock.len()))?;
        for (k, v) in lock.iter() {
            map.serialize_entry(&k.to_string(), v)?;
        }
        map.end()
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ModalDataKind {
    CreateDirectory {
        path: VfsPath,
    },
    CreateFile {
        path: VfsPath,
    },
    Properties {
        paths: Vec<VfsPath>,

        mode: Option<u32>,
        owner: Option<UserGroup>,
        group: Option<UserGroup>,
    },
    Navigate {
        path: VfsPath,
    },
    Rename {
        base_path: VfsPath,
        name: String,
    },
    CopyMove {
        kind: String,
        sources: Vec<VfsPath>,
        destination: VfsPath,
    },
    ConnectRemote {
        host: String,
    },
}

#[derive(Clone, serde::Serialize)]
pub struct ModalContext {
    pub pane_handle: Option<PaneHandle>,
}

#[derive(Clone, serde::Serialize)]
pub struct ModalData {
    #[serde(flatten)]
    pub kind: ModalDataKind,
    pub context: ModalContext,
}

#[derive(Clone)]
pub struct ModalState(pub Arc<RwLock<Option<ModalData>>>);

impl Default for ModalState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(None)))
    }
}

impl serde::Serialize for ModalState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DndFile {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, serde::Serialize)]
pub struct DndData {
    pub source_pane: PaneHandle,
    pub files: Vec<DndFile>,
}

#[derive(Clone)]
pub struct DndState(pub Arc<RwLock<Option<DndData>>>);

impl Default for DndState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(None)))
    }
}

impl serde::Serialize for DndState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(Clone)]
pub struct MainWindowState {
    pub panes: Panes,
    pub terminals: Terminals,
    pub modal: ModalState,
    pub dnd: DndState,
    pub display_options: DisplayOptions,
    pub operations: Operations,
    pub window_title: String,
}

impl serde::Serialize for MainWindowState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        use serde::ser::SerializeStruct;
        let foreground_id = self.operations.foreground_operation_id();
        let mut s = serializer.serialize_struct("MainWindowState", 8)?;
        s.serialize_field("panes", &self.panes)?;
        s.serialize_field("terminals", &self.terminals)?;
        s.serialize_field("modal", &self.modal)?;
        s.serialize_field("dnd", &self.dnd)?;
        s.serialize_field("display_options", &self.display_options)?;
        s.serialize_field("operations", &self.operations)?;
        s.serialize_field("window_title", &self.window_title)?;
        s.serialize_field("foreground_operation_id", &foreground_id)?;
        s.end()
    }
}

impl MainWindowState {
    fn new() -> Self {
        let display_options = DisplayOptions::default();

        Self {
            panes: Panes::new(),
            terminals: Terminals::new(),
            modal: ModalState::default(),
            dnd: DndState::default(),
            display_options,
            operations: Operations::default(),
            window_title: "Newt".to_string(),
        }
    }

    pub fn other_pane(&self, handle: PaneHandle) -> Arc<Pane> {
        self.panes.get(PaneHandle(1 - handle.0)).unwrap()
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        for pane in self.panes.all() {
            pane.refresh(None).await?;
        }
        Ok(())
    }

    pub fn close_modal(&self) {
        *self.modal.0.write() = None;
    }

    pub fn activate_pane(&self, handle: PaneHandle) {
        let mut opts = self.display_options.0.write();
        opts.active_pane = handle;
        opts.panes_focused = true;
    }

    pub async fn copy_pane(&self, handle: PaneHandle) -> Result<(), Error> {
        let other_pane = self.other_pane(handle);
        let pane = self.panes.get(handle).unwrap();

        pane.navigate_to(other_pane.path()).await?;

        Ok(())
    }

    pub fn toggle_hidden(&self) {
        {
            let mut display_options = self.display_options.0.write();
            display_options.show_hidden = !display_options.show_hidden;
        }

        for pane in self.panes.all() {
            pane.update_view_state();
        }
    }
}
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
            let (stream_id, files): (StreamId, Vec<File>) = bincode::deserialize(&req[..]).unwrap();
            if let Some(tx) = self.pending_streams.lock().get(&stream_id) {
                let _ = tx.send(files);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// A child process handle that can be awaited for exit.
/// Wraps `tokio::process::Child` but only exposes the wait handle.
struct ChildWaitHandle {
    child: tokio::process::Child,
}

impl ChildWaitHandle {
    async fn wait(mut self) -> Result<std::process::ExitStatus, std::io::Error> {
        self.child.wait().await
    }
}

const BOOTSTRAP_SCRIPT: &str = include_str!("../../../scripts/bootstrap.sh");

/// Compute a hash that changes whenever any agent binary changes.
/// We hash all agent binaries we can find so the remote cache is invalidated
/// when any agent is rebuilt, regardless of which triple the remote needs.
fn agent_hash() -> Result<String, Error> {
    let mut hasher = blake3::Hasher::new();
    let mut found = false;

    // Collect paths from NEWT_AGENT_DIR and dist/agents/
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("NEWT_AGENT_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    dirs.push(PathBuf::from("dist/agents"));

    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path().join("newt-agent");
                if path.is_file() {
                    hasher.update(&std::fs::read(&path)?);
                    found = true;
                }
                // Also check flat layout (NEWT_AGENT_DIR/newt-agent)
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
/// Checks `NEWT_AGENT_DIR` env var first (for development), then `dist/agents/`.
fn find_agent_binary(triple: &str) -> Result<PathBuf, Error> {
    if let Ok(dir) = std::env::var("NEWT_AGENT_DIR") {
        let dir = PathBuf::from(dir);
        // Try triple-based layout: $NEWT_AGENT_DIR/<triple>/newt-agent
        let path = dir.join(triple).join("newt-agent");
        if path.exists() {
            return Ok(path);
        }
        // Try flat layout: $NEWT_AGENT_DIR/newt-agent (dev — e.g. target/debug/)
        let path = dir.join("newt-agent");
        if path.exists() {
            return Ok(path);
        }
    }

    // Bundled agents
    let path = PathBuf::from("dist/agents").join(triple).join("newt-agent");
    if path.exists() {
        return Ok(path);
    }

    Err(Error::Custom(format!(
        "agent binary not found for triple: {}. Set NEWT_AGENT_DIR to the directory containing the agent binary.",
        triple
    )))
}

/// Find the agent binary on the local machine (for elevated mode).
fn find_local_agent_binary() -> Result<PathBuf, Error> {
    // Dev: NEWT_AGENT_DIR with flat layout
    if let Ok(dir) = std::env::var("NEWT_AGENT_DIR") {
        let path = PathBuf::from(dir).join("newt-agent");
        if path.exists() {
            return Ok(path);
        }
    }

    // Next to the current executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("newt-agent");
            if path.exists() {
                return Ok(path);
            }
        }
    }

    Err(Error::Custom(
        "local agent binary not found. Set NEWT_AGENT_DIR to the directory containing the agent binary.".into(),
    ))
}

async fn create_remote_connection(
    transport_cmd: &[String],
    _publisher: &Arc<UpdatePublisher<MainWindowState>>,
) -> Result<
    (
        ChildWaitHandle,
        impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    ),
    Error,
> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (program, args) = transport_cmd
        .split_first()
        .ok_or_else(|| Error::Custom("empty transport command".into()))?;

    // Pass the bootstrap script as a `sh -c` argument so that stdin remains
    // free for the binary upload protocol (otherwise the shell buffers ahead
    // and eats the data meant for `read` inside the script).
    let script = BOOTSTRAP_SCRIPT.replace("__NEWT_HASH__", &agent_hash()?);
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
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // Read status line, skipping any noise from .bashrc etc.
    let mut reader = BufReader::new(stdout);
    let status_line = loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(Error::Custom(
                "remote connection closed before bootstrap completed".into(),
            ));
        }
        let trimmed = line.trim();
        if trimmed.starts_with("NEWT:") {
            break trimmed.to_string();
        }
        // Skip non-protocol lines (shell init noise)
        log::debug!("bootstrap noise: {}", trimmed);
    };
    let status_line = status_line.as_str();

    if status_line == "NEWT:READY" {
        // Agent is cached and valid, stdout is now the RPC stream
        let rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(reader);
        let tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
        let stream = tokio_duplex::Duplex::new(rx, tx);
        Ok((ChildWaitHandle { child }, stream))
    } else if let Some(triple) = status_line.strip_prefix("NEWT:NEED:") {
        // Need to upload the binary
        let binary_path = find_agent_binary(triple)?;
        let binary_data = tokio::fs::read(&binary_path).await?;
        let size = binary_data.len();

        // Write size line then binary data
        stdin.write_all(format!("{}\n", size).as_bytes()).await?;
        stdin.write_all(&binary_data).await?;
        stdin.flush().await?;

        // After upload, the script execs the agent — stdout becomes RPC stream
        let rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(reader);
        let tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
        let stream = tokio_duplex::Duplex::new(rx, tx);
        Ok((ChildWaitHandle { child }, stream))
    } else if let Some(error) = status_line.strip_prefix("NEWT:ERROR:") {
        Err(Error::Custom(format!("remote bootstrap error: {}", error)))
    } else {
        Err(Error::Custom(format!(
            "unexpected bootstrap response: {}",
            status_line
        )))
    }
}

struct MainWindowContextInner {
    fs: Arc<dyn Filesystem>,
    shell_service: Arc<dyn ShellService>,
    vfs_manager: Arc<dyn VfsManager>,
    terminal_client: Arc<dyn TerminalClient>,
    file_reader: Arc<dyn FileReader>,
    operations_client: Arc<dyn OperationsClient>,
    mounted_vfs: RwLock<HashMap<VfsId, MountedVfsInfo>>,
    next_operation_id: AtomicU64,

    window: WebviewWindow,
    main_window_state: MainWindowState,
    publisher: Arc<UpdatePublisher<MainWindowState>>
}

#[derive(Clone)]
pub struct MainWindowContext {
    inner: Arc<MainWindowContextInner>,
}

impl<'de> tauri::ipc::CommandArg<'de, Wry> for MainWindowContext {
    fn from_command(
        command: tauri::ipc::CommandItem<'de, Wry>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        let window = command.message.webview();
        let app_handle = window.app_handle();
        let s: State<GlobalContext> = app_handle.state();

        s.main_window(&window)
            .ok_or_else(|| tauri::ipc::InvokeError::from("window not yet initialized"))
    }
}

/// Apply an `OperationProgress` update to the operations state map
fn apply_operation_progress(operations: &Operations, progress: OperationProgress) {
    let mut ops = operations.0.write();
    match progress {
        OperationProgress::Prepared {
            id,
            total_bytes,
            total_items,
        } => {
            if let Some(op) = ops.get_mut(&id) {
                op.total_bytes = Some(total_bytes);
                op.total_items = Some(total_items);
                op.status = OperationStatus::Running;
            }
        }
        OperationProgress::Progress {
            id,
            bytes_done,
            items_done,
            current_item,
        } => {
            if let Some(op) = ops.get_mut(&id) {
                op.bytes_done = bytes_done;
                op.items_done = items_done;
                op.current_item = current_item;
                op.status = OperationStatus::Running;
                op.issue = None;
            }
        }
        OperationProgress::Completed { id } => {
            ops.remove(&id);
        }
        OperationProgress::Failed { id, error } => {
            if let Some(op) = ops.get_mut(&id) {
                op.status = OperationStatus::Failed;
                op.error = Some(error);
            }
        }
        OperationProgress::Cancelled { id } => {
            ops.remove(&id);
        }
        OperationProgress::Issue { id, issue } => {
            if let Some(op) = ops.get_mut(&id) {
                op.status = OperationStatus::WaitingForInput;
                op.issue = Some(OperationIssueInfo {
                    issue_id: issue.issue_id,
                    kind: format!("{:?}", issue.kind),
                    message: issue.message,
                    detail: issue.detail,
                    actions: issue
                        .actions
                        .iter()
                        .map(|a| match a {
                            newt_common::operation::IssueAction::Skip => "skip".to_string(),
                            newt_common::operation::IssueAction::Overwrite => {
                                "overwrite".to_string()
                            }
                            newt_common::operation::IssueAction::Retry => "retry".to_string(),
                            newt_common::operation::IssueAction::Abort => "abort".to_string(),
                        })
                        .collect(),
                });
            }
        }
    }
}

impl MainWindowContext {
    pub async fn create(
        window: WebviewWindow,
        connection_target: ConnectionTarget,
        window_title: String,
        init_channel: Option<&Channel<InitEvent>>,
    ) -> Result<Self, Error> {
        // Create state and publisher first
        let mut global_state = MainWindowState::new();
        global_state.window_title = window_title;
        let publisher = Arc::new(UpdatePublisher::new(
            window.clone(),
            "main_window",
            global_state.clone(),
        ));

        let (
            fs,
            shell_service,
            vfs_manager,
            terminal_client,
            file_reader,
            operations_client,
            communicator,
            initial_dir,
        ): (
            Arc<dyn Filesystem>,
            Arc<dyn ShellService>,
            Arc<dyn VfsManager>,
            Arc<dyn TerminalClient>,
            Arc<dyn FileReader>,
            Arc<dyn OperationsClient>,
            Option<Communicator>,
            VfsPath,
        ) = match &connection_target {
            ConnectionTarget::Local => {
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::unbounded_channel::<OperationProgress>();

                let registry = Arc::new(VfsRegistry::with_root(Arc::new(LocalVfs::new())));
                let op_context = Arc::new(OperationContext {
                    registry: registry.clone(),
                });
                let fs: Arc<dyn Filesystem> = Arc::new(VfsRegistryFs::new(registry.clone()));
                let shell_service: Arc<dyn ShellService> = Arc::new(LocalShellService);
                let vfs_manager: Arc<dyn VfsManager> =
                    Arc::new(VfsRegistryManager::new(registry.clone()));
                let terminal_client = Arc::new(newt_common::terminal::Local::new());
                let file_reader: Arc<dyn FileReader> =
                    Arc::new(VfsRegistryFileReader::new(registry.clone()));
                let operations_client: Arc<dyn OperationsClient> =
                    Arc::new(newt_common::operation::Local::new(progress_tx, op_context));

                // Spawn a task to forward local progress updates to the UI state
                let operations = global_state.operations.clone();
                let publisher_clone = publisher.clone();
                tokio::spawn(async move {
                    while let Some(progress) = progress_rx.recv().await {
                        apply_operation_progress(&operations, progress);
                        let _ = publisher_clone.publish();
                    }
                });

                let initial_dir = VfsPath::root(std::env::current_dir().unwrap());

                (
                    fs,
                    shell_service,
                    vfs_manager,
                    terminal_client,
                    file_reader,
                    operations_client,
                    None,
                    initial_dir,
                )
            }
            ConnectionTarget::Remote { transport_cmd } => {
                send_init_status(init_channel, "Connecting to remote host...");
                let (child, stream) = create_remote_connection(transport_cmd, &publisher).await?;

                let pending_streams: PendingStreams =
                    Arc::new(parking_lot::Mutex::new(HashMap::new()));

                let host_dispatcher = HostDispatcher {
                    operations: global_state.operations.clone(),
                    publisher: publisher.clone(),
                    pending_streams: pending_streams.clone(),
                };
                let communicator = Communicator::with_dispatcher(host_dispatcher, stream);

                let fs = Arc::new(newt_common::filesystem::Remote::new_with_streams(
                    communicator.clone(),
                    pending_streams,
                ));
                let shell_service: Arc<dyn ShellService> =
                    Arc::new(ShellRemote::new(communicator.clone()));
                let vfs_manager: Arc<dyn VfsManager> =
                    Arc::new(VfsManagerRemote::new(communicator.clone()));
                let terminal_client =
                    Arc::new(newt_common::terminal::Remote::new(communicator.clone()));
                let file_reader: Arc<dyn FileReader> =
                    Arc::new(newt_common::file_reader::Remote::new(communicator.clone()));
                let operations_client: Arc<dyn OperationsClient> =
                    Arc::new(newt_common::operation::Remote::new(communicator.clone()));

                tokio::spawn(async move {
                    let ret = child.wait().await.unwrap();
                    eprintln!("child exited: {}", ret);
                });

                // For remote, resolve home directory
                let _initial_dir = shell_service
                    .shell_expand("~".to_string())
                    .await
                    .unwrap_or_else(|_| VfsPath::root("/"));

                let initial_dir = VfsPath::root("/");

                (
                    fs,
                    shell_service,
                    vfs_manager,
                    terminal_client,
                    file_reader,
                    operations_client,
                    Some(communicator),
                    initial_dir,
                )
            }
            ConnectionTarget::Elevated => {
                if cfg!(not(target_os = "linux")) {
                    return Err(Error::Custom(
                        "elevated mode is not supported on this platform".into(),
                    ));
                }

                send_init_status(init_channel, "Waiting for authorization...");
                let agent_path = find_local_agent_binary()?;
                let mut cmd = tokio::process::Command::new("pkexec");
                cmd.arg(&agent_path)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit());
                if let Ok(rust_log) = std::env::var("RUST_LOG") {
                    cmd.env("RUST_LOG", rust_log);
                }
                let mut child = cmd.spawn()?;

                let stdin = child.stdin.take().unwrap();
                let stdout = child.stdout.take().unwrap();

                let rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(stdout);
                let tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
                let stream = tokio_duplex::Duplex::new(rx, tx);

                let pending_streams: PendingStreams =
                    Arc::new(parking_lot::Mutex::new(HashMap::new()));

                let host_dispatcher = HostDispatcher {
                    operations: global_state.operations.clone(),
                    publisher: publisher.clone(),
                    pending_streams: pending_streams.clone(),
                };
                let communicator = Communicator::with_dispatcher(host_dispatcher, stream);

                let fs = Arc::new(newt_common::filesystem::Remote::new_with_streams(
                    communicator.clone(),
                    pending_streams,
                ));
                let shell_service: Arc<dyn ShellService> =
                    Arc::new(ShellRemote::new(communicator.clone()));
                let vfs_manager: Arc<dyn VfsManager> =
                    Arc::new(VfsManagerRemote::new(communicator.clone()));
                let terminal_client =
                    Arc::new(newt_common::terminal::Remote::new(communicator.clone()));
                let file_reader: Arc<dyn FileReader> =
                    Arc::new(newt_common::file_reader::Remote::new(communicator.clone()));
                let operations_client: Arc<dyn OperationsClient> =
                    Arc::new(newt_common::operation::Remote::new(communicator.clone()));

                tokio::spawn(async move {
                    let ret = child.wait().await.unwrap();
                    eprintln!("elevated agent exited: {}", ret);
                });

                let initial_dir = shell_service
                    .shell_expand("~".to_string())
                    .await
                    .unwrap_or_else(|_| VfsPath::root("/"));

                (
                    fs,
                    shell_service,
                    vfs_manager,
                    terminal_client,
                    file_reader,
                    operations_client,
                    Some(communicator),
                    initial_dir,
                )
            }
        };

        global_state.panes.add(Pane::new(
            fs.clone(),
            initial_dir.clone(),
            global_state.display_options.clone(),
            publisher.clone(),
        ));
        global_state.panes.add(Pane::new(
            fs.clone(),
            initial_dir,
            global_state.display_options.clone(),
            publisher.clone(),
        ));
        send_init_status(init_channel, "Loading...");
        global_state.refresh().await?;

        for pane in global_state.panes.all() {
            tauri::async_runtime::spawn(async move {
                pane.watch_changes().await;
            });
        }

        // Pre-populate mounted_vfs with the ROOT entry
        let mut initial_mounted = HashMap::new();
        initial_mounted.insert(
            VfsId::ROOT,
            MountedVfsInfo {
                vfs_id: VfsId::ROOT,
                descriptor: &LOCAL_VFS_DESCRIPTOR,
                mount_meta: Vec::new(),
            },
        );

        Ok(Self {
            inner: Arc::new(MainWindowContextInner {
                fs,
                shell_service,
                vfs_manager,
                terminal_client,
                file_reader,
                operations_client,
                mounted_vfs: RwLock::new(initial_mounted),
                next_operation_id: AtomicU64::new(1),
                window,
                publisher,
                main_window_state: global_state,
            }),
        })
    }

    pub fn fs(&self) -> Arc<dyn Filesystem> {
        self.inner.fs.clone()
    }

    pub fn shell_service(&self) -> Arc<dyn ShellService> {
        self.inner.shell_service.clone()
    }

    pub fn terminal_client(&self) -> Arc<dyn TerminalClient> {
        self.inner.terminal_client.clone()
    }

    pub fn file_reader(&self) -> Arc<dyn FileReader> {
        self.inner.file_reader.clone()
    }

    pub fn window(&self) -> WebviewWindow {
        self.inner.window.clone()
    }

    pub fn with_update<T>(
        &self,
        f: impl FnOnce(&MainWindowState) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let ret = f(&self.inner.main_window_state);
        self.inner.publisher.publish()?;
        ret
    }

    pub async fn with_update_async<T, F, Fut>(&self, f: F) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
        F: FnOnce(MainWindowState) -> Fut,
    {
        let ret = f(self.inner.main_window_state.clone()).await;
        self.inner.publisher.publish()?;
        ret
    }

    pub fn with_pane_update<T>(
        &self,
        pane_handle: PaneHandle,
        f: impl FnOnce(&MainWindowState, &Pane) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.with_update(|s| {
            let pane = s.panes.get(pane_handle).unwrap();
            f(s, &pane)
        })
    }

    pub async fn with_pane_update_async<T, F, Fut>(
        &self,
        pane_handle: PaneHandle,
        f: F,
    ) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
        F: FnOnce(MainWindowState, Arc<Pane>) -> Fut,
    {
        self.with_update_async(|s| {
            let pane = s.panes.get(pane_handle).unwrap();
            async move { f(s, pane).await }
        })
        .await
    }

    pub fn panes(&self) -> &Panes {
        &self.inner.main_window_state.panes
    }

    pub fn active_pane(&self) -> Option<Arc<Pane>> {
        self.inner.main_window_state.panes.get(
            self.inner
                .main_window_state
                .display_options
                .0
                .read()
                .active_pane,
        )
    }

    pub fn active_terminal(&self) -> Option<Arc<Terminal>> {
        self.inner
            .main_window_state
            .display_options
            .0
            .read()
            .active_terminal
            .and_then(|handle| self.inner.main_window_state.terminals.get(handle))
    }

    pub fn terminals(&self) -> &Terminals {
        &self.inner.main_window_state.terminals
    }

    pub async fn create_terminal(&self, path: Option<&Path>) -> Result<Arc<Terminal>, Error> {
        let terminal = Terminal::create(self.clone(), self.inner.window.clone(), path).await?;

        self.with_update(|s| {
            let terminal = s.terminals.insert(terminal.handle, terminal);
            let mut opts = s.display_options.0.write();
            opts.active_terminal = Some(terminal.handle);
            opts.panes_focused = false;
            Ok(terminal)
        })
    }

    pub fn operations_client(&self) -> Arc<dyn OperationsClient> {
        self.inner.operations_client.clone()
    }

    pub fn next_operation_id(&self) -> OperationId {
        self.inner.next_operation_id.fetch_add(1, Ordering::SeqCst)
    }

    pub fn operations(&self) -> &Operations {
        &self.inner.main_window_state.operations
    }

    pub fn publish_full(&self) -> Result<(), Error> {
        self.inner.publisher.publish_full()
    }

    pub fn publish(&self) -> Result<(), Error> {
        self.inner.publisher.publish()
    }

    pub async fn mount_vfs(
        &self,
        request: newt_common::vfs::MountRequest,
    ) -> Result<newt_common::vfs::MountResponse, Error> {
        let response = self.inner.vfs_manager.mount(request).await?;
        let descriptor = lookup_descriptor(&response.type_name)
            .ok_or_else(|| Error::Custom(format!("unknown VFS type: {}", response.type_name)))?;
        self.inner.mounted_vfs.write().insert(
            response.vfs_id,
            MountedVfsInfo {
                vfs_id: response.vfs_id,
                descriptor,
                mount_meta: response.mount_meta.clone(),
            },
        );
        Ok(response)
    }

    pub async fn unmount_vfs(&self, vfs_id: VfsId) -> Result<(), Error> {
        self.inner.vfs_manager.unmount(vfs_id).await?;
        self.inner.mounted_vfs.write().remove(&vfs_id);
        Ok(())
    }

    pub fn vfs_descriptor(
        &self,
        vfs_id: VfsId,
    ) -> Option<&'static dyn newt_common::vfs::VfsDescriptor> {
        self.inner
            .mounted_vfs
            .read()
            .get(&vfs_id)
            .map(|info| info.descriptor)
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        self.with_update_async(|gs| async move {
            gs.refresh().await?;
            Ok(())
        })
        .await?;
        Ok(())
    }
}
