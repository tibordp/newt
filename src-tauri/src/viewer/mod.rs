use newt_common::find::{SearchMatch, SearchPattern};
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

/// Display mode for the file viewer. Wire format is snake_case to match
/// the strings the frontend uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ViewerMode {
    Text,
    Hex,
    Image,
    Audio,
    Video,
    Pdf,
}

impl ViewerMode {
    /// Stable identifier used in menu item ids — matches the serde rename.
    fn id(self) -> &'static str {
        match self {
            ViewerMode::Text => "text",
            ViewerMode::Hex => "hex",
            ViewerMode::Image => "image",
            ViewerMode::Audio => "audio",
            ViewerMode::Video => "video",
            ViewerMode::Pdf => "pdf",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ViewerMode::Text => "Text",
            ViewerMode::Hex => "Hex",
            ViewerMode::Image => "Image",
            ViewerMode::Audio => "Audio",
            ViewerMode::Video => "Video",
            ViewerMode::Pdf => "PDF",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "text" => ViewerMode::Text,
            "hex" => ViewerMode::Hex,
            "image" => ViewerMode::Image,
            "audio" => ViewerMode::Audio,
            "video" => ViewerMode::Video,
            "pdf" => ViewerMode::Pdf,
            _ => return None,
        })
    }

    const ALL: [ViewerMode; 6] = [
        ViewerMode::Text,
        ViewerMode::Hex,
        ViewerMode::Image,
        ViewerMode::Audio,
        ViewerMode::Video,
        ViewerMode::Pdf,
    ];
}

pub struct ViewerState {
    mode: RwLock<ViewerMode>,
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
        *state.mode.write() = ViewerMode::Text;
        let _ = self.publisher.publish_full();
    }

    pub fn set_mode(&self, mode: ViewerMode) {
        *self.publisher.state().mode.write() = mode;
        self.rebuild_menu(mode);
        let _ = self.publisher.publish_full();
    }

    pub fn publish_full(&self) {
        let _ = self.publisher.publish_full();
    }

    fn rebuild_menu(&self, active_mode: ViewerMode) {
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

// Server-side state — see the same impl on `MainWindowContext`.
impl specta::function::FunctionArg for ViewerWindowContext {
    fn to_datatype(_: &mut specta::TypeCollection) -> Option<specta::datatype::DataType> {
        None
    }
}

/// Create a ViewerWindow with UpdatePublisher but no menu.
/// Used both for pre-warming and direct creation.
pub fn create_viewer_window(window: &WebviewWindow) -> Arc<ViewerWindow> {
    let state = ViewerState {
        mode: RwLock::new(ViewerMode::Text),
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
    let current_mode = *viewer.publisher.state().mode.read();
    let menu = build_menu(app_handle, &prefix, current_mode)?;

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

        #[cfg(target_os = "macos")]
        if suffix == "quit" {
            let global_ctx: State<GlobalContext> = _app_handle.state();
            global_ctx.quit(_app_handle);
            return;
        }

        // Handle edit menu items by emitting events to the frontend
        if suffix == "copy" || suffix == "select_all" || suffix == "goto" {
            let _ = window_clone.emit_to(window_clone.label(), "viewer-menu", suffix);
            return;
        }

        let viewer = match viewer_weak.upgrade() {
            Some(v) => v,
            None => return,
        };
        let Some(mode) = suffix.strip_prefix("mode_").and_then(ViewerMode::from_id) else {
            return;
        };
        viewer.set_mode(mode);
    });

    Ok(())
}

fn has_edit_menu(mode: ViewerMode) -> bool {
    matches!(mode, ViewerMode::Text | ViewerMode::Hex)
}

