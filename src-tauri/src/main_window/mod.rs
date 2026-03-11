pub mod pane;
pub mod session;
pub mod terminal;

use newt_common::file_reader::FileReader;
use newt_common::filesystem::{Filesystem, ShellService, UserGroup};
use newt_common::operation::{OperationId, OperationProgress, OperationsClient};
use newt_common::terminal::TerminalClient;
use newt_common::terminal::TerminalHandle;
use newt_common::vfs::{MountedVfsInfo, VfsId, VfsPath, all_descriptors, lookup_descriptor};
use parking_lot::{RwLock, RwLockWriteGuard};
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use std::cmp::PartialOrd;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;
use tauri::WebviewWindow;
use tauri::Wry;

use crate::GlobalContext;
use crate::common::Error;
use crate::common::UpdatePublisher;

use self::pane::Pane;
use self::session::Session;
use self::terminal::Terminal;

pub use self::session::{AgentResolver, ConnectionState, ConnectionStatus, ConnectionTarget};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayOptionsInner {
    pub show_hidden: bool,
    pub active_pane: PaneHandle,
    pub active_terminal: Option<TerminalHandle>,
    pub panes_focused: bool,
    pub terminal_panel_visible: bool,
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
            terminal_panel_visible: false,
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

impl PaneHandle {
    pub fn left() -> Self {
        PaneHandle(0)
    }

    pub fn right() -> Self {
        PaneHandle(1)
    }
}

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

    pub fn is_empty(&self) -> bool {
        self.0.read().is_empty()
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

    pub fn first_handle(&self) -> Option<TerminalHandle> {
        self.0.read().keys().copied().min_by_key(|h| h.0)
    }

    pub fn handles_sorted(&self) -> Vec<TerminalHandle> {
        let mut handles: Vec<_> = self.0.read().keys().copied().collect();
        handles.sort_by_key(|h| h.0);
        handles
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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfirmAction {
    DeleteSelected { paths: Vec<VfsPath> },
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ModalDataKind {
    CreateDirectory {
        path: VfsPath,
    },
    CreateFile {
        path: VfsPath,
        open_editor: bool,
    },
    Properties {
        paths: Vec<VfsPath>,
        name: String,
        size: Option<u64>,
        is_dir: bool,
        is_symlink: bool,
        symlink_target: Option<String>,
        mode: Option<u32>,
        owner: Option<UserGroup>,
        group: Option<UserGroup>,
        modified: Option<i128>,
        accessed: Option<i128>,
        created: Option<i128>,
    },
    Navigate {
        path: VfsPath,
        display_path: String,
    },
    Rename {
        base_path: VfsPath,
        name: String,
    },
    CopyMove {
        kind: String,
        sources: Vec<VfsPath>,
        destination: VfsPath,
        display_destination: String,
        summary: String,
    },
    ConnectRemote {
        host: String,
    },
    MountSftp {
        host: String,
    },
    SelectVfs {
        targets: Vec<VfsTarget>,
    },
    CommandPalette {
        #[serde(skip_serializing_if = "Option::is_none")]
        category_filter: Option<String>,
    },
    HotPaths,
    Settings,
    Confirm {
        message: String,
        action: ConfirmAction,
    },
    UserCommandInput {
        command_index: usize,
        command_title: String,
        prompts: Vec<UserCommandPrompt>,
        confirms: Vec<String>,
    },
    Debug,
    ConnectionLog,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct UserCommandPrompt {
    pub label: String,
    pub default: String,
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

#[derive(Clone, serde::Serialize)]
pub struct VfsTarget {
    pub vfs_id: Option<VfsId>,
    pub type_name: String,
    pub display_name: String,
    /// Human-readable label for a mounted instance (e.g. hostname for SFTP).
    pub label: Option<String>,
    /// Dialog to open when user selects this unmounted VFS type.
    /// If None and vfs_id is None, the type supports auto-mount.
    pub mount_dialog: Option<String>,
}

// ---------------------------------------------------------------------------
// Askpass — SSH password / host-key prompts via SSH_ASKPASS
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize)]
pub struct AskpassPrompt {
    pub prompt: String,
    pub is_secret: bool,
}

#[derive(Clone)]
pub struct AskpassState(pub Arc<RwLock<Option<AskpassPrompt>>>);

impl Default for AskpassState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(None)))
    }
}

