use newt_common::vfs::VfsPath;
use tauri::{Manager, WebviewWindow, Window};

use super::{EDITOR_WINDOW_SIZE, VIEWER_WINDOW_SIZE, show_prewarmed};
use crate::common::Error;
use crate::main_window::session::ConnectionTarget;
use crate::main_window::{MainWindowContext, PaneHandle};

/// Build a child WebviewWindow (viewer or editor) and register its parent
/// MainWindowContext under the new label so IPC commands resolve correctly.
///
/// The returned `(label, window)` is the caller's to register in the
/// kind-specific windows map (`viewer_windows` or `editor_windows`).
fn build_child_window(
    app_handle: &tauri::AppHandle,
    parent_ctx: &MainWindowContext,
    url: &str,
    title: &str,
    size: (f64, f64),
    visible: bool,
) -> Result<(String, tauri::WebviewWindow), Error> {
    let label = uuid::Uuid::new_v4().to_string();
    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();

    // Resolve the theme exactly as the main window does. This matters
    // beyond the title bar: the WebView2 color scheme is a process-wide
    // profile, and a window built without an explicit theme stamps it
    // back to the OS setting on creation — so a prewarmed F3/F4 window
    // (created in the background) would silently reset *every* window's
    // webview to system mode. Building them with the app theme keeps the
    // shared profile consistent.
    let theme = global_ctx
        .preferences()
        .handle()
        .load()
        .appearance
        .theme
        .to_tauri_theme()
        .or_else(crate::detect_theme);

    let mut builder =
        tauri::WebviewWindowBuilder::new(app_handle, &label, tauri::WebviewUrl::App(url.into()))
            .title(title)
            .inner_size(size.0, size.1)
            .theme(theme)
            .center();

    if visible {
        builder = builder.focused(true);
    } else {
        // Must explicitly drop focus: the builder defaults to focused(true),
        // and on Windows an invisible-but-focused window still becomes the
        // foreground window, stealing input from the window we just showed.
        builder = builder.visible(false).focused(false);
    }

    // Register the parent context only after the window builds successfully —
    // otherwise a build failure would leave a stale entry in `main_windows`
    // that no `WindowEvent::Destroyed` ever cleans up. The webview hasn't
    // started loading at this point, so no IPC has fired yet.
    let window = builder.build()?;
    crate::disable_webview_autofill(&window);

    // Live title-bar updates on theme-preference changes. (Webview
    // content already follows globally via the shared WebView2 profile /
    // window appearance.)
    crate::spawn_theme_sync(&window, global_ctx.preferences().handle());

    global_ctx
        .main_windows
        .lock()
        .insert(label.clone(), parent_ctx.clone());
    Ok((label, window))
}

/// Create a pre-warmed hidden viewer window for a main window.
pub(crate) fn prewarm_viewer(
    app_handle: &tauri::AppHandle,
    main_ctx: &MainWindowContext,
    main_label: &str,
) {
    let (label, window) = match build_child_window(
        app_handle,
        main_ctx,
        "/viewer",
        "Viewer",
        VIEWER_WINDOW_SIZE,
        false,
    ) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("pre-warm viewer for {}: build failed: {}", main_label, e);
            return;
        }
    };
    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
    let viewer = crate::viewer::create_viewer_window(&window);
    global_ctx.register_viewer_window(&label, crate::viewer::ViewerWindowContext(viewer));
    global_ctx.set_prewarmed_viewer(main_label, crate::PrewarmedWindow { label, window });
    log::debug!("pre-warmed viewer for {}", main_label);
}

/// Create a pre-warmed hidden editor window for a main window.
pub(crate) fn prewarm_editor(
    app_handle: &tauri::AppHandle,
    main_ctx: &MainWindowContext,
    main_label: &str,
) {
    let (label, window) = match build_child_window(
        app_handle,
        main_ctx,
        "/editor",
        "Editor",
        EDITOR_WINDOW_SIZE,
        false,
    ) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("pre-warm editor for {}: build failed: {}", main_label, e);
            return;
        }
    };
    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
    let editor = crate::editor::create_editor_window(&window);
    global_ctx.register_editor_window(&label, crate::editor::EditorWindowContext(editor));
    global_ctx.set_prewarmed_editor(main_label, crate::PrewarmedWindow { label, window });
    log::debug!("pre-warmed editor for {}", main_label);
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_view(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    if pane.is_focused_dir() {
        return Ok(());
    }
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return Ok(()),
    };

    let app_handle = ctx.window().app_handle().clone();
    let main_label = ctx.main_window_label().to_string();
    let path_display = ctx.format_vfs_path(&full_path);
    let file_server_base = ctx.file_server_base_url()?;
    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();

    // Try to use a pre-warmed viewer window
    if let Some(pw) = global_ctx.take_prewarmed_viewer(&main_label) {
        let viewer_ctx = global_ctx
            .viewer_window(&pw.label)
            .expect("pre-warmed viewer must be registered");

        viewer_ctx
            .0
            .set_file(full_path, path_display.clone(), file_server_base);
        crate::viewer::activate_viewer_window(&app_handle, &pw.label, &pw.window, &viewer_ctx.0)?;

        let _ = pw.window.set_title(&format!("{} - Viewer", path_display));
        show_prewarmed(&pw.window);
    } else {
        // Fallback: create the window visible right away.
        let title = format!("{} - Viewer", path_display);
        let (label, window) = build_child_window(
            &app_handle,
            &ctx,
            "/viewer",
            &title,
            VIEWER_WINDOW_SIZE,
            true,
        )?;
        let viewer = crate::viewer::create_viewer_window(&window);
        viewer.set_file(full_path, path_display, file_server_base);
        crate::viewer::activate_viewer_window(&app_handle, &label, &window, &viewer)?;
        global_ctx.register_viewer_window(&label, crate::viewer::ViewerWindowContext(viewer));
    }

    // Always replenish the pre-warm slot for the next F3.
    prewarm_viewer(&app_handle, &ctx, &main_label);

    Ok(())
}

