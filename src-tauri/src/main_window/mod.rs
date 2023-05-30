pub mod pane;

use notify::RecommendedWatcher;
use notify::RecursiveMode;
use parking_lot::Mutex;
use parking_lot::RwLock;
use serde::ser::SerializeSeq;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;
use tauri::Window;
use tauri::Wry;

use crate::common::diff;
use crate::common::Error;
use crate::common::PatchOperation;
use crate::GlobalContext;

use self::pane::Pane;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisplayOptionsInner {
    show_hidden: bool,
}

#[derive(Default, Clone)]
pub struct DisplayOptions(Arc<RwLock<DisplayOptionsInner>>);

impl serde::Serialize for DisplayOptions {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(
    PartialEq, Eq, Hash, PartialOrd, Ord, Clone, Copy, serde::Serialize, serde::Deserialize,
)]
pub struct PaneHandle(usize);

#[derive(Clone)]
pub struct Panes(Vec<Arc<Pane>>);

impl Panes {
    pub fn new(inner: impl IntoIterator<Item = Pane>) -> Self {
        Self(inner.into_iter().map(Arc::new).collect())
    }

    pub fn get(&self, handle: PaneHandle) -> Option<Arc<Pane>> {
        self.0.get(handle.0).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = (PaneHandle, Arc<Pane>)> + '_ {
        self.0
            .iter()
            .enumerate()
            .map(|(i, p)| (PaneHandle(i), p.clone()))
    }
}

impl serde::Serialize for Panes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for e in self.0.iter() {
            seq.serialize_element(&**e)?;
        }
        seq.end()
    }
}

#[derive(Clone, serde::Serialize)]
pub struct MainWindowState {
    pub panes: Panes,
    pub display_options: DisplayOptions,
    pub window_title: String,
}

impl MainWindowState {
    fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let display_options = DisplayOptions::default();

