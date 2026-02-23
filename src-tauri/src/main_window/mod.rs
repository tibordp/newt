pub mod pane;
pub mod terminal;

use async_compression::tokio::bufread::ZstdDecoder;
use async_compression::tokio::write::ZstdEncoder;
use newt_common::api::API_OPERATION_PROGRESS;
use newt_common::filesystem::Filesystem;
use newt_common::filesystem::Remote;
use newt_common::filesystem::UserGroup;
use newt_common::operation::{OperationId, OperationProgress};
use newt_common::rpc::Communicator;

use newt_common::terminal::TerminalClient;
use newt_common::terminal::TerminalHandle;
use parking_lot::RwLock;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;

use std::process::Stdio;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;
use tauri::WebviewWindow;
use tauri::Wry;

use crate::common::Error;
use crate::common::UpdatePublisher;
use crate::GlobalContext;

use self::pane::Pane;
use self::terminal::Terminal;

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
        path: PathBuf,
    },
    CreateFile {
        path: PathBuf,
    },
    Properties {
        paths: Vec<PathBuf>,

        mode: Option<u32>,
        owner: Option<UserGroup>,
        group: Option<UserGroup>,
    },
    Navigate {
        path: PathBuf,
    },
    Rename {
        base_path: PathBuf,
        name: String,
    },
    CopyMove {
        kind: String,
        sources: Vec<PathBuf>,
        destination: PathBuf,
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

#[derive(Clone, serde::Serialize)]
pub struct MainWindowState {
    pub panes: Panes,
    pub terminals: Terminals,
    pub modal: ModalState,
    pub display_options: DisplayOptions,
    pub operations: Operations,
    pub window_title: String,
}

impl MainWindowState {
    fn new() -> Self {
        let display_options = DisplayOptions::default();

        Self {
            panes: Panes::new(),
            terminals: Terminals::new(),
            modal: ModalState::default(),
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

        pane.navigate(other_pane.path()).await?;

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

            {
                let mut ops = self.operations.0.write();
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
                                        newt_common::operation::IssueAction::Skip => {
                                            "skip".to_string()
                                        }
                                        newt_common::operation::IssueAction::Overwrite => {
                                            "overwrite".to_string()
                                        }
                                        newt_common::operation::IssueAction::Retry => {
                                            "retry".to_string()
                                        }
                                        newt_common::operation::IssueAction::Abort => {
                                            "abort".to_string()
                                        }
                                    })
                                    .collect(),
                            });
                        }
                    }
                }
            }

            let _ = self.publisher.publish();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

struct MainWindowContextInner {
    fs: Arc<dyn Filesystem>,
    terminal_client: Arc<dyn TerminalClient>,
    communicator: Communicator,
    next_operation_id: AtomicU64,

    window: WebviewWindow,
    main_window_state: MainWindowState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
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

        Ok(s.main_window(&window).expect("window not found"))
    }
}

impl MainWindowContext {
    pub async fn create(window: WebviewWindow) -> Result<Self, Error> {
        /*       let mut child = tokio::process::Command::new("/usr/bin/ssh")
                            .args(&[
                                "192.168.100.177",
                                "sh -c 'truss /usr/home/tibordp/src/newt/target/debug/newt-agent 2>~/truss.out; echo done >&2'",
                            ])
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::inherit())
                            .spawn()?;
        */
        let mut child = tokio::process::Command::new("/bin/env")
            .args(["/home/tibordp/src/newt/target/debug/newt-agent"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let mut rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(child.stdout.take().unwrap());
        let mut tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(child.stdin.take().unwrap());

        if false {
            rx = Box::new(ZstdDecoder::new(tokio::io::BufReader::new(rx)));
            tx = Box::new(ZstdEncoder::new(tx));
        }

        let stream = tokio_duplex::Duplex::new(rx, tx);

        // Create state and publisher first, so HostDispatcher can reference them
        let global_state = MainWindowState::new();
        let publisher = Arc::new(UpdatePublisher::new(
            window.clone(),
            "main_window",
            global_state.clone(),
        ));

        let host_dispatcher = HostDispatcher {
            operations: global_state.operations.clone(),
            publisher: publisher.clone(),
        };
        let communicator = Communicator::with_dispatcher(host_dispatcher, stream);
        let fs = Remote::new(communicator.clone());
        let terminal_client = newt_common::terminal::Remote::new(communicator.clone());

        tokio::spawn(async move {
            let ret = child.wait().await.unwrap();
            eprintln!("child exited: {}", ret);
        });

        let fs = Arc::new(fs);
        let terminal_client = Arc::new(terminal_client);

        global_state.panes.add(Pane::new(
            fs.clone(),
            std::env::current_dir().unwrap(),
            global_state.display_options.clone(),
            publisher.clone(),
        ));
        global_state.panes.add(Pane::new(
            fs.clone(),
            std::env::current_dir().unwrap(),
            global_state.display_options.clone(),
            publisher.clone(),
        ));
        global_state.refresh().await?;

        for pane in global_state.panes.all() {
            tauri::async_runtime::spawn(async move {
                pane.watch_changes().await;
            });
        }

        Ok(Self {
            inner: Arc::new(MainWindowContextInner {
                fs,
                terminal_client,
                communicator,
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

    pub fn terminal_client(&self) -> Arc<dyn TerminalClient> {
        self.inner.terminal_client.clone()
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

        if let Some(pane) = self.active_pane() {
            self.inner
                .window
                .set_title(&format!("{} - Newt", pane.path().display()))
                .unwrap();
        }

        ret
    }

    pub async fn with_update_async<T, F, Fut>(&self, f: F) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
        F: FnOnce(MainWindowState) -> Fut,
    {
        let ret = f(self.inner.main_window_state.clone()).await;

        self.inner.publisher.publish()?;
        if let Some(pane) = self.active_pane() {
            self.inner
                .window
                .set_title(&format!("{} - Newt", pane.path().display()))
                .unwrap();
        }

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

    pub fn communicator(&self) -> &Communicator {
        &self.inner.communicator
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

    pub async fn refresh(&self) -> Result<(), Error> {
        self.with_update_async(|gs| async move {
            gs.refresh().await?;
            Ok(())
        })
        .await?;
        Ok(())
    }
}
