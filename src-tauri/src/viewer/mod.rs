pub mod encoding;
mod markdown;
mod table;

use newt_common::find::{SearchMatch, SearchPattern};
use newt_common::vfs::VfsPath;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use tauri::ipc::CommandArg;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager, State, WebviewWindow, Wry};

use crate::GlobalContext;
use crate::common::{Error, UpdatePublisher};
use crate::main_window::MainWindowContext;
use encoding::{DetectedEncoding, ViewerEncoding};
pub use table::{TableDelimiter, TableOptions};

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
    Table,
    Markdown,
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
            ViewerMode::Table => "table",
            ViewerMode::Markdown => "markdown",
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
            ViewerMode::Table => "Table",
            ViewerMode::Markdown => "Markdown",
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
            "table" => ViewerMode::Table,
            "markdown" => ViewerMode::Markdown,
            _ => return None,
        })
    }

    const ALL: [ViewerMode; 8] = [
        ViewerMode::Text,
        ViewerMode::Hex,
        ViewerMode::Table,
        ViewerMode::Markdown,
        ViewerMode::Image,
        ViewerMode::Audio,
        ViewerMode::Video,
        ViewerMode::Pdf,
    ];

    /// Modes that decode the file as text, and so take the Encoding menu.
    fn is_textual(self) -> bool {
        matches!(
            self,
            ViewerMode::Text | ViewerMode::Table | ViewerMode::Markdown
        )
    }
}

pub struct ViewerState {
    mode: RwLock<ViewerMode>,
    file_path: RwLock<Option<VfsPath>>,
    display_path: RwLock<Option<String>>,
    file_server_base: RwLock<Option<String>>,
    encoding: RwLock<ViewerEncoding>,
    table: RwLock<TableOptions>,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self {
            mode: RwLock::new(ViewerMode::Text),
            file_path: RwLock::new(None),
            display_path: RwLock::new(None),
            file_server_base: RwLock::new(None),
            encoding: RwLock::new(ViewerEncoding::default()),
            table: RwLock::new(TableOptions::default()),
        }
    }
}

impl ViewerState {
    pub fn file_path(&self) -> Option<VfsPath> {
        self.file_path.read().clone()
    }

    /// Show `file_path`, starting over from auto-detection.
    pub fn set_file(&self, file_path: VfsPath, display_path: String, file_server_base: String) {
        *self.file_path.write() = Some(file_path);
        *self.display_path.write() = Some(display_path);
        *self.file_server_base.write() = Some(file_server_base);
        *self.mode.write() = ViewerMode::Text;
        *self.encoding.write() = ViewerEncoding::default();
        *self.table.write() = TableOptions::default();
    }

    /// Show nothing.
    pub fn clear(&self) {
        *self.file_path.write() = None;
        *self.display_path.write() = None;
        *self.encoding.write() = ViewerEncoding::default();
        *self.table.write() = TableOptions::default();
    }

    pub fn set_mode(&self, mode: ViewerMode) {
        *self.mode.write() = mode;
    }

    fn set_detected(&self, detected: DetectedEncoding) {
        self.encoding.write().detected = Some(detected);
    }

    fn set_selected_encoding(&self, selected: Option<String>) {
        self.encoding.write().selected = selected;
    }

    /// Record what the table viewer detected; true when it changed.
    fn set_table_detection(&self, delimiter: TableDelimiter, header: bool) -> bool {
        let mut options = self.table.write();
        let changed = options.detected_delimiter != Some(delimiter)
            || options.detected_header != Some(header);
        options.detected_delimiter = Some(delimiter);
        options.detected_header = Some(header);
        changed
    }
}

/// The `update:viewer` payload, and the main window's Quick View; see
/// `MainWindowStateWire`.
#[derive(Serialize, specta::Type)]
#[specta(rename = "ViewerState")]
pub struct ViewerStateWire {
    mode: ViewerMode,
    file_path: Option<VfsPath>,
    display_path: Option<String>,
    file_server_base: Option<String>,
    encoding: ViewerEncoding,
    table: TableOptions,
}

