use newt_common::vfs::VfsPath;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use tauri::ipc::CommandArg;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, Submenu};
use tauri::{Manager, State, WebviewWindow, Wry};

use crate::GlobalContext;
use crate::common::{Error, UpdatePublisher};

const MODES: &[(&str, &str)] = &[
    ("text", "Text"),
    ("hex", "Hex"),
    ("image", "Image"),
    ("audio", "Audio"),
    ("video", "Video"),
    ("pdf", "PDF"),
];

pub struct ViewerState {
    mode: RwLock<String>,
    file_path: RwLock<Option<VfsPath>>,
    display_path: RwLock<Option<String>>,
    file_server_base: RwLock<Option<String>>,
}

impl Serialize for ViewerState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ViewerState", 4)?;
        s.serialize_field("mode", &*self.mode.read())?;
        s.serialize_field("file_path", &*self.file_path.read())?;
        s.serialize_field("display_path", &*self.display_path.read())?;
        s.serialize_field("file_server_base", &*self.file_server_base.read())?;
        s.end()
    }
}

pub struct ViewerWindow {
    publisher: Arc<UpdatePublisher<ViewerState>>,
    menu: RwLock<Option<Menu<Wry>>>,
}

impl ViewerWindow {
    pub fn set_file(&self, file_path: VfsPath, display_path: String, file_server_base: String) {
        let state = self.publisher.state();
        *state.file_path.write() = Some(file_path);
        *state.display_path.write() = Some(display_path);
        *state.file_server_base.write() = Some(file_server_base);
        // Reset mode for new file
        *state.mode.write() = "text".to_string();
        let _ = self.publisher.publish_full();
    }

    pub fn set_mode(&self, mode: &str) {
        *self.publisher.state().mode.write() = mode.to_string();
        self.update_menu_checks(mode);
        let _ = self.publisher.publish_full();
    }

    pub fn publish_full(&self) {
        let _ = self.publisher.publish_full();
    }

    fn update_menu_checks(&self, active_mode: &str) {
        let menu_guard = self.menu.read();
        let menu = match menu_guard.as_ref() {
            Some(m) => m,
            None => return,
        };
        let suffix = format!("mode_{}", active_mode);
        if let Ok(items) = menu.items() {
            for item in &items {
                if let Some(submenu) = item.as_submenu()
                    && let Ok(sub_items) = submenu.items()
                {
                    for sub_item in &sub_items {
                        if let Some(check) = sub_item.as_check_menuitem() {
                            let _ = check.set_checked(check.id().as_ref().ends_with(&suffix));
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct ViewerWindowContext(pub Arc<ViewerWindow>);

impl<'de> CommandArg<'de, Wry> for ViewerWindowContext {
    fn from_command(
        command: tauri::ipc::CommandItem<'de, Wry>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        let window = command.message.webview();
        let app_handle = window.app_handle();
        let s: State<GlobalContext> = app_handle.state();

        s.viewer_window(window.label())
            .ok_or_else(|| tauri::ipc::InvokeError::from("viewer window not found"))
    }
}

/// Create a ViewerWindow with UpdatePublisher but no menu.
/// Used both for pre-warming and direct creation.
pub fn create_viewer_window(window: &WebviewWindow) -> Arc<ViewerWindow> {
    let state = ViewerState {
        mode: RwLock::new("text".to_string()),
        file_path: RwLock::new(None),
        display_path: RwLock::new(None),
        file_server_base: RwLock::new(None),
    };
    let publisher = Arc::new(UpdatePublisher::new(window.clone(), "viewer", state));

    Arc::new(ViewerWindow {
        publisher,
        menu: RwLock::new(None),
    })
}

/// Attach menu and register event handler. Called when showing the window.
pub fn activate_viewer_window(
    app_handle: &tauri::AppHandle,
    label: &str,
    window: &WebviewWindow,
    viewer: &Arc<ViewerWindow>,
) -> Result<(), Error> {
    let prefix = format!("viewer_{}_", label);
    let close_id = format!("{}close", prefix);
    let menu = build_menu(app_handle, &prefix)?;

    #[cfg(target_os = "macos")]
    {
        let global_ctx: State<GlobalContext> = app_handle.state();
        global_ctx.set_window_menu(label, menu.clone());
        let _ = app_handle.set_menu(menu.clone());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window.set_menu(menu.clone());
    }

    *viewer.menu.write() = Some(menu);

    // Register menu event handler — IDs are prefixed with the window label
    // so each handler only reacts to its own window's menu items.
    let viewer_weak = Arc::downgrade(viewer);
    let window_clone = window.clone();
    app_handle.on_menu_event(move |_app_handle, event| {
        let id = event.id().0.as_str();

        if id == close_id {
            let _ = window_clone.destroy();
            return;
        }

        // Only handle events with our prefix
        let suffix = match id.strip_prefix(prefix.as_str()) {
            Some(s) => s,
            None => return,
        };

        let viewer = match viewer_weak.upgrade() {
            Some(v) => v,
            None => return,
        };
        let mode = match suffix.strip_prefix("mode_") {
            Some(m) => m,
            None => return,
        };
        viewer.set_mode(mode);
    });

    Ok(())
}

fn build_menu(app_handle: &tauri::AppHandle, prefix: &str) -> Result<Menu<Wry>, Error> {
    let mode_items: Vec<CheckMenuItem<Wry>> = MODES
        .iter()
        .map(|(id, label)| {
            CheckMenuItem::with_id(
                app_handle,
                format!("{}mode_{}", prefix, id),
                *label,
                true,
                false,
                None::<&str>,
            )
            .unwrap()
        })
        .collect();

    let item_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = mode_items
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<Wry>)
        .collect();
    let view_submenu = Submenu::with_items(app_handle, "View", true, &item_refs)?;

    // Edit menu — predefined items so macOS routes Cmd+C/V/X/A to the webview
    #[cfg(target_os = "macos")]
    let edit_submenu = Submenu::with_items(
        app_handle,
        "Edit",
        true,
        &[
            &tauri::menu::PredefinedMenuItem::cut(app_handle, None)?,
            &tauri::menu::PredefinedMenuItem::copy(app_handle, None)?,
            &tauri::menu::PredefinedMenuItem::paste(app_handle, None)?,
            &tauri::menu::PredefinedMenuItem::select_all(app_handle, None)?,
        ],
    )?;

    let close_item = MenuItem::with_id(
        app_handle,
        format!("{}close", prefix),
        "Close",
        true,
        Some("Escape"),
    )?;
    let file_submenu = Submenu::with_items(app_handle, "File", true, &[&close_item])?;

    #[cfg(target_os = "macos")]
    let ret = Menu::with_items(app_handle, &[&file_submenu, &edit_submenu, &view_submenu]);

    #[cfg(not(target_os = "macos"))]
    let ret = Menu::with_items(app_handle, &[&file_submenu, &view_submenu]);

    Ok(ret?)
}

// --- Tauri commands ---

#[tauri::command]
pub fn set_viewer_mode(ctx: ViewerWindowContext, mode: String) -> Result<(), Error> {
    ctx.0.set_mode(&mode);
    Ok(())
}

#[tauri::command]
pub fn ping_viewer(ctx: ViewerWindowContext) -> Result<(), Error> {
    ctx.0.publish_full();
    Ok(())
}
