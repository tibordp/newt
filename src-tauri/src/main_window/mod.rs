pub mod pane;
pub mod terminal;

use newt_common::filesystem::Filesystem;
use newt_common::filesystem::Local;
use parking_lot::RwLock;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use std::collections::HashMap;

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;

use std::sync::Arc;
use tauri::Manager;
use tauri::State;
use tauri::Window;
use tauri::Wry;

use crate::common::UpdatePublisher;
use crate::common::Error;
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

#[derive(
    PartialEq, Eq, Hash, PartialOrd, Ord, Clone, Copy, serde::Serialize, serde::Deserialize,
)]
pub struct TerminalHandle(uuid::Uuid);

impl TerminalHandle {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Clone)]
pub struct Terminals(Arc<RwLock<HashMap<TerminalHandle, Arc<Terminal>>>>);

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
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ModalDataKind {
    CreateDirectory { path: PathBuf },
    Navigate { path: PathBuf },
    Rename { base_path: PathBuf, name: String },
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
            window_title: "File Manager".to_string(),
        }
    }

    fn other_pane(&self, handle: PaneHandle) -> Arc<Pane> {
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
struct MainWindowContextInner {
    fs: Arc<dyn Filesystem>,
    window: Window,
    main_window_state: MainWindowState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
}

#[derive(Clone)]
pub struct MainWindowContext {
    inner: Arc<MainWindowContextInner>,
}

impl<'de> tauri::command::CommandArg<'de, Wry> for MainWindowContext {
    fn from_command(
        command: tauri::command::CommandItem<'de, Wry>,
    ) -> Result<Self, tauri::InvokeError> {
        let window = command.message.window();
        let app_handle = window.app_handle();
        let s: State<GlobalContext> = app_handle.state();

        Ok(s.main_window(&window).expect("window not found"))
    }
}

impl MainWindowContext {
    pub async fn create(window: Window) -> Result<Self, Error> {
        let fs = Local::new();
        //let fs = Slow::new(fs);

        let fs = Arc::new(fs);
        let global_state = MainWindowState::new();

        let publisher = Arc::new(UpdatePublisher::new(
            window.clone(),
            "main_window",
            global_state.clone(),
        ));

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
                window,
                publisher,
                main_window_state: global_state,
            }),
        })
    }

    pub fn fs(&self) -> Arc<dyn Filesystem> {
        self.inner.fs.clone()
    }

    pub fn window(&self) -> Window {
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
                .set_title(&format!("{} - newt", pane.path().display()))
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
                .set_title(&format!("{} - newt", pane.path().display()))
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
        let handle = TerminalHandle::new();
        let terminal =
            Terminal::create(self.clone(), self.inner.window.clone(), handle, path).await?;

        self.with_update(|s| {
            let terminal = s.terminals.insert(handle, terminal);
            let mut opts = s.display_options.0.write();
            opts.active_terminal = Some(handle);
            opts.panes_focused = false;
            Ok(terminal)
        })
    }

    pub fn publish_full(&self) -> Result<(), Error> {
        self.inner.publisher.publish_full()
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        self.with_update_async(|gs| async move {
            gs.refresh().await?;
            Ok(())
        }).await?;
        Ok(())
    }
}
