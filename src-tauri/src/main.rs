// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
#[cfg(target_os = "macos")]
extern crate objc; // v0.2.7

pub mod cmd;
pub mod common;
pub mod main_window;
pub mod viewer;

use common::Error;
use log::debug;
use log::info;
use main_window::MainWindowContext;
use parking_lot::Mutex;
use tauri::http::ResponseBuilder;
use std::collections::HashMap;
use tauri::Invoke;
use tauri::Manager;
use tauri::State;
use tauri::Window;

#[derive(Default)]
pub struct GlobalContext {
    main_windows: Mutex<HashMap<Window, MainWindowContext>>,
}

impl GlobalContext {
    pub async fn create_main_window(&self, window: Window) -> Result<(), Error> {
        info!("creating window {}", window.label());
        let window_context = MainWindowContext::create(window.clone()).await?;
        self.main_windows.lock().insert(window, window_context);

        Ok(())
    }

    pub fn main_window(&self, window: &Window) -> Option<MainWindowContext> {
        self.main_windows.lock().get(window).cloned()
    }

    pub fn destroy_window(&self, window: &Window) -> Result<(), Error> {
        info!("destroying window {}", window.label());
        self.main_windows.lock().remove(window);
        Ok(())
    }
}

fn main() {
    pretty_env_logger::init();

    let handler = cmd::create_handler();
    let handler = Box::new(move |i: Invoke| {
        let start = std::time::Instant::now();
        let cmd = i.message.command().to_string();

        handler(i);
        debug!("handler {} took {:?}", cmd, start.elapsed());
    });

    let global_ctx = GlobalContext::default();
    tauri::Builder::default()
        .manage(global_ctx)
        .on_page_load(|w, _payload| {
            let app_handle = w.app_handle();
            let global_ctx: State<GlobalContext> = app_handle.state();

            eprintln!("{:?}", w.url());

            match w.url().scheme() {
                "newt-preview" => {

                }
                _ => {
                    tauri::async_runtime::block_on(global_ctx.create_main_window(w)).unwrap();
                }
            }
        })
        .on_window_event(
            #[allow(clippy::single_match)]
            |event| {
                let app_handle = event.window().app_handle();
                let global_ctx: State<GlobalContext> = app_handle.state();

                match event.event() {
                    tauri::WindowEvent::Destroyed => {
                        global_ctx.destroy_window(event.window()).unwrap();
                    }
                    tauri::WindowEvent::Focused(true) => {
                        if let Some(ctx) = global_ctx.main_window(event.window()) {
                            tauri::async_runtime::spawn(async move { ctx.refresh().await });
                        }
                    }
                    _ => {}
                }
            },
        )
        .register_uri_scheme_protocol("newt-preview", crate::viewer::url_handler)
        .invoke_handler(handler)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