crate::common::specta_as!(ViewerState => ViewerStateWire);

impl Serialize for ViewerState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ViewerStateWire {
            mode: *self.mode.read(),
            file_path: self.file_path.read().clone(),
            display_path: self.display_path.read().clone(),
            file_server_base: self.file_server_base.read().clone(),
            encoding: self.encoding.read().clone(),
            table: self.table.read().clone(),
        }
        .serialize(serializer)
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
        self.publisher
            .state()
            .set_file(file_path, display_path, file_server_base);
        let _ = self.publisher.publish_full();
    }

    /// Show another file in this window, as if it had been opened with F3.
    fn retarget(&self, file_path: VfsPath, display_path: String, file_server_base: String) {
        if let Some(window) = self.window.read().as_ref() {
            let _ = window.set_title(&format!("{} - Viewer", display_path));
        }
        self.set_file(file_path, display_path, file_server_base);
        self.rebuild_menu();
    }

    pub fn set_mode(&self, mode: ViewerMode) {
        self.publisher.state().set_mode(mode);
        self.rebuild_menu();
        let _ = self.publisher.publish_full();
    }

    pub fn set_encoding(&self, selected: Option<String>) {
        self.publisher.state().set_selected_encoding(selected);
        self.rebuild_menu();
        let _ = self.publisher.publish_full();
    }

    fn update_table(&self, f: impl FnOnce(&mut TableOptions) -> bool) {
        if f(&mut self.publisher.state().table.write()) {
            self.rebuild_menu();
            let _ = self.publisher.publish_full();
        }
    }

    fn set_detected(&self, detected: DetectedEncoding) {
        self.publisher.state().set_detected(detected);
        self.rebuild_menu();
        let _ = self.publisher.publish_full();
    }

    pub fn publish_full(&self) {
        let _ = self.publisher.publish_full();
    }

    fn rebuild_menu(&self) {
        let window_guard = self.window.read();
        let prefix_guard = self.prefix.read();
        let (window, prefix) = match (window_guard.as_ref(), prefix_guard.as_ref()) {
            (Some(w), Some(p)) => (w, p),
            _ => return,
        };
        let app_handle = window.app_handle();
        let state = self.publisher.state();
        let mode = *state.mode.read();
        let encoding = state.encoding.read().clone();
        let table = state.table.read().clone();
        let Ok(menu) = build_menu(app_handle, prefix, mode, &encoding, &table) else {
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
    let publisher = Arc::new(UpdatePublisher::new(
        window.clone(),
        "viewer",
        ViewerState::default(),
    ));

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
    let menu = {
        let state = viewer.publisher.state();
        let mode = *state.mode.read();
        let encoding = state.encoding.read().clone();
        let table = state.table.read().clone();
        build_menu(app_handle, &prefix, mode, &encoding, &table)?
    };

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
        if let Some(item) = suffix.strip_prefix("tbl_") {
            viewer.update_table(|options| options.apply_menu(item));
            return;
        }
        if let Some(enc) = suffix.strip_prefix("enc_") {
            if enc == "auto" {
                viewer.set_encoding(None);
            } else if let Some(name) = encoding::catalogue_name(enc) {
                viewer.set_encoding(Some(name.to_string()));
            }
            return;
        }
        let Some(mode) = suffix.strip_prefix("mode_").and_then(ViewerMode::from_id) else {
            return;
        };
        viewer.set_mode(mode);
    });

    Ok(())
}

