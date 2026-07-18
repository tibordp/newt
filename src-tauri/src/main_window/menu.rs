//! macOS main-window menu: the app menu plus session-level operations.
//!
//! Item activations are forwarded to the frontend as window-scoped
//! `menu-command` events carrying a command id, so they dispatch through the
//! exact same path as keybindings (`executeCommandById` → `cmd_*`). Item
//! accelerators are derived from the resolved keybindings at build time;
//! `rebuild_all` keeps them in sync when preferences change.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, State, Wry};

use crate::GlobalContext;
use crate::common::Error;

/// Separates the window label from the command id in menu item ids, so each
/// window's `on_menu_event` handler reacts only to its own menu.
const MENU_CMD_INFIX: &str = "::cmd::";

/// Convert a resolved binding ("meta+shift+w") into a muda accelerator
/// ("Cmd+Shift+W"). `None` (no accelerator) for keys menus can't express.
fn accelerator(key: &str) -> Option<String> {
    let mut parts: Vec<&str> = key.split('+').collect();
    let key = match parts.pop()? {
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => k.to_uppercase(),
        "," => "Comma".to_string(),
        k if k.starts_with('f') && k[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
            k.to_uppercase()
        }
        _ => return None,
    };
    let mut out = Vec::with_capacity(parts.len() + 1);
    for modifier in parts {
        out.push(match modifier {
            "meta" => "Cmd",
            "ctrl" => "Ctrl",
            "shift" => "Shift",
            "alt" => "Alt",
            _ => return None,
        });
    }
    out.push(&key);
    Some(out.join("+"))
}

fn build(app_handle: &AppHandle, label: &str) -> Result<Menu<Wry>, Error> {
    let global_ctx: State<GlobalContext> = app_handle.state();
    let resolved = global_ctx.preferences().resolved();
    let cmd_item = |id: &str, title: &str| {
        let accel = resolved
            .commands
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.shortcut.as_deref())
            .and_then(accelerator);
        MenuItem::with_id(
            app_handle,
            format!("{label}{MENU_CMD_INFIX}{id}"),
            title,
            true,
            accel.as_deref(),
        )
    };

    // First submenu is the application menu; macOS titles it with the app
    // name regardless of what is set here. No Hide/Hide Others: their fixed
    // Cmd+H would shadow the Toggle Hidden Files binding.
    let app_submenu = Submenu::with_items(
        app_handle,
        "Newt",
        true,
        &[
            &cmd_item("about", "About Newt")?,
            &PredefinedMenuItem::separator(app_handle)?,
            &cmd_item("open_settings", "Settings…")?,
            &PredefinedMenuItem::separator(app_handle)?,
            &cmd_item("quit", "Quit Newt")?,
        ],
    )?;

    let file_submenu = Submenu::with_items(
        app_handle,
        "File",
        true,
        &[
            &cmd_item("new_window", "New Window")?,
            &PredefinedMenuItem::separator(app_handle)?,
            &cmd_item("connect_remote", "Connect to Remote Host…")?,
            &PredefinedMenuItem::separator(app_handle)?,
            &cmd_item("close_window", "Close Window")?,
        ],
    )?;

    // Predefined items so macOS routes Cmd+C/V/X/A to the webview as native
    // events — without them clipboard keys die at the menu bar.
    let edit_submenu = Submenu::with_items(
        app_handle,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::cut(app_handle, None)?,
            &PredefinedMenuItem::copy(app_handle, None)?,
            &PredefinedMenuItem::paste(app_handle, None)?,
            &PredefinedMenuItem::select_all(app_handle, None)?,
        ],
    )?;

    let window_submenu = Submenu::with_items(
        app_handle,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app_handle, None)?,
            &PredefinedMenuItem::maximize(app_handle, Some("Zoom"))?,
        ],
    )?;

    Ok(Menu::with_items(
        app_handle,
        &[&app_submenu, &file_submenu, &edit_submenu, &window_submenu],
    )?)
}

/// Build and register this window's menu and its dispatch handler. The menu
/// is applied immediately (the window is frontmost at creation); afterwards
/// the `Focused` handler in `on_window_event` swaps it in per focus change.
pub fn setup(app_handle: &AppHandle, label: &str) -> Result<(), Error> {
    let global_ctx: State<GlobalContext> = app_handle.state();
    let menu = build(app_handle, label)?;
    global_ctx.set_window_menu(label, menu.clone());
    let _ = app_handle.set_menu(menu);

    let target = label.to_string();
    let prefix = format!("{label}{MENU_CMD_INFIX}");
    app_handle.on_menu_event(move |app_handle, event| {
        if let Some(command_id) = event.id().0.strip_prefix(&prefix) {
            let _ = app_handle.emit_to(&target, "menu-command", command_id);
        }
    });
    Ok(())
}

/// Rebuild every main window's menu (accelerators follow rebindings). Item
/// ids are stable, so the handlers registered in `setup` keep working.
pub fn rebuild_all(app_handle: &AppHandle) {
    let global_ctx: State<GlobalContext> = app_handle.state();
    for label in global_ctx.real_main_window_labels() {
        let menu = match build(app_handle, &label) {
            Ok(menu) => menu,
            Err(e) => {
                log::warn!("failed to rebuild menu for {label}: {e}");
                continue;
            }
        };
        global_ctx.set_window_menu(&label, menu.clone());
        let focused = app_handle
            .get_webview_window(&label)
            .is_some_and(|w| w.is_focused().unwrap_or(false));
        if focused {
            let _ = app_handle.set_menu(menu);
        }
    }
}
