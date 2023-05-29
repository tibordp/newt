// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod cmd;
pub mod common;
pub mod pane;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use pane::PaneViewState;
use serde::ser::SerializeSeq;

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

#[derive(Clone, serde::Serialize)]
pub struct UpdatePayload {
    pub state: GlobalState,
}

impl UpdatePayload {
    pub fn new(state: GlobalState) -> Self {
        Self { state }
    }
}

fn main() {
    let paths = ["/".to_string(), "/".to_string()];

    tauri::Builder::default()
        .manage(GlobalState::new(paths))
        .invoke_handler(tauri::generate_handler![
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
