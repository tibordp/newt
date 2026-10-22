//! Tauri command handlers + dispatch registry.
//!
//! The handlers are split by concern across submodules:
//!
//! - `pane`: pane interaction (selection, sorting, filtering, navigation)
//! - `window`: viewer/editor/main window lifecycle, plus session reconnect
//! - `operations`: file operations (copy/move/delete/rename/touch/chmod) +
//!   read/write helpers used by the viewer/editor
//! - `vfs`: mount/unmount/switch
//! - `dnd`: drag-and-drop start/cancel/execute, external drops
//! - `terminal`: PTY lifecycle and the terminal panel
//! - `preferences`: preferences, keybindings, hot paths, bookmarks
//! - `dialog`: the giant `dialog()` modal-opening function plus its
//!   `cmd_dialog!` shims
//!
//! `create_handler()` here glues all of those into a single Tauri invoke
//! handler with a small middleware that closes the current modal before
//! any `cmd_*` command runs.

pub mod clipboard;
pub mod dialog;
pub mod dnd;
pub mod operations;
pub mod pane;
pub mod preferences;
pub mod runtime_state;
pub mod terminal;
pub mod vfs;
pub mod window;

use newt_engine::vfs::UserGroup;
use tauri::Manager;
use tauri::WebviewWindow;
use tauri::Wry;
use tauri::ipc::Invoke;
use tauri_specta::{Builder, collect_commands};

use crate::GlobalContext;
use crate::common::Error;
use crate::main_window::{MainWindowContext, PaneHandle};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

/// Default size for newly-created viewer windows (file viewer, F3).
const VIEWER_WINDOW_SIZE: (f64, f64) = (1100.0, 800.0);

/// Default size for newly-created editor windows (F4).
const EDITOR_WINDOW_SIZE: (f64, f64) = (900.0, 700.0);

/// All POSIX permission bits (setuid/setgid/sticky + rwx for u/g/o).
const MODE_MASK: u32 = 0o7777;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn usergroup_id(ug: Option<&UserGroup>) -> Option<u32> {
    match ug {
        Some(UserGroup::Id(id)) => Some(*id),
        _ => None,
    }
}

fn show_prewarmed(window: &WebviewWindow) {
    let _ = window.show();
    let _ = window.set_focus();
}

// Re-exports for `main.rs` setup hooks.
pub(crate) use window::{prewarm_editor, prewarm_viewer};

// ---------------------------------------------------------------------------
// Lifecycle (init / askpass / ping / close_modal)
// ---------------------------------------------------------------------------