impl serde::Serialize for AskpassState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(Clone)]
pub struct MainWindowState {
    pub connection_status: ConnectionState,
    pub askpass: AskpassState,
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
        let mut s = serializer.serialize_struct("MainWindowState", 10)?;
        s.serialize_field("connection_status", &self.connection_status)?;
        s.serialize_field("askpass", &self.askpass)?;
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
            connection_status: ConnectionState::default(),
            askpass: AskpassState::default(),
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

    pub async fn as_other_pane(&self, handle: PaneHandle) -> Result<(), Error> {
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

/// Apply an `OperationProgress` update to the operations state map.
/// Used by both local progress forwarding and remote RPC notifications.
pub(crate) fn apply_operation_progress(operations: &Operations, progress: OperationProgress) {
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

// ---------------------------------------------------------------------------
// MainWindowContext
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub enum MainWindowEvent {
    /// A pane navigated — check for stale archive mounts.
    PaneNavigated,
}

#[allow(dead_code)]
struct MainWindowContextInner {
    window: WebviewWindow,
    main_window_state: MainWindowState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
    preferences: crate::preferences::PreferencesHandle,
    connection_target: ConnectionTarget,
    window_title: String,
    session: Arc<arc_swap::ArcSwap<Option<Session>>>,
    askpass_response: Arc<parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
    clipboard: RwLock<arboard::Clipboard>,
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

impl MainWindowContext {
    pub fn new(
        window: WebviewWindow,
        connection_target: ConnectionTarget,
        window_title: String,
        preferences: crate::preferences::PreferencesHandle,
    ) -> Self {
        let mut global_state = MainWindowState::new();
        global_state.window_title = window_title.clone();
        global_state.display_options.0.write().show_hidden =
            preferences.load().appearance.show_hidden;
        let publisher = Arc::new(UpdatePublisher::new(
            window.clone(),
            "main_window",
            global_state.clone(),
        ));

        Self {
            inner: Arc::new(MainWindowContextInner {
                window,
                main_window_state: global_state,
                publisher,
                preferences,
                connection_target,
                window_title,
                session: Arc::new(arc_swap::ArcSwap::from_pointee(None)),
                askpass_response: Arc::new(parking_lot::Mutex::new(None)),
                clipboard: RwLock::new(
                    arboard::Clipboard::new().expect("failed to initialize clipboard"),
                ),
            }),
        }
    }

    pub async fn connect(&self, agent_resolver: &AgentResolver) -> Result<(), Error> {
        let state = &self.inner.main_window_state;
        let publisher = &self.inner.publisher;
        let session_slot = &self.inner.session;
        let askpass_state = &state.askpass;
        let askpass_response_slot = &self.inner.askpass_response;

        let askpass_callback = {
            let askpass_state = askpass_state.clone();
            let askpass_response_slot = askpass_response_slot.clone();
            let publisher = publisher.clone();
            move |prompt: String, is_secret: bool| {
                let askpass_state = askpass_state.clone();
                let askpass_response_slot = askpass_response_slot.clone();
                let publisher = publisher.clone();
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    *askpass_state.0.write() = Some(AskpassPrompt { prompt, is_secret });
                    *askpass_response_slot.lock() = Some(tx);
                    let _ = publisher.publish();

                    let result = rx.await.unwrap_or(None);

                    *askpass_state.0.write() = None;
                    *askpass_response_slot.lock() = None;
                    let _ = publisher.publish();

                    result
                })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
            }
        };

        session::connect(
            &self.inner.connection_target,
            agent_resolver,
            state,
            publisher,
            self.inner.preferences.clone(),
            session_slot,
            |msg| {
                state.connection_status.set_connecting(msg);
                let _ = publisher.publish();
            },
            askpass_callback,
            self.clone(),
        )
        .await
    }

    pub fn askpass_respond(&self, response: Option<String>) {
        if let Some(tx) = self.inner.askpass_response.lock().take() {
            let _ = tx.send(response);
        }
    }

    fn with_session<T>(&self, f: impl FnOnce(&Session) -> T) -> Result<T, Error> {
        let guard = self.inner.session.load();
        let opt: &Option<Session> = &guard;
        opt.as_ref()
            .ok_or_else(|| Error::Custom("not connected".into()))
            .map(f)
    }

    pub fn connection_target(&self) -> &ConnectionTarget {
        &self.inner.connection_target
    }

    pub fn window_title(&self) -> &str {
        &self.inner.window_title
    }

    pub fn is_connected(&self) -> bool {
        let guard = self.inner.session.load();
        let opt: &Option<Session> = &guard;
        opt.is_some()
    }

    pub fn set_connection_failed(&self, error: String) {
        self.inner
            .main_window_state
            .connection_status
            .set_failed(error);
        let _ = self.inner.publisher.publish();
    }

    pub fn fs(&self) -> Result<Arc<dyn Filesystem>, Error> {
        self.with_session(|s| s.fs.clone())
    }

    pub fn shell_service(&self) -> Result<Arc<dyn ShellService>, Error> {
        self.with_session(|s| s.shell_service.clone())
    }

    pub fn terminal_client(&self) -> Result<Arc<dyn TerminalClient>, Error> {
        self.with_session(|s| s.terminal_client.clone())
    }

    pub fn file_reader(&self) -> Result<Arc<dyn FileReader>, Error> {
        self.with_session(|s| s.file_reader.clone())
    }

    pub fn hot_paths_provider(
        &self,
    ) -> Result<Arc<dyn newt_common::hot_paths::HotPathsProvider>, Error> {
        self.with_session(|s| s.hot_paths_provider.clone())
    }

    pub fn file_server_base_url(&self) -> Result<String, Error> {
        self.with_session(|s| {
            format!(
                "http://localhost:{}/{}",
                s.file_server_port, s.file_server_token
            )
        })
    }

    pub fn window(&self) -> WebviewWindow {
        self.inner.window.clone()
    }

    pub fn preferences(&self) -> &crate::preferences::PreferencesHandle {
        &self.inner.preferences
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
            opts.terminal_panel_visible = true;
            Ok(terminal)
        })
    }

