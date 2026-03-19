use newt_common::file_reader::{SearchMatch, SearchPattern};
use newt_common::vfs::VfsPath;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use tauri::ipc::CommandArg;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, Submenu};
use tauri::{Emitter, Manager, State, WebviewWindow, Wry};

use crate::GlobalContext;
use crate::common::{Error, UpdatePublisher};
use crate::main_window::MainWindowContext;

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
    window: RwLock<Option<WebviewWindow>>,
    prefix: RwLock<Option<String>>,
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
        self.rebuild_menu(mode);
        let _ = self.publisher.publish_full();
    }

    pub fn publish_full(&self) {
        let _ = self.publisher.publish_full();
    }

    fn rebuild_menu(&self, active_mode: &str) {
        let window_guard = self.window.read();
        let prefix_guard = self.prefix.read();
        let (window, prefix) = match (window_guard.as_ref(), prefix_guard.as_ref()) {
            (Some(w), Some(p)) => (w, p),
            _ => return,
        };
        let app_handle = window.app_handle();
        let Ok(menu) = build_menu(app_handle, prefix, active_mode) else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            let global_ctx: State<GlobalContext> = app_handle.state();
            global_ctx.set_window_menu(window.label(), menu.clone());
            let _ = app_handle.set_menu(menu.clone());
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window.set_menu(menu.clone());
        }
        *self.menu.write() = Some(menu);
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
        window: RwLock::new(None),
        prefix: RwLock::new(None),
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
    let current_mode = viewer.publisher.state().mode.read().clone();
    let menu = build_menu(app_handle, &prefix, &current_mode)?;

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
    *viewer.window.write() = Some(window.clone());
    *viewer.prefix.write() = Some(prefix.clone());

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

        // Handle edit menu items by emitting events to the frontend
        if suffix == "copy" || suffix == "select_all" || suffix == "goto" {
            let _ = window_clone.emit("viewer-menu", suffix);
            return;
        }

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

fn has_edit_menu(mode: &str) -> bool {
    matches!(mode, "text" | "hex")
}

fn build_menu(app_handle: &tauri::AppHandle, prefix: &str, mode: &str) -> Result<Menu<Wry>, Error> {
    // Use a checked CheckMenuItem for the active mode, plain MenuItem for the rest.
    // This avoids showing empty checkbox indicators (visible on some GTK themes).
    let mut mode_items: Vec<Box<dyn tauri::menu::IsMenuItem<Wry>>> = Vec::new();
    for (id, label) in MODES {
        let menu_id = format!("{}mode_{}", prefix, id);
        if *id == mode {
            mode_items.push(Box::new(CheckMenuItem::with_id(
                app_handle,
                &menu_id,
                *label,
                true,
                true,
                None::<&str>,
            )?));
        } else {
            mode_items.push(Box::new(MenuItem::with_id(
                app_handle,
                &menu_id,
                *label,
                true,
                None::<&str>,
            )?));
        }
    }

    let item_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> =
        mode_items.iter().map(|i| i.as_ref()).collect();
    let view_submenu = Submenu::with_items(app_handle, "View", true, &item_refs)?;

    let close_item = MenuItem::with_id(
        app_handle,
        format!("{}close", prefix),
        "Close",
        true,
        None::<&str>,
    )?;
    let file_submenu = Submenu::with_items(app_handle, "File", true, &[&close_item])?;

    let ret = if has_edit_menu(mode) {
        // No native accelerators — the webview handles Ctrl+C/A/G directly
        // and the menu event handler bridges menu clicks via viewer-menu events.
        // Registering native accelerators causes GTK warnings on menu rebuild.
        let copy_item = MenuItem::with_id(
            app_handle,
            format!("{}copy", prefix),
            "Copy",
            true,
            None::<&str>,
        )?;
        let select_all_item = MenuItem::with_id(
            app_handle,
            format!("{}select_all", prefix),
            "Select All",
            true,
            None::<&str>,
        )?;
        let goto_item = MenuItem::with_id(
            app_handle,
            format!("{}goto", prefix),
            "Go to Line/Offset",
            true,
            None::<&str>,
        )?;
        let edit_sep = tauri::menu::PredefinedMenuItem::separator(app_handle)?;
        let edit_submenu = Submenu::with_items(
            app_handle,
            "Edit",
            true,
            &[&copy_item, &select_all_item, &edit_sep, &goto_item],
        )?;

        Menu::with_items(app_handle, &[&file_submenu, &edit_submenu, &view_submenu])
    } else {
        Menu::with_items(app_handle, &[&file_submenu, &view_submenu])
    };

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

/// Copy a byte range from a file to the system clipboard.
/// `format`: "text" (UTF-8), "hex" (space-separated hex), "ascii" (printable ASCII).
#[tauri::command]
pub async fn copy_viewer_range(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    length: u64,
    format: String,
) -> Result<(), Error> {
    const MAX_COPY_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
    if length > MAX_COPY_BYTES {
        return Err(Error::Custom(format!(
            "Selection too large to copy ({} bytes, max {} bytes)",
            length, MAX_COPY_BYTES
        )));
    }

    let mut buf = Vec::with_capacity(length as usize);
    let mut pos = offset;
    let end = offset + length;
    while pos < end {
        let chunk_len = std::cmp::min(end - pos, 128 * 1024);
        let chunk = ctx
            .file_reader()?
            .read_range(path.clone(), pos, chunk_len)
            .await?;
        if chunk.data.is_empty() {
            break;
        }
        pos += chunk.data.len() as u64;
        buf.extend_from_slice(&chunk.data);
    }

    let text = match format.as_str() {
        "hex" => buf
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" "),
        "ascii" => buf
            .iter()
            .map(|&b| {
                if (0x20..=0x7e).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect(),
        _ => String::from_utf8_lossy(&buf).into_owned(),
    };

    ctx.clipboard().set_text(text)?;
    Ok(())
}

#[tauri::command]
pub async fn find_in_viewer(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    pattern: SearchPattern,
    max_length: u64,
) -> Result<Option<SearchMatch>, Error> {
    Ok(ctx
        .file_reader()?
        .find_in_file(path, offset, pattern, max_length)
        .await?)
}
