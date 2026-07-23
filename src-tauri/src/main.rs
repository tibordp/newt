// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
extern crate objc; // v0.2.7

pub mod cmd;
pub mod common;
pub mod connections;
pub mod discovery;
pub mod editor;
pub mod file_server;
pub mod keychain;
pub mod main_window;
pub mod preferences;
pub mod runtime_state;
pub mod shell_control;
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
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::Manager;
use tauri::State;
use tauri::Webview;
use tauri::WebviewWindow;
use tauri::Wry;
use tauri::ipc::Invoke;

#[derive(Parser, Debug)]
#[command(author, version = include_str!(concat!(env!("OUT_DIR"), "/long_version.txt")), about, long_about = None)]
struct Args {
    /// Open a session with the given transport. Supported schemes:
    ///   `local`, `pkexec` (Linux), `ssh:user@host`, `ssh-agent:user@host`,
    ///   `docker:[user@]<container>` (bootstrapless by default),
    ///   `docker-bootstrap:[user@]<container>` (cached sh bootstrap),
    ///   `podman:[user@]<container>`, `podman-bootstrap:[user@]<container>`,
    ///   `kube:[context/][namespace/]pod[:container]`,
    ///   `custom:<shell command using $NEWT_BOOTSTRAP>`,
    ///   `custom-raw:<shell command that already spawns an agent>`.
    #[arg(long, value_name = "SCHEME:SPEC", conflicts_with = "profile")]
    target: Option<String>,

    /// Open with a saved connection profile (by `name` or `id`). Spawn-style
    /// profiles (ssh/docker/podman/kube/custom) open a new window; for S3/SFTP
    /// profiles use Quick Connect inside the app.
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,

    /// Open a session inside a WSL distribution (Windows only). Bare
    /// `--wsl` uses the default distro; `--wsl <NAME>` targets a specific
    /// one. There are no saved WSL profiles.
    #[cfg(windows)]
    #[arg(
        long,
        value_name = "NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with_all = ["target", "profile"]
    )]
    wsl: Option<String>,

    /// Open an elevated session (Linux: pkexec; Windows: UAC).
    #[cfg(windows)]
    #[arg(long, conflicts_with_all = ["target", "profile", "wsl"])]
    elevated: bool,

    /// Open an elevated session (Linux: pkexec; Windows: UAC).
    #[cfg(target_os = "linux")]
    #[arg(long, conflicts_with_all = ["target", "profile"])]
    elevated: bool,

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

    /// Export tauri-specta bindings to PATH and exit (xtask use).
    #[cfg(feature = "specta-bindings")]
    #[arg(long, value_name = "PATH")]
    export_bindings: Option<std::path::PathBuf>,

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
    agent_resolver: OnceLock<Arc<dyn AgentResolver>>,
    preferences: OnceLock<preferences::PreferencesManager>,
    runtime_state: OnceLock<runtime_state::RuntimeStateManager>,
    /// A quit is in flight, waiting for the editor sweep (unsaved-changes
    /// prompts) to finish before the main windows close.
    pending_quit: AtomicBool,
    /// Main windows whose close is waiting for their own editor children's
    /// sweep to finish.
    pending_close: Mutex<HashSet<String>>,
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
            runtime_state: OnceLock::new(),
            pending_quit: AtomicBool::new(false),
            pending_close: Mutex::new(HashSet::new()),
            #[cfg(target_os = "macos")]
            window_menus: Mutex::new(HashMap::new()),
        }
    }
}

impl GlobalContext {
    pub fn init_agent_resolver(&self, app_handle: &tauri::AppHandle) {
        self.agent_resolver
            .set(Arc::new(TauriAgentResolver::new(app_handle)))
            .ok();
    }

