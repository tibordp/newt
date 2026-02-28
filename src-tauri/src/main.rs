// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
#[cfg(target_os = "macos")]
extern crate objc; // v0.2.7

pub mod cmd;
pub mod common;
pub mod file_server;
pub mod main_window;
pub mod preferences;
pub mod viewer;

use clap::Parser;
use common::Error;
use log::debug;
use log::info;
use main_window::AgentResolver;
use main_window::ConnectionTarget;
use main_window::MainWindowContext;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use tauri::ipc::Invoke;
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
    agent_resolver: OnceLock<AgentResolver>,
    preferences: OnceLock<preferences::PreferencesManager>,
}

impl GlobalContext {
    pub fn new(connection_target: ConnectionTarget, window_title: String) -> Self {
        Self {
            main_windows: Mutex::new(HashMap::new()),
            connection_target,
            window_title,
            agent_resolver: OnceLock::new(),
            preferences: OnceLock::new(),
        }
    }

    pub fn init_agent_resolver(&self, app_handle: &tauri::AppHandle) {
        self.agent_resolver.set(AgentResolver::new(app_handle)).ok();
    }

    pub fn agent_resolver(&self) -> &AgentResolver {
        self.agent_resolver
            .get()
            .expect("AgentResolver not initialized")
    }

    pub fn init_preferences(&self, app_handle: &tauri::AppHandle) {
        self.preferences
            .set(preferences::PreferencesManager::new(app_handle))
            .ok();
    }

    pub fn preferences(&self) -> &preferences::PreferencesManager {
        self.preferences
            .get()
            .expect("PreferencesManager not initialized")
    }

    pub async fn create_main_window(&self, webview: &Webview) -> Result<(), Error> {
        let label = webview.label().to_string();
        info!("creating window {}", label);
        let webview_window = webview
            .app_handle()
            .get_webview_window(&label)
            .expect("webview window not found");
        let prefs = self.preferences().settings();
        let window_context = MainWindowContext::create(
            webview_window,
            self.connection_target.clone(),
            self.window_title.clone(),
            None,
            self.agent_resolver(),
            &prefs,
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

fn detect_theme() -> Option<tauri::Theme> {
    #[cfg(target_os = "linux")]
    {
        use gio::prelude::SettingsExt;

        if let Ok(settings) =
            std::panic::catch_unwind(|| gio::Settings::new("org.gnome.desktop.interface"))
        {
            // Try freedesktop color-scheme first (GNOME 42+, KDE, etc.)
            let color_scheme = settings.string("color-scheme");
            if color_scheme.contains("prefer-dark") {
                return Some(tauri::Theme::Dark);
            }
            if color_scheme.contains("prefer-light") || color_scheme.contains("default") {
                return Some(tauri::Theme::Light);
            }

            // Fallback: check gtk-theme name for "-dark" suffix
            let gtk_theme = settings.string("gtk-theme").to_lowercase();
            if gtk_theme.contains("-dark") {
                return Some(tauri::Theme::Dark);
            }
        }
    }

    None
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

    let theme = detect_theme();

    let setup_title = window_title.clone();
    let global_ctx = GlobalContext::new(connection_target, window_title);
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(global_ctx)
        .setup(move |app| {
            let global_ctx: State<GlobalContext> = app.state();
            global_ctx.init_agent_resolver(app.handle());
            global_ctx.init_preferences(app.handle());

            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("/".into()))
                .title(&setup_title)
                .resizable(true)
                .inner_size(800.0, 600.0)
                .theme(theme)
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

            // Local mode: init is instant, block to have state ready before JS runs.
            // Remote/Elevated: frontend drives init via the `init` command + Channel.
            if matches!(global_ctx.connection_target, ConnectionTarget::Local) {
                tauri::async_runtime::block_on(global_ctx.create_main_window(webview)).unwrap();
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
                        if let Some(ctx) =
                            global_ctx.main_windows.lock().get(window.label()).cloned()
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