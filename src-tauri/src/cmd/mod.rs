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

pub mod dialog;
pub mod dnd;
pub mod operations;
pub mod pane;
pub mod preferences;
pub mod terminal;
pub mod vfs;
pub mod window;

use newt_common::filesystem::UserGroup;
use tauri::Manager;
use tauri::WebviewWindow;
use tauri::Wry;
use tauri::ipc::Invoke;
use tauri_specta::{Builder, collect_commands};

use crate::GlobalContext;
use crate::common::Error;
use crate::main_window::MainWindowContext;

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

    // Already connected (e.g. local mode via on_page_load).
    if ctx.is_connected() {
        return Ok(());
    }

    let agent_resolver = global_ctx.agent_resolver();
    if let Err(e) = ctx.connect(agent_resolver).await {
        ctx.set_connection_failed(e.to_string());
        return Err(e);
    }

    // Pre-warm viewer and editor windows now that we're connected
    let app_handle = webview.app_handle().clone();
    let main_label = ctx.main_window_label().to_string();
    prewarm_viewer(&app_handle, &ctx, &main_label);
    prewarm_editor(&app_handle, &ctx, &main_label);

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
            operations::confirm_action,
            dialog::dialog,
            window::close_window,
            window::destroy_window,
            window::set_window_title,
            window::zoom,
            // Pane interaction (called directly by frontend components)
            pane::cancel,
            pane::navigate,
            pane::enter,
            pane::focus,
            pane::set_sorting,
            pane::toggle_selected,
            pane::select_range,
            pane::set_selection,
            pane::set_selection_by_indices,
            pane::end_drag_selection,
            pane::relative_jump,
            pane::set_viewport,
            pane::set_filter,
            // File operations (called from dialog submissions)
            operations::create_directory,
            operations::touch_file,
            operations::rename,
            operations::set_metadata,
            operations::start_operation,
            operations::start_copy_move,
            operations::cancel_operation,
            operations::resolve_issue,
            operations::dismiss_operation,
            operations::background_operation,
            operations::foreground_operation,
            // File viewing/opening/editing
            operations::file_details,
            operations::read_file_range,
            operations::read_file,
            operations::write_file,
            // Viewer / Editor
            crate::viewer::set_viewer_mode,
            crate::viewer::ping_viewer,
            crate::viewer::copy_viewer_range,
            crate::viewer::find_in_viewer,
            crate::editor::set_editor_language,
            crate::editor::set_editor_wrap,
            crate::editor::ping_editor,
            window::reconnect,
            window::connect_target,
            crate::discovery::discover_ssh_hosts,
            crate::discovery::discover_docker_containers,
            crate::discovery::discover_podman_containers,
            crate::discovery::discover_kube_contexts,
            crate::discovery::discover_kube_pods,
            vfs::switch_vfs,
            vfs::unmount_vfs,
            // Terminal
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_focus,
            terminal::close_terminal,
            terminal::activate_terminal,
            // Drag & drop
            dnd::start_dnd,
            dnd::cancel_dnd,
            dnd::execute_dnd,
            dnd::external_drop,
            // Preferences
            preferences::get_preferences,
            preferences::update_preference,
            preferences::reset_preference,
            preferences::get_preferences_schema,
            preferences::set_command_keybinding,
            preferences::reset_command_keybinding,
            preferences::open_config_file,
            // Hot paths
            preferences::get_hot_paths,
            preferences::add_bookmark,
            preferences::remove_bookmark,
            // User commands
            crate::user_commands::run_user_command,
            crate::user_commands::execute_user_command,
            crate::user_commands::add_user_command_entry,
            crate::user_commands::remove_user_command_entry,
            crate::user_commands::update_user_command_entry,
            // cmd_* commands (palette / keyboard shortcut entry points)
            dialog::cmd_rename,
            dialog::cmd_properties,
            dialog::cmd_create_directory,
            dialog::cmd_create_file,
            dialog::cmd_create_and_edit,
            dialog::cmd_navigate,
            dialog::cmd_copy,
            dialog::cmd_move,
            dialog::cmd_connect_remote,
            dialog::cmd_select_vfs,
            dialog::cmd_command_palette,
            dialog::cmd_user_commands,
            dialog::cmd_directory_properties,
            dialog::cmd_open_settings,
            window::cmd_new_window,
            pane::cmd_toggle_hidden,
            window::cmd_close_window,
            window::cmd_view,
            window::cmd_edit,
            pane::cmd_open,
            pane::cmd_open_archive,
            pane::cmd_open_folder,
            pane::cmd_follow_symlink,
            pane::cmd_navigate_back,
            pane::cmd_navigate_forward,
            dialog::cmd_history_back,
            dialog::cmd_history_forward,
            dialog::cmd_history,
            pane::navigate_history,
            pane::delete_history_entry,
            pane::cmd_as_other_pane,
            pane::cmd_open_in_left_pane,
            pane::cmd_open_in_right_pane,
            pane::cmd_select_all,
            pane::cmd_deselect_all,
            pane::cmd_copy_to_clipboard,
            pane::cmd_paste_from_clipboard,
            terminal::cmd_send_to_terminal,
            terminal::cmd_toggle_terminal_panel,
            terminal::cmd_focus_panes,
            terminal::cmd_focus_terminal,
            terminal::cmd_create_terminal,
            terminal::cmd_next_terminal,
            terminal::cmd_prev_terminal,
            window::cmd_open_elevated,
            dialog::cmd_quick_connect,
            dialog::cmd_mount_s3,
            vfs::mount_s3,
            dialog::cmd_mount_sftp,
            dialog::cmd_mount_k8s,
            vfs::cmd_unmount_vfs,
            vfs::mount_sftp,
            vfs::mount_k8s,
            vfs::mount_search,
            dialog::cmd_start_search,
            dialog::cmd_hot_paths,
            preferences::cmd_add_bookmark,
            preferences::cmd_open_config_file,
            pane::cmd_refresh,
            operations::cmd_delete_selected,
            operations::cmd_show_next_operation,
            dialog::cmd_debug,
            operations::cmd_debug_run_test_operation,
            dialog::cmd_connection_log,
            dialog::cmd_about,
            // Keychain
            crate::keychain::keychain_get,
            crate::keychain::keychain_set,
            crate::keychain::keychain_delete,
            // Connections
            crate::connections::cmd_list_connections,
            crate::connections::cmd_save_connection,
            crate::connections::cmd_delete_connection,
            crate::connections::cmd_get_connection_secret,
            crate::connections::connect_profile,
        ])
        // Types that flow through state pushes (UpdatePublisher events) rather
        // than command return values. The frontend assembles MainWindowState
        // from these leaves; we don't codegen the wrapper because its Rust
        // representation uses Arc<RwLock<…>> wrappers with manual Serialize
        // impls that don't fit specta::Type cleanly.
        .typ::<newt_common::vfs::Breadcrumb>()
        .typ::<newt_common::filesystem::File>()
        .typ::<newt_common::filesystem::FileList>()
        .typ::<newt_common::filesystem::FsStats>()
        .typ::<crate::main_window::AskpassPrompt>()
        .typ::<crate::main_window::ConfirmAction>()
        .typ::<crate::main_window::DndData>()
        .typ::<crate::main_window::ModalData>()
        .typ::<crate::main_window::ModalDataKind>()
        .typ::<crate::main_window::ModalContext>()
        .typ::<crate::main_window::OperationIssueInfo>()
        .typ::<crate::main_window::OperationState>()
        .typ::<crate::main_window::OperationStatus>()
        .typ::<crate::main_window::UserCommandPrompt>()
        .typ::<crate::main_window::VfsTarget>()
        .typ::<newt_common::vfs::VfsProgress>()
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

/// `tauri-specta` Typescript export config. Shared between `main.rs` (debug
/// startup re-export) and the `export_typescript_bindings` test so the
/// emitted `bindings.ts` doesn't oscillate between two header strings.
pub fn typescript_export_config() -> specta_typescript::Typescript {
    specta_typescript::Typescript::default()
        .bigint(specta_typescript::BigIntExportBehavior::Number)
        .header(
            "/* eslint-disable */\n\
             // @ts-nocheck\n\
             // AUTO-GENERATED by tauri-specta — do not edit by hand.\n\
             // Regenerate with `cargo test -p newt export_typescript_bindings`.\n",
        )
}

pub const BINDINGS_PATH: &str = "../src/lib/bindings.ts";

#[cfg(test)]
mod tests {
    use super::*;

    /// Regenerate `src/lib/bindings.ts` from the Rust command registry as part
    /// of `cargo test`. CI runs `git diff --exit-code` after `cargo test` to
    /// fail the build if the committed bindings have drifted.
    #[test]
    fn export_typescript_bindings() {
        create_specta_builder()
            .export(typescript_export_config(), BINDINGS_PATH)
            .expect("failed to export tauri-specta bindings");
    }
}
