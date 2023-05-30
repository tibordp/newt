pub mod pane;

use notify::RecommendedWatcher;
use notify::RecursiveMode;
use parking_lot::Mutex;
use parking_lot::RwLock;
use parking_lot::RwLockUpgradableReadGuard;
use serde::ser::SerializeSeq;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;
use tauri::Window;
use tauri::Wry;

use crate::common::Error;
use crate::GlobalContext;

use self::pane::PaneViewState;

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
pub struct Panes(Vec<Arc<RwLock<PaneViewState>>>);

impl Panes {
    pub fn new(inner: impl IntoIterator<Item = PaneViewState>) -> Self {
        Self(
            inner
                .into_iter()
                .map(|state| Arc::new(RwLock::new(state)))
                .collect(),
        )
    }

    pub fn get(&self, handle: PaneHandle) -> Option<Arc<RwLock<PaneViewState>>> {
        self.0.get(handle.0).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = (PaneHandle, Arc<RwLock<PaneViewState>>)> + '_ {
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
            let e = e.read();
            seq.serialize_element(&*e)?;
        }
        seq.end()
    }
}

#[derive(Clone, serde::Serialize)]
pub struct MainWindowState {
    pub panes: Panes,
    pub display_options: DisplayOptions,
}

impl MainWindowState {
    fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let display_options = DisplayOptions::default();

        Self {
            panes: Panes::new(paths.into_iter().map(|path| {
                let path = PathBuf::from(path);
                PaneViewState::create(path, display_options.clone()).unwrap()
            })),
            display_options,
        }
    }

    fn other_pane(&self, handle: PaneHandle) -> Arc<RwLock<PaneViewState>> {
        assert!(self.panes.0.len() == 2);

        self.panes.get(PaneHandle(1 - handle.0)).unwrap()
    }

    pub fn activate_pane(&self, handle: PaneHandle) {
        for (h, pane) in self.panes.iter() {
            let mut pane = pane.write();
            pane.active = h == handle;
        }
    }

    pub fn copy_pane(&self, handle: PaneHandle) {
        let other_pane = self.other_pane(handle);
        let pane = self.panes.get(handle).unwrap();

        let new_pane = other_pane.read().clone();
        let mut target = pane.write();

        let was_active = target.active;
        *target = new_pane;
        target.deselect_all();

        if was_active {
            target.active = true;
        }
    }

    pub fn toggle_hidden(&self) {
        {
            let mut display_options = self.display_options.0.write();
            display_options.show_hidden = !display_options.show_hidden;
        }

        for (_, pane) in self.panes.iter() {
            let mut pane = pane.write();

            pane.filter();
            pane.sort();
            pane.update_focus();
        }
    }
}

struct MainWindowContextInner {
    window: Window,
    watcher: Watcher,
    global_state: MainWindowState,
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
        let watcher = Watcher::new(window.clone(), global_state.clone());

        Ok(Self {
            inner: Arc::new(MainWindowContextInner {
                window,
                watcher,
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
        self.inner.window.emit(
            "updated",
            UpdatePayload::new(self.inner.global_state.clone()),
        )?;

        ret
    }

    pub fn with_update_pane(
        &self,
        pane_handle: PaneHandle,
        f: impl FnOnce(&mut PaneViewState) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.with_update(|s| {
            let pane = s.panes.get(pane_handle).unwrap();
            let mut pane = pane.write();

            f(&mut pane)
        })
    }

    pub fn panes(&self) -> &Panes {
        &self.inner.global_state.panes
    }
}

#[derive(Clone, serde::Serialize)]
pub struct UpdatePayload {
    pub state: MainWindowState,
}

impl UpdatePayload {
    pub fn new(state: MainWindowState) -> Self {
        Self { state }
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
    pub fn new(window: Window, global_state: MainWindowState) -> Self {
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
                        let mut inner = inner.lock();
                        let mut changed = false;

                        {
                            for pane in inner.global_state.panes.0.iter_mut() {
                                let pane = pane.upgradable_read();
                                if event.paths.iter().any(|p| p.starts_with(&pane.path)) {
                                    let mut pane = RwLockUpgradableReadGuard::upgrade(pane);
                                    if let Err(e) = pane.refresh() {
                                        eprintln!("refresh error: {:?}", e);
                                    }
                                    changed = true;
                                }
                            }
                        }

                        if changed {
                            let payload = UpdatePayload::new(inner.global_state.clone());
                            window.emit("updated", payload).unwrap();
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
            .map(|(_, p)| p.read().path.clone())
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
