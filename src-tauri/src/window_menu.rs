//! A viewer's or editor's menu. Elsewhere it is the window's menu bar; on
//! GNOME, where apps have none, the header bar Newt draws offers it as its
//! primary menu (`TitleBar.tsx`) — the same `Menu`, described to the page,
//! and the same handler, called for the item the page activates.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::menu::{Menu, MenuItemKind};
use tauri::{Manager, WebviewWindow, Wry};

/// Called with the id of each item activated, from the menu bar or the page.
pub type Handler = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
pub struct WindowMenus(Mutex<HashMap<String, Entry>>);

#[derive(Default)]
struct Entry {
    menu: Option<Menu<Wry>>,
    handler: Option<Handler>,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MenuEntry {
    Item {
        id: String,
        label: String,
        enabled: bool,
        /// A check item's state; `None` for a plain item.
        checked: Option<bool>,
    },
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<MenuEntry>,
    },
    Separator,
}

/// Shows `menu` as the window's menu, replacing any it had. On macOS the
/// menu bar is the app's, switched as windows take focus, so the callers
/// handle it there.
#[cfg(not(target_os = "macos"))]
pub fn set_menu(window: &WebviewWindow, menu: Menu<Wry>) {
    if crate::appearance::draws_header_bar() {
        entry(window, |e| e.menu = Some(menu));
    } else {
        let _ = window.set_menu(menu);
    }
}

/// Sets the handler the page's activations go to, which the window's menu
/// events go to as well.
pub fn set_handler(window: &WebviewWindow, handler: Handler) {
    if !crate::appearance::draws_header_bar() {
        return;
    }
    entry(window, |e| e.handler = Some(handler));
    let app_handle = window.app_handle().clone();
    let label = window.label().to_owned();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            app_handle.state::<WindowMenus>().0.lock().remove(&label);
        }
    });
}

fn entry(window: &WebviewWindow, f: impl FnOnce(&mut Entry)) {
    let menus = window.state::<WindowMenus>();
    let mut menus = menus.0.lock();
    f(menus.entry(window.label().to_owned()).or_default());
}

fn describe(items: Vec<MenuItemKind<Wry>>) -> Vec<MenuEntry> {
    items
        .into_iter()
        .filter_map(|item| {
            Some(match item {
                MenuItemKind::MenuItem(i) => MenuEntry::Item {
                    id: i.id().0.clone(),
                    label: i.text().ok()?,
                    enabled: i.is_enabled().unwrap_or(true),
                    checked: None,
                },
                MenuItemKind::Check(c) => MenuEntry::Item {
                    id: c.id().0.clone(),
                    label: c.text().ok()?,
                    enabled: c.is_enabled().unwrap_or(true),
                    checked: Some(c.is_checked().unwrap_or(false)),
                },
                MenuItemKind::Submenu(s) => MenuEntry::Submenu {
                    label: s.text().ok()?,
                    enabled: s.is_enabled().unwrap_or(true),
                    items: describe(s.items().ok()?),
                },
                // Other predefined items are the OS's own (macOS's edit
                // items), with nothing for the page to activate.
                MenuItemKind::Predefined(p) if p.text().is_ok_and(|t| t.is_empty()) => {
                    MenuEntry::Separator
                }
                MenuItemKind::Predefined(_) | MenuItemKind::Icon(_) => return None,
            })
        })
        .collect()
}

/// A native check item flips itself before its event arrives; the page's
/// activation does the same, so the handler reads the state it expects.
fn toggle_check(items: Vec<MenuItemKind<Wry>>, id: &str) -> bool {
    items.into_iter().any(|item| match item {
        MenuItemKind::Check(c) if c.id().0 == id => {
            let _ = c.set_checked(!c.is_checked().unwrap_or(false));
            true
        }
        MenuItemKind::Submenu(s) => toggle_check(s.items().unwrap_or_default(), id),
        _ => false,
    })
}

/// The window's menu, for its header bar; `None` if it has none.
#[tauri::command]
#[specta::specta]
pub async fn get_window_menu(webview_window: WebviewWindow) -> Option<Vec<MenuEntry>> {
    let menu = {
        let menus = webview_window.state::<WindowMenus>();
        let menus = menus.0.lock();
        menus.get(webview_window.label())?.menu.clone()?
    };
    Some(describe(menu.items().ok()?))
}

#[tauri::command]
#[specta::specta]
pub async fn activate_window_menu_item(webview_window: WebviewWindow, id: String) {
    let (menu, handler) = {
        let menus = webview_window.state::<WindowMenus>();
        let menus = menus.0.lock();
        let Some(entry) = menus.get(webview_window.label()) else {
            return;
        };
        (entry.menu.clone(), entry.handler.clone())
    };
    if let Some(menu) = menu {
        toggle_check(menu.items().unwrap_or_default(), &id);
    }
    if let Some(handler) = handler {
        handler(&id);
    }
}
