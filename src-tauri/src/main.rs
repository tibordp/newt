// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod cmd;
pub mod common;
pub mod main_window;

use std::{
    collections::{HashSet, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use common::Error;
use main_window::MainWindowContext;
use notify::{RecommendedWatcher, RecursiveMode};
use serde::ser::SerializeSeq;
use tauri::{Invoke, Manager, Window, Wry, State};


pub struct GlobalContext {
    windows: Mutex<HashMap<Window, MainWindowContext>>,
}

impl GlobalContext {
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(HashMap::new())
        }
    }

    pub fn create_window(&self, window: Window) -> Result<(), Error> {
        println!("creating window");
        let window_context = MainWindowContext::create(window.clone())?;
        self.windows.lock().unwrap().insert(window, window_context);

        Ok(())
    }

    pub fn window(&self, window: &Window) -> Option<MainWindowContext> {
        println!("getting window {}", window.label());
        self.windows.lock().unwrap().get(window).cloned()
    }
}
fn main() {
    let handler = cmd::create_handler();
    let handler = Box::new(move |i| {
        let start = std::time::Instant::now();
        handler(i);
        println!("handler took {:?}", start.elapsed());
    });

    let global_ctx = GlobalContext::new();
    tauri::Builder::default()
        .manage(global_ctx)
        .on_page_load(|w, _| {
            let app_handle = w.app_handle();
            let global_ctx: State<GlobalContext> = app_handle.state();

            global_ctx.create_window(w).unwrap();
        })
        .invoke_handler(handler)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