        Self {
            panes: Panes::new(paths.into_iter().map(|path| {
                let path = PathBuf::from(path);
                Pane::new(path, display_options.clone())
            })),
            display_options,
            window_title: "File Manager".to_string(),
        }
    }

    fn other_pane(&self, handle: PaneHandle) -> Arc<Pane> {
        assert!(self.panes.0.len() == 2);

        self.panes.get(PaneHandle(1 - handle.0)).unwrap()
    }

    pub fn refresh(&self) -> Result<(), Error> {
        for (_, pane) in self.panes.iter() {
            pane.refresh()?;
        }
        Ok(())
    }

    pub fn activate_pane(&self, handle: PaneHandle) {
        for (h, pane) in self.panes.iter() {
            let mut view_state = pane.view_state_mut();
            view_state.active = h == handle;
        }
    }

    pub fn copy_pane(&self, handle: PaneHandle) -> Result<(), Error> {
        let other_pane = self.other_pane(handle);
        let pane = self.panes.get(handle).unwrap();

        pane.navigate(other_pane.path())?;

        Ok(())
    }

    pub fn toggle_hidden(&self) {
        {
            let mut display_options = self.display_options.0.write();
            display_options.show_hidden = !display_options.show_hidden;
        }

        for (_, pane) in self.panes.iter() {
            pane.update_view_state();
        }
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdatePayload {
    State(serde_json::Value),
    Patch(Vec<PatchOperation>),
}

pub struct UpdatePublisher {
    window: Window,
    previous: Mutex<serde_json::Value>,
}

impl UpdatePublisher {
    fn new(window: Window) -> Self {
        Self {
            window,
            previous: Mutex::new(serde_json::Value::Null),
        }
    }

    pub fn publish(&self, state: &MainWindowState) -> Result<(), Error> {
        let serialized = serde_json::to_value(state).unwrap();
        let patch;
        {
            let mut previous = self.previous.lock();
            patch = diff(&previous, &serialized, Some(100));
            *previous = serialized.clone();
        }

        self.window.emit(
            "updated",
            patch
                .map(UpdatePayload::Patch)
                .unwrap_or(UpdatePayload::State(serialized)),
        )?;

        Ok(())
    }
}

struct MainWindowContextInner {
    window: Window,
    watcher: Watcher,
    global_state: MainWindowState,
    publisher: Arc<UpdatePublisher>,
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
    pub fn create(window: Window) -> Result<Self, Error> {
        let paths = ["/".to_string(), "/home/tibordp/.bashrc".to_string()];
        let global_state = MainWindowState::new(paths);
        global_state.refresh()?;

        let publisher = Arc::new(UpdatePublisher::new(window.clone()));
        let watcher = Watcher::new(publisher.clone(), global_state.clone());

        Ok(Self {
            inner: Arc::new(MainWindowContextInner {
                window,
                watcher,
                publisher,
                global_state,
            }),
        })
    }

    pub fn with_update(
        &self,
        f: impl FnOnce(&MainWindowState) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let ret = f(&self.inner.global_state);

        self.inner.watcher.update_paths();
        self.inner.publisher.publish(&self.inner.global_state)?;

        if let Some(pane) = self.active_pane() {
            self.inner
                .window
                .set_title(&format!("{} - newt", pane.path().display()))
                .unwrap();
        }

        ret
    }

    pub fn with_pane_update(
        &self,
        pane_handle: PaneHandle,
        f: impl FnOnce(&Pane) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.with_update(|s| {
            let pane = s.panes.get(pane_handle).unwrap();
            f(&pane)
        })
    }

    pub fn panes(&self) -> &Panes {
        &self.inner.global_state.panes
    }

    pub fn active_pane(&self) -> Option<Arc<Pane>> {
        for (_, pane) in self.inner.global_state.panes.iter() {
            let view_state = pane.view_state();
            if view_state.active {
                drop(view_state);
                return Some(pane);
            }
        }

        None
    }
}

struct WatcherInner {
    global_state: MainWindowState,
    watcher: Option<RecommendedWatcher>,
    watched_paths: HashSet<PathBuf>,
}

#[derive(Clone)]
pub struct Watcher {
    inner: Arc<Mutex<WatcherInner>>,
}

impl Watcher {
    pub fn new(publisher: Arc<UpdatePublisher>, global_state: MainWindowState) -> Self {
        let inner = Arc::new(Mutex::new(WatcherInner {
            global_state,
            watcher: None,
            watched_paths: HashSet::new(),
        }));

        let watcher = {
            let inner = Arc::clone(&inner);
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        let inner = inner.lock();
                        let mut changed = false;

                        {
                            for (_, pane) in inner.global_state.panes.iter() {
                                let pane_path = pane.path();
                                if event.paths.iter().any(|p| p.starts_with(&pane_path)) {
                                    if let Err(e) = pane.refresh() {
                                        eprintln!("refresh error: {:?}", e);
                                    }
                                    changed = true;
                                }
                            }
                        }

                        if changed {
                            publisher.publish(&inner.global_state).unwrap();
                        }
                    }
                    Err(e) => eprintln!("watch error: {:?}", e),
                };
            })
            .unwrap()
        };

        inner.lock().watcher = Some(watcher);
        Self { inner }
    }

    pub fn update_paths(&self) {
        use notify::Watcher;
        let mut inner = self.inner.lock();

        let paths: HashSet<PathBuf> = inner
            .global_state
            .panes
            .iter()
            .map(|(_, p)| p.path())
            .collect();

        let to_add: Vec<_> = paths.difference(&inner.watched_paths).cloned().collect();
        let to_remove: Vec<_> = inner.watched_paths.difference(&paths).cloned().collect();

        let watcher = inner.watcher.as_mut().unwrap();

        for path in to_add {
            if let Err(e) = watcher.watch(&path, RecursiveMode::NonRecursive) {
                eprintln!("watch error: {:?}", e);
            }
        }
        for path in to_remove {
            match watcher.unwatch(&path) {
                Ok(_)
                | Err(notify::Error {
                    kind: notify::ErrorKind::WatchNotFound,
                    ..
                }) => {}
                Err(e) => {
                    eprintln!("unwatch error: {:?}", e);
                }
            }
        }

        inner.watched_paths = paths;
    }
}