    pub fn operations_client(&self) -> Result<Arc<dyn OperationsClient>, Error> {
        self.with_session(|s| s.operations_client.clone())
    }

    pub fn next_operation_id(&self) -> Result<OperationId, Error> {
        self.with_session(|s| s.next_operation_id.fetch_add(1, Ordering::SeqCst))
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

    pub fn compute_vfs_targets(&self) -> Result<Vec<VfsTarget>, Error> {
        /// Maps VFS type_name → dialog name for types that need user input to mount.
        fn mount_dialog_for(type_name: &str) -> Option<&'static str> {
            match type_name {
                "sftp" => Some("mount_sftp"),
                _ => None,
            }
        }

        let mounted_vfs = self.with_session(|s| s.mounted_vfs.clone())?;
        let mounted = mounted_vfs.read();
        let mut targets = Vec::new();

        for (vfs_id, info) in mounted.iter() {
            targets.push(VfsTarget {
                vfs_id: Some(*vfs_id),
                type_name: info.descriptor.type_name().to_string(),
                display_name: info.descriptor.display_name().to_string(),
                label: info.descriptor.mount_label(&info.mount_meta),
                mount_dialog: None,
            });
        }

        let mounted_types: std::collections::HashSet<&str> = mounted
            .values()
            .map(|info| info.descriptor.type_name())
            .collect();

        for desc in all_descriptors() {
            if mounted_types.contains(desc.type_name()) {
                continue;
            }
            let mount_dialog = mount_dialog_for(desc.type_name()).map(|s| s.to_string());
            if desc.auto_mount_request().is_some() || mount_dialog.is_some() {
                targets.push(VfsTarget {
                    vfs_id: None,
                    type_name: desc.type_name().to_string(),
                    display_name: desc.display_name().to_string(),
                    label: None,
                    mount_dialog,
                });
            }
        }

        targets.sort_by(|a, b| match (a.vfs_id, b.vfs_id) {
            (Some(id_a), Some(id_b)) => id_a.cmp(&id_b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.type_name.cmp(&b.type_name),
        });

        Ok(targets)
    }

    pub async fn mount_vfs(
        &self,
        request: newt_common::vfs::MountRequest,
    ) -> Result<newt_common::vfs::MountResponse, Error> {
        let vfs_manager = self.with_session(|s| s.vfs_manager.clone())?;
        let response = vfs_manager.mount(request).await?;
        let descriptor = lookup_descriptor(&response.type_name)
            .ok_or_else(|| Error::Custom(format!("unknown VFS type: {}", response.type_name)))?;
        self.with_session(|s| {
            s.mounted_vfs.write().insert(
                response.vfs_id,
                MountedVfsInfo {
                    vfs_id: response.vfs_id,
                    descriptor,
                    mount_meta: response.mount_meta.clone(),
                    origin: response.origin.clone(),
                },
            );
        })?;
        Ok(response)
    }

    pub async fn unmount_vfs(&self, vfs_id: VfsId) -> Result<(), Error> {
        let vfs_manager = self.with_session(|s| s.vfs_manager.clone())?;
        vfs_manager.unmount(vfs_id).await?;
        self.with_session(|s| {
            s.mounted_vfs.write().remove(&vfs_id);
        })?;
        Ok(())
    }

    pub fn resolve_display_path(&self, input: &str) -> Option<VfsPath> {
        self.with_session(|s| {
            for (vfs_id, info) in s.mounted_vfs.read().iter() {
                if let Some(internal_path) = info
                    .descriptor
                    .try_parse_display_path(input, &info.mount_meta)
                {
                    return Some(VfsPath::new(*vfs_id, internal_path));
                }
            }
            None
        })
        .ok()
        .flatten()
    }

    pub fn format_vfs_path(&self, vfs_path: &VfsPath) -> String {
        self.with_session(|s| {
            s.mounted_vfs.read().get(&vfs_path.vfs_id).map(|info| {
                info.descriptor
                    .format_path(&vfs_path.path, &info.mount_meta)
            })
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| vfs_path.to_string())
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        self.with_update_async(|gs| async move {
            gs.refresh().await?;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(super) async fn cleanup_stale_archive_mounts(&self) -> Result<(), Error> {
        // Collect VFS IDs currently in use by any pane
        let pane_vfs_ids: std::collections::HashSet<VfsId> =
            self.panes().all().iter().map(|p| p.path().vfs_id).collect();

        // A VFS is "in use" if a pane references it, or if another in-use VFS
        // has it as its origin (transitively). This prevents unmounting a parent
        // archive when a nested child archive is still open.
        let stale_ids: Vec<VfsId> = self.with_session(|s| {
            let mounted = s.mounted_vfs.read();

            // Expand pane VFS IDs to include all transitive origins
            let mut in_use = pane_vfs_ids.clone();
            let mut queue: Vec<VfsId> = pane_vfs_ids.into_iter().collect();
            while let Some(vfs_id) = queue.pop() {
                if let Some(info) = mounted.get(&vfs_id)
                    && let Some(ref origin) = info.origin
                    && in_use.insert(origin.vfs_id)
                {
                    queue.push(origin.vfs_id);
                }
            }

            mounted
                .iter()
                .filter(|(id, info)| info.origin.is_some() && !in_use.contains(id))
                .map(|(id, _)| *id)
                .collect()
        })?;

        for vfs_id in stale_ids {
            log::info!("unmounting stale archive VFS {:?}", vfs_id);
            self.unmount_vfs(vfs_id).await?;
        }

        Ok(())
    }

    pub fn clipboard(&self) -> RwLockWriteGuard<'_, arboard::Clipboard> {
        self.inner.clipboard.write()
    }
}