    pub fn agent_resolver(&self) -> Arc<dyn AgentResolver> {
        self.agent_resolver
            .get()
            .expect("AgentResolver not initialized")
            .clone()
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

    /// Must run after `init_preferences` — reuses its resolved config dir.
    pub fn init_runtime_state(&self, app_handle: &tauri::AppHandle) {
        self.runtime_state
            .set(runtime_state::RuntimeStateManager::new(
                app_handle,
                self.preferences().config_dir(),
            ))
            .ok();
    }

    pub fn runtime_state(&self) -> &runtime_state::RuntimeStateManager {
        self.runtime_state
            .get()
            .expect("RuntimeStateManager not initialized")
    }

    pub fn main_window(&self, webview: &Webview) -> Option<MainWindowContext> {
        self.main_windows.lock().get(webview.label()).cloned()
    }

    pub fn main_window_by_label(&self, label: &str) -> Option<MainWindowContext> {
        self.main_windows.lock().get(label).cloned()
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
        self.pending_close.lock().remove(label);
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

    /// Labels of real main windows. `main_windows` also aliases viewer/editor
    /// child labels to their parent's ctx, so filter to self-parented entries.
    pub fn real_main_window_labels(&self) -> Vec<String> {
        self.main_windows
            .lock()
            .iter()
            .filter(|(k, ctx)| *k == ctx.main_window_label())
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Editor windows that are actually showing a document (not pre-warmed
    /// hidden spares).
    fn active_editor_labels(&self) -> Vec<String> {
        let prewarmed: Vec<String> = self
            .prewarmed_editors
            .lock()
            .values()
            .map(|pw| pw.label.clone())
            .collect();
        self.editor_windows
            .lock()
            .keys()
            .filter(|l| !prewarmed.contains(l))
            .cloned()
            .collect()
    }

    /// Of `labels` (child window labels), those parented to `main_label` —
    /// resolved through the parent-ctx aliases in `main_windows`.
    fn children_of(&self, labels: Vec<String>, main_label: &str) -> Vec<String> {
        let mains = self.main_windows.lock();
        labels
            .into_iter()
            .filter(|l| {
                mains
                    .get(l)
                    .is_some_and(|ctx| ctx.main_window_label() == main_label)
            })
            .collect()
    }

    /// Non-prewarmed editor windows parented to `main_label`.
    fn active_editor_children(&self, main_label: &str) -> Vec<String> {
        self.children_of(self.active_editor_labels(), main_label)
    }

    /// Non-prewarmed viewer windows parented to `main_label`.
    fn active_viewer_children(&self, main_label: &str) -> Vec<String> {
        let prewarmed: Vec<String> = self
            .prewarmed_viewers
            .lock()
            .values()
            .map(|pw| pw.label.clone())
            .collect();
        let viewers: Vec<String> = self
            .viewer_windows
            .lock()
            .keys()
            .filter(|l| !prewarmed.contains(l))
            .cloned()
            .collect();
        self.children_of(viewers, main_label)
    }

    /// Close (not destroy) every real main window; the app exits when the
    /// last one is destroyed, taking viewer/editor children with it.
    ///
    /// Open editors are swept first so their unsaved-changes prompts fire:
    /// the quit resumes from the `Destroyed` handler once the last editor is
    /// gone, and `cancel_pending_quit` aborts it when a prompt is refused.
    pub fn quit(&self, app_handle: &tauri::AppHandle) {
        let editors = self.active_editor_labels();
        if editors.is_empty() {
            self.pending_quit.store(false, Ordering::SeqCst);
            for label in self.real_main_window_labels() {
                if let Some(window) = app_handle.get_webview_window(&label) {
                    let _ = window.close();
                }
            }
        } else if !self.pending_quit.swap(true, Ordering::SeqCst) {
            // Not already sweeping — a second quit while a prompt is up
            // must not stack another prompt onto each dirty editor.
            for label in editors {
                if let Some(window) = app_handle.get_webview_window(&label) {
                    let _ = window.close();
                }
            }
        }
    }

    /// An editor refused to close: abort any pending quit and any pending
    /// close of its parent main window.
    pub fn cancel_pending_quit(&self, editor_label: &str) {
        self.pending_quit.store(false, Ordering::SeqCst);
        let parent = self
            .main_windows
            .lock()
            .get(editor_label)
            .map(|ctx| ctx.main_window_label().to_string());
        if let Some(parent) = parent {
            self.pending_close.lock().remove(&parent);
        }
    }

    /// Any editor window whose frontend has reported unsaved changes.
    pub fn any_dirty_editor(&self) -> bool {
        self.editor_windows
            .lock()
            .values()
            .any(|ctx| ctx.0.is_dirty())
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

/// macOS Dock quit / logout sends `terminate:` to NSApp, and tao's app
/// delegate doesn't implement `applicationShouldTerminate:` — Cocoa's default
/// (`NSTerminateNow`) then kills the process before the event loop hears
/// anything: no window-close events, no unsaved-changes prompts. This is a
/// known Tauri gap, not a misuse of the API: `RunEvent::ExitRequested` is
/// emitted only for `exit()` and last-window-destroyed, never for OS
/// termination — tracked upstream in
/// <https://github.com/tauri-apps/tauri/issues/9198>.
///
/// Until that lands, inject the method into the delegate class at runtime: a
/// dirty editor cancels the termination and runs the graceful quit (editor
/// sweep) instead, while a clean app lets the termination proceed so it never
/// blocks logout. If a tao upgrade starts implementing the selector itself,
/// `install` panics on `class_addMethod` — delete this module then; the
/// `ExitRequested { code: None }` arm in the run callback picks up the
/// mapped event.
#[cfg(target_os = "macos")]
// objc 0.2's macros expand `cfg(feature = "cargo-clippy")` checks that
// trip `unexpected_cfgs` on modern rustc.
#[allow(unexpected_cfgs)]
mod terminate_guard {
    use std::os::raw::c_char;
    use std::sync::OnceLock;

    use objc::runtime::{BOOL, Class, Imp, NO, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use tauri::Manager;

    static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

    const NS_TERMINATE_CANCEL: usize = 0;
    const NS_TERMINATE_NOW: usize = 1;

    extern "C" fn application_should_terminate(
        _this: &mut Object,
        _sel: Sel,
        _sender: *mut Object,
    ) -> usize {
        let app_handle = APP_HANDLE.get().unwrap();
        let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
        if global_ctx.any_dirty_editor() {
            global_ctx.quit(app_handle);
            NS_TERMINATE_CANCEL
        } else {
            NS_TERMINATE_NOW
        }
    }

    pub fn install(app_handle: &tauri::AppHandle) {
        APP_HANDLE.set(app_handle.clone()).ok();
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let delegate: *mut Object = msg_send![app, delegate];
            let class = (*delegate).class() as *const Class as *mut Class;
            let imp = std::mem::transmute::<
                extern "C" fn(&mut Object, Sel, *mut Object) -> usize,
                Imp,
            >(application_should_terminate);
            let added: BOOL = objc::runtime::class_addMethod(
                class,
                sel!(applicationShouldTerminate:),
                imp,
                // NSApplicationTerminateReply (NSUInteger) (id, SEL, id)
                c"Q@:@".as_ptr() as *const c_char,
            );
            assert_ne!(
                added, NO,
                "delegate already implements applicationShouldTerminate:"
            );
        }
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

/// Turn off the browser-isms WebView2 (Edge Chromium) ships enabled:
///
/// * Autofill — a "saved info" popover appears over text inputs (path
///   bar, rename dialog, …). `autocomplete="off"` only partially
///   suppresses it — Chromium ignores it for many field kinds — so turn
///   the feature off at the settings level (`ICoreWebView2Settings4`).
/// * Browser accelerator keys — Ctrl+P (print!), Ctrl+F, F5, Ctrl+S, …
///   fire whenever the frontend doesn't consume the keydown
///   (`ICoreWebView2Settings3`). Editing keys (Ctrl+C/V/X/A) are
///   unaffected. This also kills F12/Ctrl+Shift+I for DevTools; debug
///   builds keep the Debug dialog's Toggle DevTools button (F12 opens
///   the dialog — the app command, not the browser accelerator).
///
/// Each cast no-ops independently if the installed runtime is too old.
#[cfg(windows)]
pub(crate) fn tune_webview_settings(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Settings3, ICoreWebView2Settings4,
    };
    use windows::core::Interface;

    let _ = window.with_webview(|pw| unsafe {
        let Ok(core) = pw.controller().CoreWebView2() else {
            return;
        };
        let Ok(settings) = core.Settings() else {
            return;
        };
        if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
            let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
        }
        if let Ok(s4) = settings.cast::<ICoreWebView2Settings4>() {
            let _ = s4.SetIsGeneralAutofillEnabled(false);
            let _ = s4.SetIsPasswordAutosaveEnabled(false);
        }
    });
}

#[cfg(not(windows))]
pub(crate) fn tune_webview_settings(_window: &tauri::WebviewWindow) {}

/// Spawn a task that keeps `window`'s title-bar theme in sync with the
/// theme preference. Shared by the main window and the child
/// (viewer/editor) windows so the resolution + subscription lives in one
/// place. Self-terminating: once the window is gone `set_theme` errors,
/// which ends the loop — so per-window tasks don't accumulate (child
/// windows are created/destroyed on every F3/F4).
pub(crate) fn spawn_theme_sync(
    window: &tauri::WebviewWindow,
    prefs: crate::preferences::PreferencesHandle,
) {
    let mut prefs_rx = prefs.subscribe();
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        while prefs_rx.changed().await.is_ok() {
            let theme = prefs
                .load()
                .appearance
                .theme
                .to_tauri_theme()
                .or_else(detect_theme);
            if window.set_theme(theme).is_err() {
                break;
            }
        }
    });
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
    let agent_hash = tauri::async_runtime::block_on(global_ctx.agent_resolver().agent_hash())
        .unwrap_or_else(|e| format!("(unavailable: {})", e));
    println!("Agents hash: {}", agent_hash);
}

/// Resolve a `--profile` argument against the saved-connections store. Only
/// spawn-style profiles (ssh/docker/podman/kube/custom) map cleanly to a
/// startup `ConnectionTarget`; S3/SFTP profiles need an existing local session
/// to mount onto, so we point the user at Quick Connect for those.
fn resolve_profile(
    config_dir: &std::path::Path,
    name: &str,
) -> Result<(ConnectionTarget, String), Error> {
    let profile = crate::connections::list_connections(config_dir)
        .into_iter()
        .find(|p| p.name == name || p.id == name)
        .ok_or_else(|| Error::Custom(format!("connection profile '{}' not found", name)))?;
    match crate::connections::connection_target_for(&profile.kind) {
        Some((ct, _label)) => Ok((ct, profile.name)),
        None => Err(Error::Custom(format!(
            "profile '{}' is not a spawn-style profile; open it via Quick Connect inside the app",
            name
        ))),
    }
}

/// Parse a `--target <scheme>:<spec>` argument into a startup `ConnectionTarget`.
/// See the doc on `Args::target` for the supported schemes.
fn parse_target(s: &str) -> Result<(ConnectionTarget, String), Error> {
    use crate::connections::{ConnectionKind, connection_target_for};

    // Schemes that take no spec.
    match s {
        "local" => return Ok((ConnectionTarget::Local, "Newt".to_string())),
        "pkexec" => {
            if cfg!(not(target_os = "linux")) {
                return Err(Error::Custom("pkexec is only supported on Linux".into()));
            }
            return Ok((ConnectionTarget::Elevated, "Elevated".to_string()));
        }
        // Cross-platform spelling: pkexec on Linux, UAC on Windows.
        "elevated" => {
            if cfg!(not(any(target_os = "linux", windows))) {
                return Err(Error::Custom(
                    "elevated mode is not supported on this platform".into(),
                ));
            }
            return Ok((ConnectionTarget::Elevated, "Elevated".to_string()));
        }
        _ => {}
    }

    let (scheme, spec) = s.split_once(':').ok_or_else(|| {
        Error::Custom(format!(
            "--target value {:?} is missing a `:<spec>` (try `ssh:user@host` or `--help`)",
            s
        ))
    })?;
    if spec.is_empty() {
        return Err(Error::Custom(format!(
            "--target=`{}:` is missing its spec",
            scheme
        )));
    }

    let kind = match scheme {
        "ssh" => ConnectionKind::Ssh {
            host: spec.to_string(),
            forward_agent: false,
            login_shell: true,
        },
        "ssh-agent" => ConnectionKind::Ssh {
            host: spec.to_string(),
            forward_agent: true,
            login_shell: true,
        },
        "docker" | "docker-bootstrap" | "podman" | "podman-bootstrap" => {
            let (user, container) = match spec.split_once('@') {
                Some((u, c)) if !u.is_empty() && !c.is_empty() => {
                    (Some(u.to_string()), c.to_string())
                }
                _ => (None, spec.to_string()),
            };
            // Docker / Podman containers are typically on the local engine —
            // `cp` + direct exec is fast, has fewer moving parts, and works
            // for sh-less images. `-bootstrap` opts into the cached sh-based
            // path (useful when the agent already exists in the container's
            // cache and you want to skip the re-upload).
            let bootstrapless = !scheme.ends_with("-bootstrap");
            if scheme.starts_with("docker") {
                ConnectionKind::Docker {
                    container,
                    user,
                    bootstrapless,
                }
            } else {
                ConnectionKind::Podman {
                    container,
                    user,
                    bootstrapless,
                }
            }
        }
        "kube" => parse_kube_spec(spec)?,
        "custom" => ConnectionKind::Custom {
            command: spec.to_string(),
            skip_bootstrap: false,
        },
        "custom-raw" => ConnectionKind::Custom {
            command: spec.to_string(),
            skip_bootstrap: true,
        },
        other => {
            return Err(Error::Custom(format!(
                "unknown --target scheme {:?} (expected ssh / ssh-agent / docker / docker-bootstrap / podman / podman-bootstrap / kube / custom / custom-raw / local / pkexec)",
                other
            )));
        }
    };

    connection_target_for(&kind)
        .ok_or_else(|| Error::Custom("internal: scheme did not produce a spawn target".into()))
}

/// Parse the kube spec: `[context/][namespace/]pod[:container]`.
fn parse_kube_spec(spec: &str) -> Result<crate::connections::ConnectionKind, Error> {
    let (rest, container) = match spec.split_once(':') {
        Some((r, c)) if !c.is_empty() => (r, Some(c.to_string())),
        _ => (spec, None),
    };
    let segments: Vec<&str> = rest.split('/').collect();
    let (context, namespace, pod) = match segments.as_slice() {
        [pod] => (None, None, (*pod).to_string()),
        [ns, pod] => (None, Some((*ns).to_string()), (*pod).to_string()),
        [ctx, ns, pod] => (
            Some((*ctx).to_string()),
            Some((*ns).to_string()),
            (*pod).to_string(),
        ),
        _ => {
            return Err(Error::Custom(format!(
                "kube spec {:?} should be `[context/][namespace/]pod[:container]`",
                spec
            )));
        }
    };
    if pod.is_empty() {
        return Err(Error::Custom("kube pod name is empty".into()));
    }
    Ok(crate::connections::ConnectionKind::Kube {
        context,
        namespace,
        pod,
        container,
    })
}

fn main() {
    // Shell-integration CLI mode: inside a Newt terminal (NEWT_SHELL_SOCK
    // set) with a known verb as argv[1], `newt pwd` etc. remote-control the
    // owning session instead of launching the app. The verb requirement
    // keeps ordinary launches (`newt --target ssh:…`) untouched even when
    // run from an integrated terminal.
    if newt_common::shell_control::is_cli_invocation(true, true) {
        std::process::exit(newt_common::shell_control::run_cli());
    }

    let args = Args::parse();

    #[cfg(feature = "specta-bindings")]
    if let Some(path) = &args.export_bindings {
        cmd::create_specta_builder()
            .export(cmd::typescript_export_config(), path)
            .expect("failed to export tauri-specta bindings");
        return;
    }
    apply_log_flags(args.verbose, args.quiet);
    pretty_env_logger::init();

    // Before tauri::Builder spawns anything — `set_var` is not thread-safe.
    newt_common::locale::ensure_locale();

    // Connection target for the non-profile cases — `--profile` is resolved
    // inside `setup` once the preferences directory is known.
    let non_profile: Option<(ConnectionTarget, String)> = match (&args.target, &args.profile) {
        (Some(spec), _) => match parse_target(spec) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("newt: {}", e);
                std::process::exit(2);
            }
        },
        (None, None) => Some((ConnectionTarget::Local, "Newt".to_string())),
        (None, Some(_)) => None,
    };

    // `--wsl[=NAME]` resolves to a WSL target at launch. It conflicts with
    // `--target`/`--profile`, so `non_profile` is the Local default here.
    #[cfg(windows)]
    let non_profile = match &args.wsl {
        Some(name) => {
            let installed = discovery::wsl::list_distros();
            if installed.is_empty() {
                eprintln!("newt: no WSL distributions installed");
                std::process::exit(2);
            }
            let distro = if name.is_empty() {
                installed
                    .iter()
                    .find(|d| d.is_default)
                    .unwrap_or(&installed[0])
                    .name
                    .clone()
            } else {
                // Validate up front so a typo fails fast with the list,
                // rather than surfacing as an opaque WslLaunch error.
                match installed.iter().find(|d| d.name == *name) {
                    Some(d) => d.name.clone(),
                    None => {
                        eprintln!(
                            "newt: no WSL distribution named {:?}. Installed: {}",
                            name,
                            installed
                                .iter()
                                .map(|d| d.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        std::process::exit(2);
                    }
                }
            };
            // Bare label — the setup hook wraps it into `Newt [<label>]`.
            Some((
                ConnectionTarget::Wsl {
                    distro: distro.clone(),
                },
                format!("WSL: {}", distro),
            ))
        }
        None => non_profile,
    };

    // `--elevated` (pkexec on Linux, UAC on Windows). Conflicts with
    // `--target`/`--profile`/`--wsl`, so this just overrides the default.
    #[cfg(any(target_os = "linux", windows))]
    let non_profile = if args.elevated {
        Some((ConnectionTarget::Elevated, "Elevated".to_string()))
    } else {
        non_profile
    };

    let specta_builder = cmd::create_specta_builder();

    // Bindings are regenerated out-of-band by `cargo xtask gen-bindings`
    // (see `--export-bindings` above), not on every startup.

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
            global_ctx.init_runtime_state(app.handle());

            if print_config {
                print_resolved_config(&global_ctx);
                app.handle().exit(0);
                return Ok(());
            }

            // Resolve `--profile` now that the preferences manager (and thus
            // the config dir) is available; otherwise use the target picked
            // out of `--target` / default-local above.
            let (ct, default_title) = match (&profile_arg, &non_profile) {
                (Some(name), _) => {
                    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
                    resolve_profile(&config_dir, name)?
                }
                (None, Some(pair)) => pair.clone(),
                (None, None) => {
                    unreachable!("non_profile is None only when profile_arg is Some")
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

            // Menu accelerators mirror the resolved keybindings, so rebuild
            // the window menus whenever preferences change.
            #[cfg(target_os = "macos")]
            {
                let app_handle = app.handle().clone();
                let mut prefs_rx = global_ctx.preferences().handle().subscribe();
                tauri::async_runtime::spawn(async move {
                    while prefs_rx.changed().await.is_ok() {
                        main_window::menu::rebuild_all(&app_handle);
                    }
                });
            }

            // Drive letters change under us (USB, net use); refresh the
            // affected mounts' roots when Windows broadcasts it.
            #[cfg(windows)]
            main_window::drives::spawn_drive_watcher(app.handle().clone());

            Ok(())
        })
        .on_window_event(
            #[allow(clippy::single_match)]
            |window, event| {
                let app_handle = window.app_handle();
                let global_ctx: State<GlobalContext> = app_handle.state();

                match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // Viewer/editor children can't outlive the session
                        // they were spawned from, so a closing main window
                        // takes them along — editors via the graceful sweep
                        // (unsaved-changes prompts): the close is prevented,
                        // resumes from the Destroyed handler once the last
                        // child editor is gone, and is aborted by
                        // `cancel_quit` when a prompt is refused.
                        let label = window.label();
                        let is_real_main = global_ctx
                            .main_windows
                            .lock()
                            .get(label)
                            .is_some_and(|ctx| ctx.main_window_label() == label);
                        if is_real_main {
                            for viewer in global_ctx.active_viewer_children(label) {
                                if let Some(w) = app_handle.get_webview_window(&viewer) {
                                    let _ = w.close();
                                }
                            }
                            let editors = global_ctx.active_editor_children(label);
                            if !editors.is_empty() {
                                api.prevent_close();
                                global_ctx.pending_close.lock().insert(label.to_string());
                                for editor in editors {
                                    if let Some(w) = app_handle.get_webview_window(&editor) {
                                        let _ = w.close();
                                    }
                                }
                            }
                        }
                    }
                    tauri::WindowEvent::Destroyed => {
                        global_ctx.destroy_window(window.label()).unwrap();
                        // A pending quit resumes once its editor sweep is
                        // done (the last editor window is gone).
                        if global_ctx.pending_quit.load(Ordering::SeqCst)
                            && global_ctx.active_editor_labels().is_empty()
                        {
                            global_ctx.quit(app_handle);
                        }
                        // Per-window closes resume once their own sweep is
                        // done.
                        let pending: Vec<String> =
                            global_ctx.pending_close.lock().iter().cloned().collect();
                        for main_label in pending {
                            if global_ctx.active_editor_children(&main_label).is_empty() {
                                global_ctx.pending_close.lock().remove(&main_label);
                                if let Some(w) = app_handle.get_webview_window(&main_label) {
                                    let _ = w.close();
                                }
                            }
                        }
                        // Exit when the last real main window is gone (on
                        // macOS, Tauri would otherwise keep the process alive
                        // per standard Cocoa behavior).
                        if global_ctx.real_main_window_labels().is_empty() {
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
                            tauri::async_runtime::spawn(async move {
                                // Catch-all for drive changes that don't
                                // broadcast WM_DEVICECHANGE (subst): sweep
                                // the roots on focus regain, like the pane
                                // auto-refresh below.
                                #[cfg(windows)]
                                main_window::drives::refresh_host_drive_mounts(&ctx).await;
                                ctx.refresh(false).await
                            });
                        }
                    }
                    tauri::WindowEvent::DragDrop(event) => {
                        use tauri::Emitter;
                        // Scoped to this window: `position` is window-relative,
                        // so a broadcast would have every other window resolve
                        // a drop target from coordinates that aren't theirs.
                        let label = window.label();
                        match event {
                            tauri::DragDropEvent::Enter { paths, position } => {
                                let _ = window.emit_to(
                                    label,
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
                                let _ = window.emit_to(
                                    label,
                                    "external-drag",
                                    serde_json::json!({
                                        "kind": "over",
                                        "x": position.x,
                                        "y": position.y,
                                    }),
                                );
                            }
                            tauri::DragDropEvent::Drop { paths, position } => {
                                let _ = window.emit_to(
                                    label,
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
                                let _ = window.emit_to(
                                    label,
                                    "external-drag",
                                    serde_json::json!({ "kind": "leave" }),
                                );
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            },
        )
        .invoke_handler(handler)
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| match event {
            // The app delegate exists only once the app has launched.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Ready => terminate_guard::install(app_handle),
            // Safety net: an exit request that isn't our own `exit(0)`
            // (which carries a code) while main windows are still open is
            // rerouted through the graceful quit (editor sweep).
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => {
                let global_ctx: State<GlobalContext> = app_handle.state();
                if !global_ctx.real_main_window_labels().is_empty() {
                    api.prevent_exit();
                    global_ctx.quit(app_handle);
                }
            }
            _ => {}
        });
}

#[cfg(test)]
mod target_tests {
    use super::*;
    use crate::connections::ConnectionKind;
    use crate::main_window::{ConnectionTarget, SpawnSpec};

    fn kind_of(target: &ConnectionTarget) -> &SpawnSpec {
        match target {
            ConnectionTarget::Spawn(s) => s,
            _ => panic!("expected spawn"),
        }
    }

    #[test]
    fn local_scheme() {
        let (ct, _) = parse_target("local").unwrap();
        assert!(matches!(ct, ConnectionTarget::Local));
    }

    #[test]
    fn ssh_scheme() {
        let (ct, label) = parse_target("ssh:alice@host.example").unwrap();
        assert_eq!(label, "alice@host.example");
        match kind_of(&ct) {
            SpawnSpec::Bootstrap {
                transport_cmd,
                askpass,
                ..
            } => {
                assert!(*askpass);
                assert_eq!(transport_cmd.first().unwrap(), "ssh");
                assert!(!transport_cmd.contains(&"-A".to_string()));
            }
            _ => panic!("expected bootstrap"),
        }
    }

    #[test]
    fn ssh_agent_scheme() {
        let (ct, _) = parse_target("ssh-agent:alice@host.example").unwrap();
        match kind_of(&ct) {
            SpawnSpec::Bootstrap { transport_cmd, .. } => {
                assert!(transport_cmd.contains(&"-A".to_string()));
            }
            _ => panic!("expected bootstrap"),
        }
    }

    #[test]
    fn docker_scheme_with_user() {
        let kind = match parse_kube_spec("ns/p:c") {
            Ok(k) => k,
            Err(e) => panic!("kube parse failed: {}", e),
        };
        assert!(matches!(
            kind,
            ConnectionKind::Kube {
                container: Some(_),
                namespace: Some(_),
                context: None,
                ..
            }
        ));
    }

    #[test]
    fn docker_defaults_to_bootstrapless() {
        let (ct, _) = parse_target("docker:nt").unwrap();
        assert!(matches!(kind_of(&ct), SpawnSpec::DirectCopy(_)));
    }

    #[test]
    fn docker_bootstrap_opts_into_sh_bootstrap() {
        let (ct, _) = parse_target("docker-bootstrap:nt").unwrap();
        match kind_of(&ct) {
            SpawnSpec::Bootstrap { transport_cmd, .. } => {
                assert_eq!(transport_cmd.first().unwrap(), "docker");
            }
            _ => panic!("expected sh bootstrap"),
        }
    }

    #[test]
    fn custom_command() {
        let (ct, _) = parse_target(r#"custom:ssh foo@bar "$NEWT_BOOTSTRAP""#).unwrap();
        match kind_of(&ct) {
            SpawnSpec::CustomShell {
                command,
                skip_bootstrap,
                ..
            } => {
                assert_eq!(command, r#"ssh foo@bar "$NEWT_BOOTSTRAP""#);
                assert!(!skip_bootstrap);
            }
            _ => panic!("expected custom shell"),
        }
    }

    #[test]
    fn custom_raw_command() {
        let (ct, _) = parse_target("custom-raw:my-pre-spawned-agent").unwrap();
        match kind_of(&ct) {
            SpawnSpec::CustomShell { skip_bootstrap, .. } => {
                assert!(skip_bootstrap);
            }
            _ => panic!("expected custom shell"),
        }
    }

    #[test]
    fn invalid_scheme() {
        assert!(parse_target("nope:thing").is_err());
        assert!(parse_target("ssh:").is_err());
        assert!(parse_target("nope").is_err());
    }

    #[test]
    fn kube_levels() {
        let k = parse_kube_spec("pod").unwrap();
        assert!(matches!(
            k,
            ConnectionKind::Kube {
                context: None,
                namespace: None,
                ..
            }
        ));
        let k = parse_kube_spec("ns/pod").unwrap();
        assert!(matches!(
            k,
            ConnectionKind::Kube {
                context: None,
                namespace: Some(_),
                container: None,
                ..
            }
        ));
        let k = parse_kube_spec("ctx/ns/pod:c").unwrap();
        assert!(matches!(
            k,
            ConnectionKind::Kube {
                context: Some(_),
                namespace: Some(_),
                container: Some(_),
                ..
            }
        ));
    }
}
