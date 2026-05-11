// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
extern crate objc; // v0.2.7

pub mod cmd;
pub mod common;
pub mod connections;
pub mod editor;
pub mod file_server;
pub mod keychain;
pub mod main_window;
pub mod preferences;
pub mod user_commands;
pub mod viewer;

use clap::{ArgAction, Parser};
use common::Error;
use log::debug;
use log::info;
use main_window::ConnectionTarget;
use main_window::MainWindowContext;
use main_window::spawn_main_window;
use main_window::{AgentResolver, TauriAgentResolver};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use tauri::Manager;
use tauri::State;
use tauri::Webview;
use tauri::WebviewWindow;
use tauri::Wry;
use tauri::ipc::Invoke;

#[derive(Parser, Debug)]
#[command(author, version = include_str!(concat!(env!("OUT_DIR"), "/long_version.txt")), about, long_about = None)]
struct Args {
    /// Connect to a remote host via SSH (e.g., "user@host")
    #[cfg_attr(target_os = "linux", arg(long, conflicts_with_all = ["elevated", "profile"]))]
    #[cfg_attr(not(target_os = "linux"), arg(long, conflicts_with = "profile"))]
    connect: Option<String>,

    /// Run with an elevated (root) agent via pkexec (Linux only).
    #[cfg(target_os = "linux")]
    #[arg(long, conflicts_with = "profile")]
    elevated: bool,

    /// Open with a saved connection profile (by `name` or `id`). Currently
    /// supports profiles of type `remote`; for S3/SFTP profiles use Quick
    /// Connect inside the app.
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,

    /// Window title suffix (e.g., "user@host" or "Elevated")
    #[arg(long)]
    title: Option<String>,

    /// Override the configuration directory (settings, connections, hot
    /// paths, history). Defaults to the platform's standard app-config
    /// location.
    #[arg(long, value_name = "PATH")]
    config_dir: Option<std::path::PathBuf>,

    /// Initial path for the left pane (defaults to cwd locally, $HOME on remote).
    #[arg(long, value_name = "PATH")]
    cwd_left: Option<std::path::PathBuf>,

    /// Initial path for the right pane (defaults to same as left).
    #[arg(long, value_name = "PATH")]
    cwd_right: Option<std::path::PathBuf>,

    /// Print resolved version, config directory, and agent inventory to
    /// stdout and exit. Useful for "what state is my install in?" debugging.
    #[arg(long)]
    print_config: bool,

    /// Increase log verbosity (-v: debug, -vv: trace). Ignored if RUST_LOG is set.
    #[arg(short, long, action = ArgAction::Count, conflicts_with = "quiet")]
    verbose: u8,

    /// Only log errors. Ignored if RUST_LOG is set.
    #[arg(short, long)]
    quiet: bool,
}

/// Apply `-v`/`-q` to the `RUST_LOG` env var if the user hasn't already
/// set one. The explicit env var always wins so power users can still
/// dial in per-module filters.
fn apply_log_flags(verbose: u8, quiet: bool) {
    if std::env::var_os("RUST_LOG").is_some() {
        return;
    }
    let level = match (quiet, verbose) {
        (true, _) => "error",
        (_, 0) => "info",
        (_, 1) => "debug",
        (_, _) => "trace",
    };
    // SAFETY: single-threaded startup, before any logger or other env-reader
    // has spawned.
    unsafe { std::env::set_var("RUST_LOG", level) };
}

/// A pre-warmed hidden window ready to be activated.
pub struct PrewarmedWindow {
    pub label: String,
    pub window: WebviewWindow,
}

pub struct GlobalContext {
    main_windows: Mutex<HashMap<String, MainWindowContext>>,
    viewer_windows: Mutex<HashMap<String, viewer::ViewerWindowContext>>,
    editor_windows: Mutex<HashMap<String, editor::EditorWindowContext>>,
    /// Pre-warmed hidden viewer windows, keyed by parent main window label.
    prewarmed_viewers: Mutex<HashMap<String, PrewarmedWindow>>,
    /// Pre-warmed hidden editor windows, keyed by parent main window label.
    prewarmed_editors: Mutex<HashMap<String, PrewarmedWindow>>,
    agent_resolver: OnceLock<TauriAgentResolver>,
    preferences: OnceLock<preferences::PreferencesManager>,
    #[cfg(target_os = "macos")]
    window_menus: Mutex<HashMap<String, tauri::menu::Menu<tauri::Wry>>>,
}