pub(crate) fn open_editor_window(
    ctx: &MainWindowContext,
    full_path: &VfsPath,
) -> Result<(), Error> {
    let app_handle = ctx.window().app_handle().clone();
    let main_label = ctx.main_window_label().to_string();
    let path_display = ctx.format_vfs_path(full_path);
    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();

    // Try to use a pre-warmed editor window
    if let Some(pw) = global_ctx.take_prewarmed_editor(&main_label) {
        let editor_ctx = global_ctx
            .editor_window(&pw.label)
            .expect("pre-warmed editor must be registered");

        editor_ctx
            .0
            .set_file(full_path.clone(), path_display.clone());
        crate::editor::activate_editor_window(&app_handle, &pw.label, &pw.window, &editor_ctx.0)?;

        let _ = pw.window.set_title(&format!("{} - Editor", path_display));
        show_prewarmed(&pw.window);
    } else {
        // Fallback: create the window visible right away.
        let title = format!("{} - Editor", path_display);
        let (label, window) = build_child_window(
            &app_handle,
            ctx,
            "/editor",
            &title,
            EDITOR_WINDOW_SIZE,
            true,
        )?;
        let editor = crate::editor::create_editor_window(&window);
        editor.set_file(full_path.clone(), path_display);
        crate::editor::activate_editor_window(&app_handle, &label, &window, &editor)?;
        global_ctx.register_editor_window(&label, crate::editor::EditorWindowContext(editor));
    }

    // Always replenish the pre-warm slot for the next F4.
    prewarm_editor(&app_handle, ctx, &main_label);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_edit(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    if pane.is_focused_dir() {
        return Ok(());
    }
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return Ok(()),
    };

    open_editor_window(&ctx, &full_path)
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_new_window(
    webview: tauri::Webview,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    crate::main_window::spawn_main_window(
        webview.app_handle(),
        ConnectionTarget::Local,
        "Newt".to_string(),
        [None, None],
    )?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn reconnect(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.disconnect_for_reconnect().await;

    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
    let agent_resolver = global_ctx.agent_resolver();
    ctx.connect(agent_resolver).await?;

    // Prewarmed viewer/editor windows hold only UI state (file_path, mode,
    // etc. — all populated at activation via cmd_view / open_editor_window),
    // not session data, so they survive the session swap. No re-prewarm.
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn connect_target(
    webview: tauri::Webview,
    kind: crate::connections::ConnectionKind,
) -> Result<(), Error> {
    let (target, label) = crate::connections::connection_target_for(&kind)
        .ok_or_else(|| Error::Custom("not a spawn-style connection kind".into()))?;
    crate::main_window::spawn_main_window(
        webview.app_handle(),
        target,
        format!("Newt [{}]", label),
        [None, None],
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tauri::command]
#[specta::specta]
pub async fn cmd_open_elevated(
    webview: tauri::Webview,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    crate::main_window::spawn_main_window(
        webview.app_handle(),
        ConnectionTarget::Elevated,
        "Newt [Elevated]".to_string(),
        [None, None],
    )?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
#[specta::specta]
pub async fn cmd_open_elevated(_pane_handle: PaneHandle) -> Result<(), Error> {
    Err(Error::Custom(
        "Elevated mode is only supported on Linux".into(),
    ))
}

#[tauri::command]
#[specta::specta]
pub fn close_window(window: Window) -> Result<(), Error> {
    window.close()?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn destroy_window(window: Window) -> Result<(), Error> {
    window.destroy()?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_window_title(webview_window: WebviewWindow, title: String) -> Result<(), Error> {
    // NOTE: set_title doesn't visually update on Wayland (upstream Tauri/GTK bug).
    // Works on X11 and macOS. Keeping it so it works where it can.
    webview_window.set_title(&title)?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn cmd_close_window(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.window().close()?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn zoom(webview: tauri::Webview, factor: f64) -> Result<(), Error> {
    webview
        .set_zoom(factor)
        .map_err(|_| Error::Custom("zoom failed".into()))?;
    Ok(())
}
