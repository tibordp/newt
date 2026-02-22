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
use std::collections::HashMap;
use tauri::ipc::Invoke;
use tauri::Manager;
use tauri::State;
use tauri::Webview;
use tauri::Wry;

#[derive(Default)]
pub struct GlobalContext {
    main_windows: Mutex<HashMap<String, MainWindowContext>>,
}

impl GlobalContext {
    pub async fn create_main_window(&self, webview: &Webview) -> Result<(), Error> {
        let label = webview.label().to_string();
        info!("creating window {}", label);
        let webview_window = webview
            .app_handle()
            .get_webview_window(&label)
            .expect("webview window not found");
        let window_context = MainWindowContext::create(webview_window).await?;
        self.main_windows.lock().insert(label, window_context);

        Ok(())
    }

    pub fn main_window(&self, webview: &Webview) -> Option<MainWindowContext> {
        self.main_windows.lock().get(webview.label()).cloned()
    }

    pub fn destroy_window(&self, label: &str) -> Result<(), Error> {
        info!("destroying window {}", label);
        self.main_windows.lock().remove(label);
        Ok(())
    }
}

fn main() {
    pretty_env_logger::init();

    let handler = cmd::create_handler();
    let handler = Box::new(move |i: Invoke<Wry>| -> bool {
        let start = std::time::Instant::now();
        let cmd = i.message.command().to_string();

        let result = handler(i);
        debug!("handler {} took {:?}", cmd, start.elapsed());
        result
    });

    let global_ctx = GlobalContext::default();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(global_ctx)
        .on_page_load(|webview, _payload| {
            let app_handle = webview.app_handle();
            let global_ctx: State<GlobalContext> = app_handle.state();

            let url = webview.url().unwrap();
            eprintln!("{:?}", url);

            match url.scheme() {
                "newt-preview" => {}
                _ => {
                    tauri::async_runtime::block_on(global_ctx.create_main_window(webview))
                        .unwrap();
                }
            }
        })
        .on_window_event(
            #[allow(clippy::single_match)]
            |window, event| {
                let app_handle = window.app_handle();
                let global_ctx: State<GlobalContext> = app_handle.state();

                match event {
                    tauri::WindowEvent::Destroyed => {
                        global_ctx.destroy_window(window.label()).unwrap();
                    }
                    tauri::WindowEvent::Focused(true) => {
                        if let Some(ctx) = global_ctx
                            .main_windows
                            .lock()
                            .get(window.label())
                            .cloned()
                        {
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