impl Default for GlobalContext {
    fn default() -> Self {
        Self {
            main_windows: Mutex::new(HashMap::new()),
            viewer_windows: Mutex::new(HashMap::new()),
            editor_windows: Mutex::new(HashMap::new()),
            prewarmed_viewers: Mutex::new(HashMap::new()),
            prewarmed_editors: Mutex::new(HashMap::new()),
            agent_resolver: OnceLock::new(),
            preferences: OnceLock::new(),
            #[cfg(target_os = "macos")]
            window_menus: Mutex::new(HashMap::new()),
        }
    }
}

impl GlobalContext {
    pub fn init_agent_resolver(&self, app_handle: &tauri::AppHandle) {
        self.agent_resolver
            .set(TauriAgentResolver::new(app_handle))
            .ok();
    }

    pub fn agent_resolver(&self) -> &dyn AgentResolver {
        self.agent_resolver
            .get()
            .expect("AgentResolver not initialized")
    }

    pub fn init_preferences(
        &self,
        app_handle: &tauri::AppHandle,
        config_dir_override: Option<std::path::PathBuf>,
    ) {
        self.preferences
            .set(preferences::PreferencesManager::new(
                app_handle,
                config_dir_override,
            ))
            .ok();
    }

    pub fn preferences(&self) -> &preferences::PreferencesManager {
        self.preferences
            .get()
            .expect("PreferencesManager not initialized")
    }

    pub fn main_window(&self, webview: &Webview) -> Option<MainWindowContext> {
        self.main_windows.lock().get(webview.label()).cloned()
    }

    pub fn register_viewer_window(&self, label: &str, ctx: viewer::ViewerWindowContext) {
        self.viewer_windows.lock().insert(label.to_string(), ctx);
    }

    pub fn viewer_window(&self, label: &str) -> Option<viewer::ViewerWindowContext> {
        self.viewer_windows.lock().get(label).cloned()
    }

    pub fn register_editor_window(&self, label: &str, ctx: editor::EditorWindowContext) {
        self.editor_windows.lock().insert(label.to_string(), ctx);
    }

    pub fn editor_window(&self, label: &str) -> Option<editor::EditorWindowContext> {
        self.editor_windows.lock().get(label).cloned()
    }

    pub fn destroy_window(&self, label: &str) -> Result<(), Error> {
        info!("destroying window {}", label);
        let was_main = self.main_windows.lock().remove(label).is_some();
        self.viewer_windows.lock().remove(label);
        self.editor_windows.lock().remove(label);
        #[cfg(target_os = "macos")]
        self.window_menus.lock().remove(label);

        // If a main window was destroyed, also destroy its pre-warmed children
        if was_main {
            self.destroy_prewarmed_for(label);
        }

        // Also clean up if a pre-warmed window itself was destroyed
        self.prewarmed_viewers
            .lock()
            .retain(|_, pw| pw.label != label);
        self.prewarmed_editors
            .lock()
            .retain(|_, pw| pw.label != label);

        Ok(())
    }

    /// Take a pre-warmed viewer window for the given main window.
    pub fn take_prewarmed_viewer(&self, main_label: &str) -> Option<PrewarmedWindow> {
        self.prewarmed_viewers.lock().remove(main_label)
    }

    /// Take a pre-warmed editor window for the given main window.
    pub fn take_prewarmed_editor(&self, main_label: &str) -> Option<PrewarmedWindow> {
        self.prewarmed_editors.lock().remove(main_label)
    }

    /// Store a pre-warmed viewer window for the given main window.
    pub fn set_prewarmed_viewer(&self, main_label: &str, pw: PrewarmedWindow) {
        self.prewarmed_viewers
            .lock()
            .insert(main_label.to_string(), pw);
    }

    /// Store a pre-warmed editor window for the given main window.
    pub fn set_prewarmed_editor(&self, main_label: &str, pw: PrewarmedWindow) {
        self.prewarmed_editors
            .lock()
            .insert(main_label.to_string(), pw);
    }