fn has_edit_menu(mode: ViewerMode) -> bool {
    matches!(mode, ViewerMode::Text | ViewerMode::Hex | ViewerMode::Table)
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

/// A checked CheckMenuItem when active, a plain MenuItem otherwise: a
/// radio group without the empty checkbox indicators some GTK themes draw
/// on unchecked check items.
pub(super) fn radio_item(
    app_handle: &tauri::AppHandle,
    id: String,
    label: &str,
    active: bool,
) -> Result<Box<dyn tauri::menu::IsMenuItem<Wry>>, Error> {
    Ok(if active {
        Box::new(CheckMenuItem::with_id(
            app_handle,
            id,
            label,
            true,
            true,
            None::<&str>,
        )?)
    } else {
        Box::new(MenuItem::with_id(
            app_handle,
            id,
            label,
            true,
            None::<&str>,
        )?)
    })
}

fn encoding_submenu(
    app_handle: &tauri::AppHandle,
    prefix: &str,
    encoding: &ViewerEncoding,
) -> Result<Submenu<Wry>, Error> {
    let auto_label = match &encoding.detected {
        Some(d) if d.bom_len > 0 => format!("Auto-detect ({}, BOM)", d.encoding),
        Some(d) => format!("Auto-detect ({})", d.encoding),
        None => "Auto-detect".to_string(),
    };
    let auto_item = radio_item(
        app_handle,
        format!("{prefix}enc_auto"),
        &auto_label,
        encoding.selected.is_none(),
    )?;
    let separator = PredefinedMenuItem::separator(app_handle)?;

    let mut groups: Vec<Submenu<Wry>> = Vec::new();
    for group in encoding::CATALOGUE {
        let items = group
            .encodings
            .iter()
            .map(|name| {
                radio_item(
                    app_handle,
                    format!("{prefix}enc_{name}"),
                    name,
                    encoding.selected.as_deref() == Some(name),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> =
            items.iter().map(|i| i.as_ref()).collect();
        groups.push(Submenu::with_items(app_handle, group.label, true, &refs)?);
    }

    let mut refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![auto_item.as_ref(), &separator];
    refs.extend(
        groups
            .iter()
            .map(|g| g as &dyn tauri::menu::IsMenuItem<Wry>),
    );
    Ok(Submenu::with_items(app_handle, "Encoding", true, &refs)?)
}

fn build_menu(
    app_handle: &tauri::AppHandle,
    prefix: &str,
    mode: ViewerMode,
    encoding: &ViewerEncoding,
    table: &TableOptions,
) -> Result<Menu<Wry>, Error> {
    let mode_items = ViewerMode::ALL
        .iter()
        .map(|&m| {
            radio_item(
                app_handle,
                format!("{}mode_{}", prefix, m.id()),
                m.label(),
                m == mode,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    let item_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> =
        mode_items.iter().map(|i| i.as_ref()).collect();
    let view_submenu = Submenu::with_items(app_handle, "View", true, &item_refs)?;

    let encoding_submenu = mode
        .is_textual()
        .then(|| encoding_submenu(app_handle, prefix, encoding))
        .transpose()?;
    let table_submenu = (mode == ViewerMode::Table)
        .then(|| table::table_submenu(app_handle, prefix, table))
        .transpose()?;

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
            if mode == ViewerMode::Table {
                "Go to Row"
            } else {
                "Go to Line/Offset"
            },
            true,
            None::<&str>,
        )?;
        let edit_sep = PredefinedMenuItem::separator(app_handle)?;
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
    if let Some(table) = &table_submenu {
        items.push(table);
    }
    if let Some(enc) = &encoding_submenu {
        items.push(enc);
    }
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum CopyFormat {
    /// Lossy decode of the bytes in the named encoding.
    Text { encoding: String },
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
        CopyFormat::Text { encoding } => encoding::decode(&buf, &encoding),
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

/// Record the encoding sniffed from the file's leading bytes, which the
/// text viewer hands over from its first chunk. `eof` says the bytes are
/// the whole file.
#[tauri::command]
#[specta::specta]
pub fn sniff_viewer_encoding(
    ctx: ViewerWindowContext,
    prefix: Vec<u8>,
    eof: bool,
) -> Result<(), Error> {
    ctx.0.set_detected(encoding::detect(&prefix, eof));
    Ok(())
}

/// At most `max_size` bytes of `path`, decoded from `encoding` after a
/// `bom_len`-byte BOM, as rendered Markdown.
#[tauri::command]
#[specta::specta]
pub async fn render_markdown(
    ctx: MainWindowContext,
    path: VfsPath,
    max_size: u64,
    encoding: String,
    bom_len: u32,
) -> Result<Vec<markdown::MarkdownNode>, Error> {
    let data = ctx.fs()?.read_file(path, max_size).await?;
    // Tens of milliseconds of parsing for a large document.
    tokio::task::spawn_blocking(move || {
        let body = data.get(bom_len as usize..).unwrap_or_default();
        markdown::render(&encoding::decode(body, &encoding))
    })
    .await
    .map_err(|e| Error::Custom(e.to_string()))
}

/// Show another file in this viewer window: a relative link followed from
/// rendered Markdown.
#[tauri::command]
#[specta::specta]
pub async fn open_in_viewer(
    ctx: MainWindowContext,
    viewer: ViewerWindowContext,
    path: VfsPath,
) -> Result<(), Error> {
    let display_path = ctx.format_vfs_path(&path);
    if ctx.fs()?.file_details(path.clone()).await?.is_dir {
        return Err(Error::Custom(format!("{display_path} is a directory")));
    }
    viewer
        .0
        .retarget(path, display_path, ctx.file_server_base_url()?);
    Ok(())
}

/// Record what the table viewer detected from the file's first chunk, for
/// the Table menu's Auto entries to name it.
#[tauri::command]
#[specta::specta]
pub fn report_table_detection(
    ctx: ViewerWindowContext,
    delimiter: TableDelimiter,
    header: bool,
) -> Result<(), Error> {
    if ctx
        .0
        .publisher
        .state()
        .set_table_detection(delimiter, header)
    {
        ctx.0.rebuild_menu();
        ctx.0.publish_full();
    }
    Ok(())
}

/// Search pattern as the viewer's search bar states it. `Text` is encoded
/// here into the file's byte encoding; the filesystem search only knows
/// bytes.
#[derive(Debug, Clone, serde::Deserialize, specta::Type)]
pub enum ViewerSearchPattern {
    Text {
        text: String,
        encoding: String,
    },
    Bytes(Vec<u8>),
    /// Byte regex over the raw file; non-ASCII literals in the pattern are
    /// UTF-8, so they only match in UTF-8 files.
    Regex(String),
}

#[tauri::command]
#[specta::specta]
pub async fn find_in_viewer(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    pattern: ViewerSearchPattern,
    max_length: u64,
) -> Result<Option<SearchMatch>, Error> {
    let pattern = match pattern {
        ViewerSearchPattern::Text { text, encoding } => {
            SearchPattern::Literal(encoding::encode(&text, &encoding))
        }
        ViewerSearchPattern::Bytes(bytes) => SearchPattern::Literal(bytes),
        ViewerSearchPattern::Regex(re) => SearchPattern::Regex(re),
    };
    Ok(ctx
        .fs()?
        .find_in_file(path, offset, pattern, max_length)
        .await?)
}

// --- Quick View: the viewer in the main window's right slot ---

/// Point Quick View at the active pane's focused file — the file itself,
/// for an entry of a synthetic VFS such as search results. A folder, `..`
/// or an empty listing shows nothing.
fn preview_active_focus(ctx: &MainWindowContext) -> Result<(), Error> {
    let target = ctx
        .active_pane()
        .filter(|pane| !pane.is_focused_dir())
        .and_then(|pane| pane.get_focused_source());
    let target = match target {
        Some(path) => Some((
            ctx.format_vfs_path(&path),
            path,
            ctx.file_server_base_url()?,
        )),
        None => None,
    };
    ctx.with_update(|gs| {
        match target {
            Some((display_path, path, base)) => {
                if gs.preview.file_path().as_ref() != Some(&path) {
                    gs.preview.set_file(path, display_path, base);
                }
            }
            None => gs.preview.clear(),
        }
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn cmd_toggle_quick_view(
    ctx: MainWindowContext,
    _pane_handle: crate::main_window::PaneHandle,
) -> Result<(), Error> {
    let on = ctx.with_update(|gs| {
        let mut opts = gs.display_options.0.write();
        opts.quick_view = !opts.quick_view;
        Ok(opts.quick_view)
    })?;
    if on {
        preview_active_focus(&ctx)
    } else {
        ctx.with_update(|gs| {
            gs.preview.clear();
            Ok(())
        })
    }
}

/// Follow the active pane's focus; the frontend calls this, debounced, as
/// the focused row moves.
#[tauri::command]
#[specta::specta]
pub fn preview_focused(ctx: MainWindowContext) -> Result<(), Error> {
    preview_active_focus(&ctx)
}

// The preview's counterparts of the viewer window's commands carry the file
// they concern: rendering one file can report after Quick View has moved on
// to the next, and that report must not land on it.

#[tauri::command]
#[specta::specta]
pub fn set_preview_mode(
    ctx: MainWindowContext,
    path: VfsPath,
    mode: ViewerMode,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        if gs.preview.file_path().as_ref() == Some(&path) {
            gs.preview.set_mode(mode);
        }
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn sniff_preview_encoding(
    ctx: MainWindowContext,
    path: VfsPath,
    prefix: Vec<u8>,
    eof: bool,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        if gs.preview.file_path().as_ref() == Some(&path) {
            gs.preview.set_detected(encoding::detect(&prefix, eof));
        }
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn report_preview_table_detection(
    ctx: MainWindowContext,
    path: VfsPath,
    delimiter: TableDelimiter,
    header: bool,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        if gs.preview.file_path().as_ref() == Some(&path) {
            gs.preview.set_table_detection(delimiter, header);
        }
        Ok(())
    })
}

/// Open `path` in a viewer window: a relative link followed from Markdown
/// rendered in Quick View.
#[tauri::command]
#[specta::specta]
pub async fn open_viewer(ctx: MainWindowContext, path: VfsPath) -> Result<(), Error> {
    if ctx.fs()?.file_details(path.clone()).await?.is_dir {
        let display_path = ctx.format_vfs_path(&path);
        return Err(Error::Custom(format!("{display_path} is a directory")));
    }
    crate::cmd::window::open_viewer_window(&ctx, &path)
}

/// Pick the preview's encoding; `None` is auto-detect.
#[tauri::command]
#[specta::specta]
pub fn set_preview_encoding(
    ctx: MainWindowContext,
    path: VfsPath,
    selected: Option<String>,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        if gs.preview.file_path().as_ref() == Some(&path) {
            gs.preview.set_selected_encoding(selected);
        }
        Ok(())
    })
}

/// Apply a table option to the preview, named as the viewer window's Table
/// menu names it (`delim_comma`, `header_auto`, `quoted`, …).
#[tauri::command]
#[specta::specta]
pub fn set_preview_table_option(
    ctx: MainWindowContext,
    path: VfsPath,
    option: String,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        if gs.preview.file_path().as_ref() == Some(&path) {
            gs.preview.table.write().apply_menu(&option);
        }
        Ok(())
    })
}

#[derive(Serialize, specta::Type)]
pub struct EncodingGroupView {
    label: &'static str,
    encodings: &'static [&'static str],
}

/// The viewer window's Encoding menu, for Quick View's.
#[tauri::command]
#[specta::specta]
pub fn encoding_catalogue() -> Vec<EncodingGroupView> {
    encoding::CATALOGUE
        .iter()
        .map(|g| EncodingGroupView {
            label: g.label,
            encodings: g.encodings,
        })
        .collect()
}