#[tauri::command]
#[specta::specta]
pub fn askpass_respond(ctx: MainWindowContext, response: Option<String>) -> Result<(), Error> {
    ctx.askpass_respond(response);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn init(
    webview: tauri::Webview,
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<(), Error> {
    let ctx = global_ctx
        .main_window(&webview)
        .ok_or_else(|| Error::Custom("window not initialized".into()))?;

    if !ctx.is_connected() {
        let agent_resolver = global_ctx.agent_resolver();
        if let Err(e) = ctx.connect(agent_resolver, false).await {
            ctx.set_connection_failed(e.to_string());
            return Err(e);
        }
    }

    // Prewarm only from here — never during setup: this command running
    // proves the main webview booted its frontend, i.e. webview creation
    // is long past its fragile early-composition phase (see the
    // blank-window note in `main.rs::setup`). Each prewarm in its own
    // spawn so neither blocks this command or the other. Guarded so a
    // webview reload doesn't leak a second set of hidden windows.
    let main_label = ctx.main_window_label().to_string();
    if !global_ctx.has_prewarmed(&main_label) {
        let app_handle = webview.app_handle().clone();
        let viewer_ctx = ctx.clone();
        let viewer_label = main_label.clone();
        tauri::async_runtime::spawn(async move {
            prewarm_viewer(&app_handle, &viewer_ctx, &viewer_label);
        });
        let app_handle = webview.app_handle().clone();
        let editor_ctx = ctx.clone();
        tauri::async_runtime::spawn(async move {
            prewarm_editor(&app_handle, &editor_ctx, &main_label);
        });
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn ping(
    webview: tauri::Webview,
    global_ctx: tauri::State<'_, GlobalContext>,
    name: String,
) -> Result<(), Error> {
    let label = webview.label();
    match name.as_str() {
        "viewer" => {
            if let Some(ctx) = global_ctx.viewer_window(label) {
                ctx.0.publish_full();
            }
        }
        "editor" => {
            if let Some(ctx) = global_ctx.editor_window(label) {
                ctx.0.publish_full();
            }
        }
        _ => {
            if let Some(ctx) = global_ctx.main_window(&webview) {
                ctx.publish_full()?;
            }
        }
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn close_modal(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_update(|gs| {
        gs.close_modal();
        Ok(())
    })
}

/// Hand a URL to the system's default browser: the UI's own outbound links
/// call this, and `confine_webview` sends navigations that would leave the
/// app the same way. Only web and mail URLs: the opener also launches local
/// files and programs, and the viewer renders links from files it doesn't
/// trust.
#[tauri::command]
#[specta::specta]
pub fn open_url(url: String) -> Result<(), Error> {
    open_external(&url)
}

/// Open a web or mail link with the system's handler; anything else is
/// refused.
pub fn open_external(url: &str) -> Result<(), Error> {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    if !matches!(scheme.as_deref(), Some("http" | "https" | "mailto")) {
        return Err(Error::Custom(format!("not a web or mail link: {url}")));
    }
    opener::open(url)?;
    Ok(())
}

const DOCS_URL: &str = "https://newt-fm.org/docs/";

#[tauri::command]
#[specta::specta]
pub fn cmd_documentation(_ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    opener::open(DOCS_URL)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri invoke handler — dispatch + middleware
// ---------------------------------------------------------------------------

/// Build the tauri-specta `Builder` with the full command registry. Callers
/// pull the invoke handler out via `.invoke_handler()` and (in debug builds)
/// emit `bindings.ts` via `.export(...)`.
pub fn create_specta_builder() -> Builder<Wry> {
    Builder::<Wry>::new()
        .commands(collect_commands![
            // Core / lifecycle
            init,
            askpass_respond,
            ping,
            close_modal,
            open_url,
            cmd_documentation,
            operations::confirm_delete,
            dialog::dialog,
            window::close_window,
            window::cancel_quit,
            window::destroy_window,
            window::set_window_title,
            window::zoom,
            // Pane interaction (called directly by frontend components)
            pane::cancel,
            pane::navigate,
            pane::navigate_to_path,
            pane::enter,
            pane::focus,
            pane::set_sorting,
            pane::toggle_selected,
            pane::select_range,
            pane::set_selection,
            pane::set_selection_by_indices,
            pane::end_drag_selection,
            pane::relative_jump,
            pane::wheel_select,
            pane::set_viewport,
            pane::set_filter,
            // File operations (called from dialog submissions)
            operations::create_directory,
            operations::touch_file,
            operations::rename,
            operations::set_metadata,
            operations::apply_properties,
            operations::start_operation,
            operations::start_copy_move,
            operations::start_create_archive,
            operations::cancel_operation,
            operations::resolve_issue,
            operations::dismiss_operation,
            operations::background_operation,
            operations::foreground_operation,
            // File viewing/opening/editing
            operations::file_info,
            operations::inspect_file,
            operations::read_file_range,
            operations::read_file,
            operations::write_file,
            // Viewer / Editor
            crate::viewer::set_viewer_mode,
            crate::viewer::set_viewer_dissect_formats,
            crate::viewer::ping_viewer,
            crate::viewer::copy_viewer_range,
            crate::viewer::sniff_viewer_encoding,
            crate::viewer::render_markdown,
            crate::viewer::report_table_detection,
            crate::viewer::open_in_viewer,
            crate::viewer::find_in_viewer,
            crate::viewer::image_exif,
            crate::viewer::dissect::dissect_open,
            crate::viewer::dissect::dissect_formats,
            crate::viewer::dissect::dissect_content_of,
            crate::viewer::dissect::dissect_reinterpret,
            crate::viewer::dissect::dissect_close,
            crate::viewer::dissect::dissect_stop,
            crate::viewer::dissect::dissect_children,
            crate::viewer::dissect::dissect_locate,
            crate::viewer::dissect::dissect_source,
            crate::viewer::dissect::dissect_read,
            crate::viewer::dissect::dissect_copy,
            crate::viewer::dissect::dissect_save,
            crate::viewer::dissect::dissect_answer_secret,
            crate::viewer::cmd_toggle_quick_view,
            crate::viewer::preview_focused,
            crate::viewer::set_preview_mode,
            crate::viewer::sniff_preview_encoding,
            crate::viewer::report_preview_table_detection,
            crate::viewer::open_viewer,
            crate::viewer::set_preview_encoding,
            crate::viewer::set_preview_table_option,
            crate::viewer::encoding_catalogue,
            crate::editor::set_editor_language,
            crate::editor::set_editor_wrap,
            crate::editor::set_editor_dirty,
            crate::editor::ping_editor,
            window::reconnect,
            window::connection_log,
            window::connect_target,
            crate::discovery::discover_ssh_hosts,
            crate::discovery::discover_docker_containers,
            crate::discovery::discover_podman_containers,
            crate::discovery::discover_kube_contexts,
            crate::discovery::discover_kube_pods,
            vfs::switch_vfs,
            vfs::unmount_vfs,
            vfs::cmd_map_network_drive,
            vfs::cmd_unmap_network_drive,
            vfs::confirm_unmap_drive,
            // Terminal
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_focus,
            terminal::close_terminal,
            terminal::read_clipboard_text,
            terminal::attach_terminal_output,
            terminal::activate_terminal,
            // Drag & drop
            dnd::start_dnd,
            dnd::cancel_dnd,
            dnd::execute_dnd,
            dnd::dnd_drag_out,
            dnd::external_drop,
            // Preferences
            preferences::get_preferences,
            preferences::update_preference,
            preferences::reset_preference,
            preferences::association_table,
            preferences::dismiss_config_problems,
            preferences::association_row,
            preferences::set_association,
            preferences::editor_languages,
            preferences::get_preferences_schema,
            preferences::set_command_keybindings,
            preferences::reset_command_keybinding,
            preferences::open_config_file,
            preferences::reveal_config_file,
            // Runtime state (ephemeral UI state, not settings.toml)
            runtime_state::get_runtime_state,
            runtime_state::update_runtime_state,
            runtime_state::forget_recent_connection,
            // Hot paths
            preferences::get_hot_paths,
            preferences::add_bookmark,
            preferences::remove_bookmark,
            preferences::undo_add_bookmark,
            // User commands
            crate::user_commands::run_user_command,
            crate::user_commands::execute_user_command,
            crate::user_commands::add_user_command_entry,
            crate::user_commands::remove_user_command_entry,
            crate::user_commands::update_user_command_entry,
            // cmd_* commands (palette / keyboard shortcut entry points)
            dialog::cmd_rename,
            dialog::cmd_batch_rename,
            dialog::batch_rename_set_rule,
            dialog::batch_rename_set_viewport,
            dialog::batch_rename_set_filter,
            dialog::batch_rename_find_problem,
            dialog::batch_rename_start,
            dialog::cmd_properties,
            dialog::cmd_create_directory,
            dialog::cmd_create_file,
            dialog::cmd_create_and_edit,
            dialog::cmd_navigate,
            dialog::cmd_copy,
            dialog::cmd_move,
            dialog::cmd_create_archive,
            dialog::cmd_connect_remote,
            dialog::cmd_select_vfs,
            dialog::cmd_sort,
            dialog::cmd_command_palette,
            dialog::cmd_user_commands,
            dialog::cmd_directory_properties,
            dialog::cmd_open_settings,
            dialog::cmd_change_association,
            dialog::open_settings_at,
            window::cmd_new_window,
            pane::cmd_toggle_hidden,
            pane::cmd_toggle_maximized,
            pane::cmd_start_filter,
            window::cmd_close_window,
            window::cmd_quit,
            window::cmd_view,
            window::cmd_edit,
            pane::cmd_open,
            pane::cmd_browse_into,
            pane::cmd_open_with,
            pane::open_with_apps,
            pane::open_with_app,
            pane::cmd_open_folder,
            pane::cmd_follow_symlink,
            pane::cmd_navigate_back,
            pane::cmd_navigate_root,
            pane::cmd_navigate_forward,
            dialog::cmd_history_back,
            dialog::cmd_history_forward,
            dialog::cmd_history,
            pane::navigate_history,
            pane::delete_history_entry,
            pane::cmd_as_other_pane,
            pane::cmd_swap_panes,
            pane::cmd_open_in_left_pane,
            pane::cmd_open_in_right_pane,
            pane::cmd_select_all,
            pane::cmd_deselect_all,
            pane::cmd_invert_selection,
            pane::cmd_toggle_selection,
            pane::cmd_select_same_extension,
            pane::select_by_pattern,
            pane::count_pattern_matches,
            dialog::cmd_select_by_pattern,
            dialog::cmd_deselect_by_pattern,
            pane::cmd_compute_size,
            pane::cmd_compute_all_sizes,
            clipboard::cmd_copy_to_clipboard,
            clipboard::cmd_cut_to_clipboard,
            clipboard::cmd_copy_path,
            clipboard::cmd_paste_from_clipboard,
            clipboard::cmd_move_from_clipboard,
            terminal::cmd_send_to_terminal,
            terminal::cmd_toggle_terminal_panel,
            terminal::cmd_focus_panes,
            terminal::cmd_focus_terminal,
            terminal::cmd_create_terminal,
            terminal::cmd_next_terminal,
            terminal::cmd_prev_terminal,
            window::cmd_open_elevated,
            window::cmd_connect_wsl,
            window::close_with_running_operations,
            window::connect_wsl_distro,
            dialog::cmd_quick_connect,
            dialog::cmd_mount_s3,
            vfs::mount_s3,
            dialog::cmd_mount_sftp,
            vfs::cmd_unmount_vfs,
            vfs::mount_sftp,
            vfs::mount_search,
            dialog::cmd_start_search,
            dialog::cmd_hot_paths,
            dialog::cmd_quick_open,
            dialog::quick_open_query,
            dialog::quick_open_accept,
            preferences::cmd_add_bookmark,
            preferences::cmd_open_config_file,
            pane::cmd_refresh,
            pane::shell_context_menu,
            operations::cmd_delete_selected,
            operations::cmd_delete_permanent,
            operations::cmd_show_next_operation,
            dialog::cmd_debug,
            operations::cmd_debug_run_test_operation,
            dialog::cmd_connection_log,
            dialog::cmd_about,
            // Keychain
            // Connections
            crate::connections::cmd_list_connections,
            crate::connections::cmd_save_connection,
            crate::connections::cmd_delete_connection,
            crate::connections::cmd_get_connection_secret,
            crate::connections::connect_profile,
            crate::connections::edit_connection,
            crate::connections::edit_recent_connection,
            crate::connections::connect_recent,
        ])
        // Types that flow through state pushes (UpdatePublisher events) rather
        // than command return values.
        .typ::<crate::main_window::MainWindowStateWire<'static>>()
        .typ::<crate::viewer::ViewerStateWire>()
        .typ::<crate::viewer::dissect::DissectProgress>()
        .typ::<crate::viewer::dissect::DissectChildrenAdded>()
        .typ::<crate::viewer::dissect::DissectTaskProgress>()
        .typ::<crate::editor::EditorStateWire>()
        .typ::<newt_engine::vfs::File>()
        .typ::<newt_engine::vfs::FileList>()
        .typ::<newt_engine::vfs::FsStats>()
        .typ::<crate::main_window::AskpassPrompt>()
        .typ::<crate::main_window::DeleteConfirmMode>()
        .typ::<crate::main_window::DndData>()
        .typ::<crate::main_window::ModalData>()
        .typ::<crate::main_window::ModalDataKind>()
        .typ::<crate::main_window::ModalContext>()
        .typ::<crate::main_window::MountSummary>()
        .typ::<crate::main_window::OperationIssueInfo>()
        .typ::<crate::main_window::OperationState>()
        .typ::<crate::main_window::OperationStatus>()
        .typ::<crate::main_window::UserCommandPrompt>()
        .typ::<crate::main_window::VfsTarget>()
        .typ::<newt_engine::vfs::MetadataTraits>()
        .typ::<newt_engine::vfs::VfsProgress>()
        .typ::<crate::main_window::pane::HistoryEntryView>()
        .typ::<crate::main_window::session::ConnectionStatus>()
        .typ::<crate::main_window::DisplayOptionsInner>()
}

/// Wrap a Tauri invoke handler with our `cmd_*` middleware: any command whose
/// name starts with `cmd_` (i.e. is a palette / shortcut entry point) closes
/// the current modal before the actual handler runs.
pub fn wrap_with_modal_close_middleware(
    inner: impl Fn(Invoke<Wry>) -> bool + Send + Sync + 'static,
) -> Box<dyn Fn(Invoke<Wry>) -> bool + Send + Sync + 'static> {
    Box::new(move |invoke: Invoke<Wry>| {
        if invoke.message.command().starts_with("cmd_") {
            let webview = invoke.message.webview();
            let app_handle = webview.app_handle().clone();
            let global_ctx: tauri::State<GlobalContext> = app_handle.state();
            if let Some(mwc) = global_ctx.main_window(&webview) {
                let _ = mwc.with_update(|gs| {
                    gs.close_modal();
                    Ok(())
                });
            }
        }
        inner(invoke)
    })
}

/// `tauri-specta` Typescript export config. Used by the `--export-bindings`
/// path (driven by `cargo xtask gen-bindings`).
pub fn typescript_export_config() -> specta_typescript::Typescript {
    specta_typescript::Typescript::default()
        .bigint(specta_typescript::BigIntExportBehavior::Number)
        .header(
            "/* eslint-disable */\n\
             // @ts-nocheck\n\
             // AUTO-GENERATED by tauri-specta — do not edit by hand.\n\
             // Regenerate with `cargo xtask gen-bindings`.\n",
        )
}

// ---------------------------------------------------------------------------
// Shell integration: run a palette command by id
// ---------------------------------------------------------------------------

/// Run the main-window command `id` (`user_command_N` included) on `pane`,
/// as the palette or a key binding would: as an invoke of its `cmd_*` (or
/// `run_user_command`) through the app's own invoke handler, so it gets the
/// same arguments, middleware and ACL. `Ok(false)` when there is no such
/// command.
pub async fn invoke_palette_command(
    ctx: &MainWindowContext,
    id: &str,
    pane: PaneHandle,
) -> Result<bool, Error> {
    use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponse};
    use tauri::webview::InvokeRequest;

    let app = ctx.window().app_handle().clone();
    let global: tauri::State<GlobalContext> = app.state();
    let known = global
        .preferences()
        .resolved()
        .commands
        .iter()
        .any(|c| c.id == id && c.scope == crate::preferences::commands::CommandScope::Main);
    if !known {
        return Ok(false);
    }
    let (cmd, body) = match id.strip_prefix("user_command_") {
        Some(index) => {
            let index: usize = index.parse().expect("user command ids are numbered");
            (
                "run_user_command".to_string(),
                serde_json::json!({ "paneHandle": pane, "index": index }),
            )
        }
        None => (
            format!("cmd_{id}"),
            serde_json::json!({ "paneHandle": pane }),
        ),
    };

    let webview: tauri::Webview = ctx.window().as_ref().clone();
    let request = InvokeRequest {
        cmd,
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: webview.url().map_err(|e| Error::Custom(e.to_string()))?,
        body: InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: app.invoke_key().to_string(),
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    webview.on_message(
        request,
        Box::new(move |_, _, response, _, _| {
            let _ = tx.send(response);
        }),
    );
    match rx.await.expect("an invoke is always answered") {
        InvokeResponse::Ok(_) => Ok(true),
        InvokeResponse::Err(e) => Err(Error::Custom(match e.0 {
            serde_json::Value::String(message) => message,
            other => other.to_string(),
        })),
    }
}

#[cfg(test)]
mod palette_command_tests {
    /// The palette, key bindings and `newt cmd` all run a main-window
    /// command by invoking `cmd_<id>` with just a pane handle; the generated
    /// bindings show whether each one is registered with that shape.
    #[test]
    fn every_main_command_has_its_cmd_handler() {
        let bindings = include_str!("../../../src/lib/bindings.ts");
        for def in crate::preferences::commands::default_commands() {
            if def.scope != crate::preferences::commands::CommandScope::Main {
                continue;
            }
            let invoke = format!("TAURI_INVOKE(\"cmd_{}\", {{ paneHandle }})", def.id);
            assert!(
                bindings.contains(&invoke),
                "no `cmd_{}(pane_handle)` command",
                def.id
            );
        }
    }
}

#[cfg(test)]
mod open_url_tests {
    use super::open_url;

    /// Rendered documents can carry any link; only web and mail URLs may
    /// reach the opener, which would launch local files and programs.
    #[test]
    fn refuses_what_is_not_a_web_or_mail_link() {
        for url in [
            "file:///etc/passwd",
            "/Applications/Calculator.app",
            "C:\\Windows\\System32\\calc.exe",
            "javascript:alert(1)",
            "smb://host/share",
            "relative/page.md",
        ] {
            assert!(open_url(url.to_string()).is_err(), "{url}");
        }
    }
}