/// Edit submenu for modes without a custom one. macOS needs predefined items
/// so Cmd+C/V/X/A route to the webview as native events (see
/// `main_window::menu`) — the image viewer's info panel relies on this for
/// text copy. Other platforms handle these keys in the webview without menu
/// involvement.
fn native_edit_submenu(app_handle: &tauri::AppHandle) -> Result<Option<Submenu<Wry>>, Error> {
    #[cfg(target_os = "macos")]
    {
        use tauri::menu::PredefinedMenuItem;
        Ok(Some(Submenu::with_items(
            app_handle,
            "Edit",
            true,
            &[
                &PredefinedMenuItem::cut(app_handle, None)?,
                &PredefinedMenuItem::copy(app_handle, None)?,
                &PredefinedMenuItem::paste(app_handle, None)?,
                &PredefinedMenuItem::select_all(app_handle, None)?,
            ],
        )?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app_handle;
        Ok(None)
    }
}

fn build_menu(
    app_handle: &tauri::AppHandle,
    prefix: &str,
    mode: ViewerMode,
) -> Result<Menu<Wry>, Error> {
    // Use a checked CheckMenuItem for the active mode, plain MenuItem for the rest.
    // This avoids showing empty checkbox indicators (visible on some GTK themes).
    let mut mode_items: Vec<Box<dyn tauri::menu::IsMenuItem<Wry>>> = Vec::new();
    for &m in &ViewerMode::ALL {
        let menu_id = format!("{}mode_{}", prefix, m.id());
        if m == mode {
            mode_items.push(Box::new(CheckMenuItem::with_id(
                app_handle,
                &menu_id,
                m.label(),
                true,
                true,
                None::<&str>,
            )?));
        } else {
            mode_items.push(Box::new(MenuItem::with_id(
                app_handle,
                &menu_id,
                m.label(),
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

    let edit_submenu = if has_edit_menu(mode) {
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
        Some(Submenu::with_items(
            app_handle,
            "Edit",
            true,
            &[&copy_item, &select_all_item, &edit_sep, &goto_item],
        )?)
    } else {
        native_edit_submenu(app_handle)?
    };

    // The menubar's first submenu is the application menu; give it a Quit
    // item so ⌘Q works from a viewer window too.
    #[cfg(target_os = "macos")]
    let app_submenu = {
        let quit_item = MenuItem::with_id(
            app_handle,
            format!("{}quit", prefix),
            "Quit Newt",
            true,
            Some("Cmd+Q"),
        )?;
        Submenu::with_items(app_handle, "Newt", true, &[&quit_item])?
    };

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![&file_submenu];
    if let Some(edit) = &edit_submenu {
        items.push(edit);
    }
    items.push(&view_submenu);
    #[cfg(target_os = "macos")]
    items.insert(0, &app_submenu);

    Ok(Menu::with_items(app_handle, &items)?)
}

// --- Tauri commands ---

#[tauri::command]
#[specta::specta]
pub fn set_viewer_mode(ctx: ViewerWindowContext, mode: ViewerMode) -> Result<(), Error> {
    ctx.0.set_mode(mode);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn ping_viewer(ctx: ViewerWindowContext) -> Result<(), Error> {
    ctx.0.publish_full();
    Ok(())
}

/// How to render a byte range when copying to the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum CopyFormat {
    /// UTF-8 lossy decode of the bytes.
    Text,
    /// Space-separated uppercase hex (`AB CD EF`).
    Hex,
    /// Printable ASCII (0x20–0x7e); other bytes become `.`.
    Ascii,
}

/// Copy a byte range from a file to the system clipboard.
#[tauri::command]
#[specta::specta]
pub async fn copy_viewer_range(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    length: u64,
    format: CopyFormat,
) -> Result<(), Error> {
    const MAX_COPY_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
    if length > MAX_COPY_BYTES {
        return Err(Error::Custom(format!(
            "Selection too large to copy ({} bytes, max {} bytes)",
            length, MAX_COPY_BYTES
        )));
    }

    const CHUNK: u64 = 128 * 1024;
    let buf = if length <= CHUNK {
        // Single chunk — one stateless read, no handle session.
        ctx.fs()?.read_range(path, offset, length).await?.data
    } else {
        let mut reader = ctx.fs()?.open_read_at(path).await?;
        let mut buf = Vec::with_capacity(length as usize);
        let mut pos = offset;
        let end = offset + length;
        while pos < end {
            let chunk_len = std::cmp::min(end - pos, CHUNK);
            let data = reader.read_at(pos, chunk_len).await?;
            if data.is_empty() {
                break;
            }
            pos += data.len() as u64;
            buf.extend_from_slice(&data);
        }
        buf
    };

    let text = match format {
        CopyFormat::Hex => buf
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" "),
        CopyFormat::Ascii => buf
            .iter()
            .map(|&b| {
                if (0x20..=0x7e).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect(),
        CopyFormat::Text => String::from_utf8_lossy(&buf).into_owned(),
    };

    ctx.clipboard().set_text(text)?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct ExifRow {
    pub label: String,
    pub value: String,
}

fn gps_coord(exif: &exif::Exif, tag: exif::Tag, ref_tag: exif::Tag) -> Option<f64> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let exif::Value::Rational(parts) = &field.value else {
        return None;
    };
    if parts.len() < 3 {
        return None;
    }
    let dd = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
    let negative = matches!(
        exif.get_field(ref_tag, exif::In::PRIMARY).map(|f| &f.value),
        Some(exif::Value::Ascii(v)) if v.first().and_then(|s| s.first()) == Some(&b'S')
            || v.first().and_then(|s| s.first()) == Some(&b'W')
    );
    Some(if negative { -dd } else { dd })
}

/// Tag's display value with units, ASCII quoting stripped. `None` when the
/// tag is absent or displays empty.
fn tag_display(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let value = field.display_value().with_unit(exif).to_string();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(&value)
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn exif_rows(exif: &exif::Exif) -> Vec<ExifRow> {
    use exif::Tag;

    let mut rows: Vec<ExifRow> = Vec::new();
    let mut push = |label: &str, value: Option<String>| {
        if let Some(value) = value {
            rows.push(ExifRow {
                label: label.to_string(),
                value,
            });
        }
    };

    // Make is usually redundant with the model name ("OnePlus" / "OnePlus 13")
    let make = tag_display(exif, Tag::Make);
    let model = tag_display(exif, Tag::Model);
    let camera = match (make, model) {
        (Some(make), Some(model)) => {
            if model.to_lowercase().starts_with(&make.to_lowercase()) {
                Some(model)
            } else {
                Some(format!("{} {}", make, model))
            }
        }
        (make, model) => make.or(model),
    };
    push("Camera", camera);
    push("Lens", tag_display(exif, Tag::LensModel));
    push("Taken", tag_display(exif, Tag::DateTimeOriginal));

    let exposure_parts: Vec<String> = [
        tag_display(exif, Tag::ExposureTime),
        tag_display(exif, Tag::FNumber),
        tag_display(exif, Tag::PhotographicSensitivity).map(|v| format!("ISO {}", v)),
    ]
    .into_iter()
    .flatten()
    .collect();
    push(
        "Exposure",
        (!exposure_parts.is_empty()).then(|| exposure_parts.join(" · ")),
    );

    let focal = match (
        tag_display(exif, Tag::FocalLength),
        tag_display(exif, Tag::FocalLengthIn35mmFilm),
    ) {
        (Some(fl), Some(fl35)) => Some(format!("{} ({} equiv.)", fl, fl35)),
        (fl, fl35) => fl.or(fl35),
    };
    push("Focal length", focal);

    // "0 EV" is noise, show bias only when set
    let bias = exif
        .get_field(Tag::ExposureBiasValue, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            exif::Value::SRational(v) => v.first().map(|r| r.to_f64()),
            _ => None,
        });
    if bias.is_some_and(|b| b != 0.0) {
        push("Exposure bias", tag_display(exif, Tag::ExposureBiasValue));
    }

    // The full Flash display enumerates return-light detection and
    // suppression; the part before the first comma is the fired state
    let flash = tag_display(exif, Tag::Flash).map(|v| {
        let mut s = v.split(',').next().unwrap_or(&v).trim().to_string();
        if let Some(first) = s.get(0..1) {
            let upper = first.to_uppercase();
            s.replace_range(0..1, &upper);
        }
        s
    });
    push("Flash", flash);

    push("Software", tag_display(exif, Tag::Software));
    push("Artist", tag_display(exif, Tag::Artist));
    push("Copyright", tag_display(exif, Tag::Copyright));

    let location = match (
        gps_coord(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef),
        gps_coord(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef),
    ) {
        (Some(lat), Some(lon)) => Some(format!("{:.6}, {:.6}", lat, lon)),
        _ => None,
    };
    push("Location", location);

    rows
}

/// EXIF summary for the image viewer's info panel. Reads a bounded prefix of
/// the file — EXIF sits near the start of every container we care about;
/// files whose metadata lies deeper simply report none, as do files without
/// EXIF at all.
#[tauri::command]
#[specta::specta]
pub async fn image_exif(ctx: MainWindowContext, path: VfsPath) -> Result<Vec<ExifRow>, Error> {
    const MAX_EXIF_PREFIX: u64 = 4 * 1024 * 1024;

    let buf = ctx.fs()?.read_range(path, 0, MAX_EXIF_PREFIX).await?.data;

    Ok(exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(&buf))
        .map(|exif| exif_rows(&exif))
        .unwrap_or_default())
}

#[tauri::command]
#[specta::specta]
pub async fn find_in_viewer(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    pattern: SearchPattern,
    max_length: u64,
) -> Result<Option<SearchMatch>, Error> {
    Ok(ctx
        .fs()?
        .find_in_file(path, offset, pattern, max_length)
        .await?)
}