    /// Destroy pre-warmed windows belonging to a main window.
    fn destroy_prewarmed_for(&self, main_label: &str) {
        if let Some(pw) = self.prewarmed_viewers.lock().remove(main_label) {
            info!("destroying pre-warmed viewer {}", pw.label);
            self.viewer_windows.lock().remove(&pw.label);
            self.main_windows.lock().remove(&pw.label);
            let _ = pw.window.destroy();
        }
        if let Some(pw) = self.prewarmed_editors.lock().remove(main_label) {
            info!("destroying pre-warmed editor {}", pw.label);
            self.editor_windows.lock().remove(&pw.label);
            self.main_windows.lock().remove(&pw.label);
            let _ = pw.window.destroy();
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_window_menu(&self, label: &str, menu: tauri::menu::Menu<tauri::Wry>) {
        self.window_menus.lock().insert(label.to_string(), menu);
    }

    #[cfg(target_os = "macos")]
    pub fn get_window_menu(&self, label: &str) -> Option<tauri::menu::Menu<tauri::Wry>> {
        self.window_menus.lock().get(label).cloned()
    }
}

pub fn detect_theme() -> Option<tauri::Theme> {
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

/// Diagnostic dump for `--print-config`. Prints the same identity info the
/// About dialog shows, plus the resolved configuration directory and the
/// agent binaries the host has on hand.
fn print_resolved_config(global_ctx: &GlobalContext) {
    println!(
        "{}",
        include_str!(concat!(env!("OUT_DIR"), "/long_version.txt"))
    );
    println!();
    println!(
        "Config dir: {}",
        global_ctx.preferences().config_dir().display()
    );
    let agent_hash = global_ctx
        .agent_resolver()
        .agent_hash()
        .unwrap_or_else(|e| format!("(unavailable: {})", e));
    println!("Agents hash: {}", agent_hash);
}

/// Resolve a `--profile` argument against the saved-connections store. Only
/// `remote` profiles map cleanly to a startup `ConnectionTarget`; S3/SFTP
/// profiles need an existing local session to mount onto, so we point the
/// user at Quick Connect for those.
fn resolve_profile(
    config_dir: &std::path::Path,
    name: &str,
) -> Result<(ConnectionTarget, String), Error> {
    let profile = crate::connections::list_connections(config_dir)
        .into_iter()
        .find(|p| p.name == name || p.id == name)
        .ok_or_else(|| Error::Custom(format!("connection profile '{}' not found", name)))?;
    match &profile.kind {
        crate::connections::ConnectionKind::Remote { host } => Ok((
            ConnectionTarget::Remote {
                transport_cmd: crate::main_window::ssh_transport_cmd(host),
            },
            profile.name,
        )),
        _ => Err(Error::Custom(format!(
            "profile '{}' is not a remote profile; open it via Quick Connect inside the app",
            name
        ))),
    }
}

fn main() {
    let args = Args::parse();
    apply_log_flags(args.verbose, args.quiet);
    pretty_env_logger::init();

    // Connection target for the non-profile cases — `--profile` is resolved
    // inside `setup` once the preferences directory is known.
    let non_profile_ct: Option<ConnectionTarget> = if let Some(ref host) = args.connect {
        Some(ConnectionTarget::Remote {
            transport_cmd: crate::main_window::ssh_transport_cmd(host),
        })
    } else {
        #[cfg(target_os = "linux")]
        {
            if args.elevated {
                Some(ConnectionTarget::Elevated)
            } else if args.profile.is_some() {
                None
            } else {
                Some(ConnectionTarget::Local)
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            if args.profile.is_some() {
                None
            } else {
                Some(ConnectionTarget::Local)
            }
        }
    };

    let specta_builder = cmd::create_specta_builder();

    // In debug builds, regenerate `src/lib/bindings.ts` from the Rust command
    // registry on every startup. CI fails if the committed bindings drift.
    #[cfg(debug_assertions)]
    if let Err(e) = specta_builder.export(cmd::typescript_export_config(), cmd::BINDINGS_PATH) {
        log::warn!("failed to export tauri-specta bindings: {}", e);
    }

    let inner_handler = specta_builder.invoke_handler();
    let handler = cmd::wrap_with_modal_close_middleware(inner_handler);
    let handler = Box::new(move |i: Invoke<Wry>| -> bool {
        let start = std::time::Instant::now();
        let cmd = i.message.command().to_string();

        let result = handler(i);
        debug!("handler {} took {:?}", cmd, start.elapsed());
        result
    });

    let explicit_title = args.title.clone();
    let profile_arg = args.profile.clone();
    let config_dir_arg = args.config_dir.clone();
    let print_config = args.print_config;
    let initial_pane_paths: [Option<std::path::PathBuf>; 2] = [
        args.cwd_left.clone(),
        args.cwd_right.clone().or_else(|| args.cwd_left.clone()),
    ];
    let global_ctx = GlobalContext::default();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(global_ctx)
        .setup(move |app| {
            let global_ctx: State<GlobalContext> = app.state();
            global_ctx.init_agent_resolver(app.handle());
            global_ctx.init_preferences(app.handle(), config_dir_arg.clone());

            if print_config {
                print_resolved_config(&global_ctx);
                app.handle().exit(0);
                return Ok(());
            }

            // Resolve `--profile` now that the preferences manager (and thus
            // the config dir) is available; otherwise use the target picked
            // out of `--connect` / `--elevated` / default-local above.
            let (ct, default_title) = match (&profile_arg, &non_profile_ct) {
                (Some(name), _) => {
                    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
                    resolve_profile(&config_dir, name)?
                }
                (None, Some(ct)) => (ct.clone(), "Newt".to_string()),
                (None, None) => {
                    unreachable!("non_profile_ct is None only when profile_arg is Some")
                }
            };
            let wt = match &explicit_title {
                Some(t) => format!("Newt [{}]", t),
                None if default_title == "Newt" => "Newt".to_string(),
                None => format!("Newt [{}]", default_title),
            };

            let (_window, ctx) = spawn_main_window(
                app.handle(),
                ct.clone(),
                wt.clone(),
                initial_pane_paths.clone(),
            )?;

            // Local mode: connect synchronously so state is ready before JS runs.
            // Remote/Elevated: `init` command triggers connect asynchronously.
            if matches!(ct, ConnectionTarget::Local) {
                let agent_resolver = global_ctx.agent_resolver();
                // If the local connect fails, log and degrade rather than
                // killing the process: the frontend's `init` command will
                // also try to connect once the webview loads, which is the
                // path that handles errors visibly.
                if let Err(e) = tauri::async_runtime::block_on(ctx.connect(agent_resolver)) {
                    log::error!("local connect failed during setup: {}", e);
                    ctx.set_connection_failed(e.to_string());
                } else {
                    // Pre-warm viewer and editor windows
                    cmd::prewarm_viewer(app.handle(), &ctx, "main");
                    cmd::prewarm_editor(app.handle(), &ctx, "main");
                }
            }

            Ok(())
        })
        .on_window_event(
            #[allow(clippy::single_match)]
            |window, event| {
                let app_handle = window.app_handle();
                let global_ctx: State<GlobalContext> = app_handle.state();

                match event {
                    tauri::WindowEvent::Destroyed => {
                        global_ctx.destroy_window(window.label()).unwrap();
                        // Exit when the last real main window is gone. On macOS,
                        // Tauri would otherwise keep the process alive (standard
                        // Cocoa behavior); we want the same semantics as
                        // pre-refactor, when each main window was its own
                        // process. `main_windows` also contains aliased entries
                        // for prewarmed/active viewer/editor children (value is
                        // the parent's ctx), so filter to self-parented entries.
                        let has_real_main = global_ctx
                            .main_windows
                            .lock()
                            .iter()
                            .any(|(k, ctx)| k == ctx.main_window_label());
                        if !has_real_main {
                            app_handle.exit(0);
                        }
                    }
                    tauri::WindowEvent::Focused(true) => {
                        // On macOS, swap the app-wide menu to match the focused window
                        #[cfg(target_os = "macos")]
                        {
                            if let Some(menu) = global_ctx.get_window_menu(window.label()) {
                                let _ = app_handle.set_menu(menu);
                            } else {
                                let _ = app_handle.remove_menu();
                            }
                        }

                        if let Some(ctx) =
                            global_ctx.main_windows.lock().get(window.label()).cloned()
                        {
                            tauri::async_runtime::spawn(async move { ctx.refresh(false).await });
                        }
                    }
                    tauri::WindowEvent::DragDrop(event) => {
                        use tauri::Emitter;
                        match event {
                            tauri::DragDropEvent::Enter { paths, position } => {
                                let _ = window.emit(
                                    "external-drag",
                                    serde_json::json!({
                                        "kind": "enter",
                                        "paths": paths,
                                        "x": position.x,
                                        "y": position.y,
                                    }),
                                );
                            }
                            tauri::DragDropEvent::Over { position } => {
                                let _ = window.emit(
                                    "external-drag",
                                    serde_json::json!({
                                        "kind": "over",
                                        "x": position.x,
                                        "y": position.y,
                                    }),
                                );
                            }
                            tauri::DragDropEvent::Drop { paths, position } => {
                                let _ = window.emit(
                                    "external-drag",
                                    serde_json::json!({
                                        "kind": "drop",
                                        "paths": paths,
                                        "x": position.x,
                                        "y": position.y,
                                    }),
                                );
                            }
                            tauri::DragDropEvent::Leave => {
                                let _ = window
                                    .emit("external-drag", serde_json::json!({ "kind": "leave" }));
                            }
                            _ => {}
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
