use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use tauri::ipc::CommandArg;
use tauri::menu::{CheckMenuItem, Menu, Submenu};
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
}

impl Serialize for ViewerState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ViewerState", 1)?;
        s.serialize_field("mode", &*self.mode.read())?;
        s.end()
    }
}

pub struct ViewerWindow {
    publisher: Arc<UpdatePublisher<ViewerState>>,
    menu: Menu<Wry>,
}

impl ViewerWindow {
    pub fn set_mode(&self, mode: &str) {
        *self.publisher.state().mode.write() = mode.to_string();
        self.update_menu_checks(mode);
        let _ = self.publisher.publish_full();
    }

    pub fn publish_full(&self) {
        let _ = self.publisher.publish_full();
    }

    fn update_menu_checks(&self, active_mode: &str) {
        let active_id = format!("viewer_mode_{}", active_mode);
        if let Ok(items) = self.menu.items() {
            for item in &items {
                if let Some(submenu) = item.as_submenu()
                    && let Ok(sub_items) = submenu.items()
                {
                    for sub_item in &sub_items {
                        if let Some(check) = sub_item.as_check_menuitem() {
                            let _ = check.set_checked(check.id().as_ref() == active_id.as_str());
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

pub fn create_viewer_window(
    app_handle: &tauri::AppHandle,
    _label: &str,
    window: &WebviewWindow,
) -> Result<Arc<ViewerWindow>, Error> {
    let menu = build_menu(app_handle)?;

    #[cfg(target_os = "macos")]
    {
        let global_ctx: State<GlobalContext> = app_handle.state();
        global_ctx.set_window_menu(_label, menu.clone());
        let _ = app_handle.set_menu(menu.clone());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window.set_menu(menu.clone());
    }

    let state = ViewerState {
        mode: RwLock::new("text".to_string()),
    };
    let publisher = Arc::new(UpdatePublisher::new(window.clone(), "viewer", state));

    let viewer = Arc::new(ViewerWindow { publisher, menu });

    // Register menu event handler
    let viewer_weak = Arc::downgrade(&viewer);
    app_handle.on_menu_event(move |_app_handle, event| {
        let viewer = match viewer_weak.upgrade() {
            Some(v) => v,
            None => return,
        };
        let mode = match event.id().as_ref() {
            "viewer_mode_text" => "text",
            "viewer_mode_hex" => "hex",
            "viewer_mode_image" => "image",
            "viewer_mode_audio" => "audio",
            "viewer_mode_video" => "video",
            "viewer_mode_pdf" => "pdf",
            _ => return,
        };
        viewer.set_mode(mode);
    });

    Ok(viewer)
}

fn build_menu(app_handle: &tauri::AppHandle) -> Result<Menu<Wry>, Error> {
    let mode_items: Vec<CheckMenuItem<Wry>> = MODES
        .iter()
        .map(|(id, label)| {
            CheckMenuItem::with_id(
                app_handle,
                format!("viewer_mode_{}", id),
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
    Ok(Menu::with_items(app_handle, &[&view_submenu])?)
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
