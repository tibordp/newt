// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![feature(io_error_more)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
#[cfg(target_os = "macos")]
extern crate objc; // v0.2.7

pub mod cmd;
pub mod common;
pub mod filesystem;
pub mod main_window;

use common::Error;
use main_window::MainWindowContext;
use parking_lot::Mutex;
use std::collections::HashMap;
use tauri::Manager;
use tauri::State;
use tauri::Window;

#[derive(Default)]
pub struct GlobalContext {
    main_windows: Mutex<HashMap<Window, MainWindowContext>>,
}

impl GlobalContext {
    pub async fn create_window(&self, window: Window) -> Result<(), Error> {
        println!("creating window");
        let window_context = MainWindowContext::create(window.clone()).await?;
        self.main_windows.lock().insert(window, window_context);

        Ok(())
    }

    pub fn main_window(&self, window: &Window) -> Option<MainWindowContext> {
        self.main_windows.lock().get(window).cloned()
    }

    pub fn destroy_window(&self, window: &Window) -> Result<(), Error> {
        println!("destroying window {}", window.label());
        self.main_windows.lock().remove(window);
        Ok(())
    }
}

fn main() {
    pretty_env_logger::init();

    let handler = cmd::create_handler();
    let handler = Box::new(move |i| {
        let start = std::time::Instant::now();
        handler(i);
        println!("handler took {:?}", start.elapsed());
    });

    let global_ctx = GlobalContext::default();
    tauri::Builder::default()
        .manage(global_ctx)
        .on_page_load(|w, _payload| {
            let app_handle = w.app_handle();
            let global_ctx: State<GlobalContext> = app_handle.state();

            tauri::async_runtime::block_on(global_ctx.create_window(w)).unwrap();
        })
        .on_window_event(
            #[allow(clippy::single_match)]
            |event| match event.event() {
                tauri::WindowEvent::Destroyed => {
                    let app_handle = event.window().app_handle();
                    let global_ctx: State<GlobalContext> = app_handle.state();

                    global_ctx.destroy_window(event.window()).unwrap();
                }
                _ => {}
            },
        )
        .invoke_handler(handler)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
