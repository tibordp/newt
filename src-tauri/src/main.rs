// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
#[cfg(target_os = "macos")]
extern crate objc; // v0.2.7

pub mod cmd;
pub mod common;
pub mod main_window;
pub mod viewer;

use clap::Parser;
use common::Error;
use log::debug;
use log::info;
use main_window::ConnectionTarget;
use main_window::MainWindowContext;
use parking_lot::Mutex;
use std::collections::HashMap;
use tauri::ipc::Invoke;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tauri::Webview;
use tauri::Wry;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Connect to a remote host via SSH (e.g., "user@host")
    #[arg(long)]
    connect: Option<String>,

    /// Run with an elevated (root) agent via pkexec
    #[arg(long)]
    elevated: bool,

    /// Window title suffix (e.g., "user@host" or "Elevated")
    #[arg(long)]
    title: Option<String>,
}

pub struct GlobalContext {
    main_windows: Mutex<HashMap<String, MainWindowContext>>,
    connection_target: ConnectionTarget,
    window_title: String,
}

impl GlobalContext {
    pub fn new(connection_target: ConnectionTarget, window_title: String) -> Self {
        Self {
            main_windows: Mutex::new(HashMap::new()),
            connection_target,
            window_title,
        }
    }

    pub async fn create_main_window(&self, webview: &Webview) -> Result<(), Error> {
        let label = webview.label().to_string();
        info!("creating window {}", label);
        let webview_window = webview
            .app_handle()
            .get_webview_window(&label)
            .expect("webview window not found");
        let window_context = MainWindowContext::create(
            webview_window,
            self.connection_target.clone(),
            self.window_title.clone(),
        )
        .await?;
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

    let args = Args::parse();

    let connection_target = if let Some(ref host) = args.connect {
        ConnectionTarget::Remote {
            transport_cmd: vec!["ssh".to_string(), host.clone()],
        }
    } else if args.elevated {
        ConnectionTarget::Elevated
    } else {
        ConnectionTarget::Local
    };

    let handler = cmd::create_handler();
    let handler = Box::new(move |i: Invoke<Wry>| -> bool {
        let start = std::time::Instant::now();
        let cmd = i.message.command().to_string();

        let result = handler(i);
        debug!("handler {} took {:?}", cmd, start.elapsed());
        result
    });

    let window_title = match args.title {
        Some(ref t) => format!("Newt [{}]", t),
        None => "Newt".to_string(),
    };

    let setup_title = window_title.clone();
    let global_ctx = GlobalContext::new(connection_target, window_title);
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(global_ctx)
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("/".into()),
            )
            .title(&setup_title)
            .resizable(true)
            .inner_size(800.0, 600.0)
            .build()?;
            Ok(())
        })
        .on_page_load(|webview, _payload| {
            let app_handle = webview.app_handle();
            let global_ctx: State<GlobalContext> = app_handle.state();

            // If the label already exists (e.g. viewer windows pre-registered by the
            // `view` command), skip creating a new agent/context.
            if global_ctx.main_window(webview).is_some() {
                return;
            }

            match &global_ctx.connection_target {
                ConnectionTarget::Local => {
                    tauri::async_runtime::block_on(global_ctx.create_main_window(webview))
                        .unwrap();
                }
                _ => {
                    // For remote/elevated connections, spawn async so the event
                    // loop keeps running and the webview can render a connecting
                    // indicator while SSH/pkexec completes.
                    let connection_target = global_ctx.connection_target.clone();
                    let window_title = global_ctx.window_title.clone();
                    let app_handle = app_handle.clone();
                    let label = webview.label().to_string();
                    tauri::async_runtime::spawn(async move {
                        let webview_window = app_handle
                            .get_webview_window(&label)
                            .expect("webview window not found");
                        match MainWindowContext::create(
                            webview_window.clone(),
                            connection_target,
                            window_title,
                        )
                        .await
                        {
                            Ok(ctx) => {
                                app_handle
                                    .state::<GlobalContext>()
                                    .main_windows
                                    .lock()
                                    .insert(label, ctx.clone());
                                let _ = ctx.publish_full();
                            }
                            Err(e) => {
                                log::error!("Failed to initialize: {}", e);
                                let _ = webview_window.emit("init_error", e.to_string());
                            }
                        }
                    });
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
        .invoke_handler(handler)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
