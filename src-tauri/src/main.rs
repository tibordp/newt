// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod cmd;
pub mod common;
pub mod pane;

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use common::Error;
use notify::{RecommendedWatcher, RecursiveMode};
use pane::PaneViewState;
use serde::ser::SerializeSeq;
use tauri::{window, AppHandle, Invoke, Manager, Window, Wry};

#[derive(
    PartialEq, Eq, Hash, PartialOrd, Ord, Clone, Copy, serde::Serialize, serde::Deserialize,
)]
pub struct PaneHandle(usize);

#[derive(Clone)]
pub struct Panes(Vec<Arc<Mutex<PaneViewState>>>);

impl Panes {
    fn new(inner: impl IntoIterator<Item = PaneViewState>) -> Self {
        Self(
            inner
                .into_iter()
                .map(|state| Arc::new(Mutex::new(state)))
                .collect(),
        )
    }

    fn get(&self, handle: PaneHandle) -> Option<Arc<Mutex<PaneViewState>>> {
        self.0.get(handle.0).cloned()
    }

    fn iter(&self) -> impl Iterator<Item = (PaneHandle, Arc<Mutex<PaneViewState>>)> + '_ {
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
            let e = e.lock().unwrap();
            seq.serialize_element(&*e)?;
        }
        seq.end()
    }
}

#[derive(Clone, serde::Serialize)]
pub struct GlobalState {
    pub panes: Panes,
}

impl GlobalState {
    fn new(paths: impl IntoIterator<Item = String>) -> Self {
        Self {
            panes: Panes::new(paths.into_iter().map(|path| {
                let path = PathBuf::from(path);
                PaneViewState::create(path).unwrap()
            })),
        }
    }

    fn other_pane(&self, handle: PaneHandle) -> Arc<Mutex<PaneViewState>> {
        assert!(self.panes.0.len() == 2);

        self.panes.get(PaneHandle(1 - handle.0)).unwrap()
    }

    pub fn activate_pane(&self, handle: PaneHandle) {
        for (h, pane) in self.panes.iter() {
            let mut pane = pane.lock().unwrap();
            pane.active = h == handle;
        }
    }

    pub fn copy_pane(&self, handle: PaneHandle) {
        let other_pane = self.other_pane(handle);
        let pane = self.panes.get(handle).unwrap();

        let new_pane = other_pane.lock().unwrap().clone();
        let mut target = pane.lock().unwrap();

        let was_active = target.active;
        *target = new_pane;

        if was_active {
            target.active = true;
        }
    }
}

pub struct WindowContext {
    window: Window,
    watcher: Watcher,
    global_state: GlobalState,
}

impl WindowContext {
    pub fn create(window: Window) -> Result<Self, Error> {
        let paths = ["/".to_string(), "/".to_string()];
        let global_state = GlobalState::new(paths);
        let watcher = Watcher::new(window.clone(), global_state.clone());

        Ok(Self {
            window,
            watcher,
            global_state,
        })
    }

    pub fn with_updates(
        &self,
        f: impl FnOnce(&GlobalState) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let ret = f(&self.global_state);

        self.watcher.update_paths();
        self.window.emit("updated", UpdatePayload::new(self.global_state.clone()))?;

        ret
    }
}

#[derive(Clone, serde::Serialize)]
pub struct UpdatePayload {
    pub state: GlobalState,
}

impl UpdatePayload {
    pub fn new(state: GlobalState) -> Self {
        Self { state }
    }
}

struct WatcherInner {
    global_state: GlobalState,
    watcher: Option<RecommendedWatcher>,
    watched_paths: HashSet<PathBuf>,
}

#[derive(Clone)]
pub struct Watcher {
    inner: Arc<Mutex<WatcherInner>>,
}

impl Watcher {
    pub fn new(window: Window, global_state: GlobalState) -> Self {
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
                        let mut inner = inner.lock().unwrap();
                        let mut changed = false;

                        {
                            let mut panes = inner.global_state.panes.0.iter_mut();
                            while let Some(pane) = panes.next() {
                                let mut pane = pane.lock().unwrap();
                                if event.paths.iter().any(|p| p.starts_with(&pane.path)) {
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

        inner.lock().unwrap().watcher = Some(watcher);
        Self { inner }
    }

    pub fn update_paths(&self) {
        use notify::Watcher;
        let mut inner = self.inner.lock().unwrap();

        let paths: HashSet<PathBuf> = inner
            .global_state
            .panes
            .iter()
            .map(|(_, p)| p.lock().unwrap().path.clone())
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
                Ok(_) | Err(notify::Error{ kind: notify::ErrorKind::WatchNotFound, .. }) => {}
                Err(e) => {
                    eprintln!("unwatch error: {:?}", e);
                }
            }
        }

        inner.watched_paths = paths;
    }
}

fn main() {
    let handler: Box<dyn Fn(Invoke<Wry>) + Send + Sync + 'static> =
        Box::new(tauri::generate_handler![
            cmd::navigate,
            cmd::ping,
            cmd::focus,
            cmd::set_sorting,
            cmd::toggle_selected,
            cmd::select_all,
            cmd::deselect_all,
            cmd::relative_jump,
            cmd::set_filter,
            cmd::copy_pane
        ]);

    let handler = Box::new(move |i| {
        let start = std::time::Instant::now();
        handler(i);
        println!("handler took {:?}", start.elapsed());
    });

    tauri::Builder::default()
        .on_page_load(|w, _| {
            let context = WindowContext::create(w.clone()).unwrap();
            w.app_handle().manage(context);
        })
        .invoke_handler(handler)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
