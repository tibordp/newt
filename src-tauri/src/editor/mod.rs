use newt_common::vfs::VfsPath;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use tauri::ipc::CommandArg;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager, State, WebviewWindow, Wry};

use crate::GlobalContext;
use crate::common::{Error, UpdatePublisher};

const LANGUAGES: &[(&str, &str)] = &[
    ("plaintext", "Plain Text"),
    ("c", "C"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("css", "CSS"),
    ("dockerfile", "Dockerfile"),
    ("go", "Go"),
    ("html", "HTML"),
    ("ini", "INI / TOML"),
    ("java", "Java"),
    ("javascript", "JavaScript"),
    ("json", "JSON"),
    ("kotlin", "Kotlin"),
    ("lua", "Lua"),
    ("markdown", "Markdown"),
    ("perl", "Perl"),
    ("php", "PHP"),
    ("python", "Python"),
    ("ruby", "Ruby"),
    ("rust", "Rust"),
    ("scss", "SCSS"),
    ("shell", "Shell"),
    ("sql", "SQL"),
    ("swift", "Swift"),
    ("typescript", "TypeScript"),
    ("xml", "XML"),
    ("yaml", "YAML"),
];

pub struct EditorState {
    language: RwLock<String>,
    word_wrap: RwLock<bool>,
    file_path: RwLock<Option<VfsPath>>,
    display_path: RwLock<Option<String>>,
}

impl Serialize for EditorState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("EditorState", 4)?;
        s.serialize_field("language", &*self.language.read())?;
        s.serialize_field("word_wrap", &*self.word_wrap.read())?;
        s.serialize_field("file_path", &*self.file_path.read())?;
        s.serialize_field("display_path", &*self.display_path.read())?;
        s.end()
    }
}

pub struct EditorWindow {
    window: WebviewWindow,
    publisher: Arc<UpdatePublisher<EditorState>>,
    menu: RwLock<Option<Menu<Wry>>>,
}

impl EditorWindow {
    pub fn set_file(&self, file_path: VfsPath, display_path: String) {
        let state = self.publisher.state();
        *state.file_path.write() = Some(file_path);
        *state.display_path.write() = Some(display_path);
        // Reset state for new file
        *state.language.write() = "plaintext".to_string();
        *state.word_wrap.write() = false;
        let _ = self.publisher.publish_full();
    }

    pub fn set_language(&self, lang: &str) {
        *self.publisher.state().language.write() = lang.to_string();
        self.update_language_checks(lang);
        let _ = self.publisher.publish_full();
    }

    pub fn set_word_wrap(&self, wrap: bool) {
        *self.publisher.state().word_wrap.write() = wrap;
        self.update_wrap_check(wrap);
        let _ = self.publisher.publish_full();
    }

    pub fn publish_full(&self) {
        let _ = self.publisher.publish_full();
    }

    fn update_language_checks(&self, active_lang: &str) {
        let menu_guard = self.menu.read();
        let menu = match menu_guard.as_ref() {
            Some(m) => m,
            None => return,
        };
        let suffix = format!("lang_{}", active_lang);
        for check in check_menu_items(menu) {
            let id = check.id();
            if id.as_ref().contains("lang_") {
                let _ = check.set_checked(id.as_ref().ends_with(&suffix));
            }
        }
    }

    fn update_wrap_check(&self, wrap: bool) {
        let menu_guard = self.menu.read();
        let menu = match menu_guard.as_ref() {
            Some(m) => m,
            None => return,
        };
        for check in check_menu_items(menu) {
            if check.id().as_ref().ends_with("wrap") {
                let _ = check.set_checked(wrap);
            }
        }
    }
}

#[derive(Clone)]
pub struct EditorWindowContext(pub Arc<EditorWindow>);

impl<'de> CommandArg<'de, Wry> for EditorWindowContext {
    fn from_command(
        command: tauri::ipc::CommandItem<'de, Wry>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        let window = command.message.webview();
        let app_handle = window.app_handle();
        let s: State<GlobalContext> = app_handle.state();

        s.editor_window(window.label())
            .ok_or_else(|| tauri::ipc::InvokeError::from("editor window not found"))
    }
}

/// Create an EditorWindow with UpdatePublisher but no menu.
pub fn create_editor_window(window: &WebviewWindow) -> Arc<EditorWindow> {
    let state = EditorState {
        language: RwLock::new("plaintext".to_string()),
        word_wrap: RwLock::new(false),
        file_path: RwLock::new(None),
        display_path: RwLock::new(None),
    };
    let publisher = Arc::new(UpdatePublisher::new(window.clone(), "editor", state));

    Arc::new(EditorWindow {
        window: window.clone(),
        publisher,
        menu: RwLock::new(None),
    })
}

/// Attach menu and register event handler. Called when showing the window.
pub fn activate_editor_window(
    app_handle: &tauri::AppHandle,
    label: &str,
    #[cfg_attr(target_os = "macos", allow(unused_variables))] window: &WebviewWindow,
    editor: &Arc<EditorWindow>,
) -> Result<(), Error> {
    let prefix = format!("editor_{}_", label);
    let save_id = format!("{}save", prefix);
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

    *editor.menu.write() = Some(menu);

    // Register menu event handler — IDs are prefixed with the window label
    // so each handler only reacts to its own window's menu items.
    let editor_weak = Arc::downgrade(editor);
    app_handle.on_menu_event(move |_app_handle, event| {
        let id = event.id().0.as_str();

        // Only handle events with our prefix
        if !id.starts_with(prefix.as_str()) {
            return;
        }

        let editor = match editor_weak.upgrade() {
            Some(e) => e,
            None => return,
        };

        if id == save_id {
            let _ = editor
                .window
                .emit_to(editor.window.label(), "editor-action", "save");
        } else if id == close_id {
            let _ = editor.window.close();
        } else if id.ends_with("wrap") {
            // CheckMenuItem auto-toggles; read the new checked state
            let menu_guard = editor.menu.read();
            let checked = menu_guard.as_ref().and_then(|menu| {
                check_menu_items(menu)
                    .find(|c| c.id().as_ref().ends_with("wrap"))
                    .and_then(|c| c.is_checked().ok())
            });
            if let Some(checked) = checked {
                *editor.publisher.state().word_wrap.write() = checked;
                let _ = editor.publisher.publish_full();
            }
        } else if let Some(suffix) = id.strip_prefix(prefix.as_str())
            && let Some(lang) = suffix.strip_prefix("lang_")
        {
            editor.set_language(lang);
        }
    });

    Ok(())
}

/// Iterate all `CheckMenuItem`s across all submenus of a menu.
fn check_menu_items(menu: &Menu<Wry>) -> impl Iterator<Item = CheckMenuItem<Wry>> {
    menu.items()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| item.as_submenu().map(|s| s.items().unwrap_or_default()))
        .flatten()
        .filter_map(|item| item.as_check_menuitem().cloned())
}

fn build_menu(app_handle: &tauri::AppHandle, prefix: &str) -> Result<Menu<Wry>, Error> {
    // File menu
    let save_item = MenuItem::with_id(
        app_handle,
        format!("{}save", prefix),
        "Save",
        true,
        Some("CmdOrCtrl+S"),
    )?;
    let close_item = MenuItem::with_id(
        app_handle,
        format!("{}close", prefix),
        "Close",
        true,
        Some("CmdOrCtrl+W"),
    )?;
    let file_submenu = Submenu::with_items(
        app_handle,
        "File",
        true,
        &[
            &save_item,
            &PredefinedMenuItem::separator(app_handle)?,
            &close_item,
        ],
    )?;

    // Edit menu — predefined items so macOS routes Cmd+C/V/X/A to the webview
    #[cfg(target_os = "macos")]
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

    // View menu — word wrap as checkbox
    let wrap_item = CheckMenuItem::with_id(
        app_handle,
        format!("{}wrap", prefix),
        "Word Wrap",
        true,
        false,
        None::<&str>,
    )?;
    let view_submenu = Submenu::with_items(app_handle, "View", true, &[&wrap_item])?;

    // Language menu — radio-style check items
    let lang_items: Vec<CheckMenuItem<Wry>> = LANGUAGES
        .iter()
        .map(|(id, label)| {
            CheckMenuItem::with_id(
                app_handle,
                format!("{}lang_{}", prefix, id),
                *label,
                true,
                false,
                None::<&str>,
            )
            .unwrap()
        })
        .collect();

    let lang_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = lang_items
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<Wry>)
        .collect();
    let lang_submenu = Submenu::with_items(app_handle, "Language", true, &lang_refs)?;

    #[cfg(target_os = "macos")]
    let ret = Menu::with_items(
        app_handle,
        &[&file_submenu, &edit_submenu, &view_submenu, &lang_submenu],
    );

    #[cfg(not(target_os = "macos"))]
    let ret = Menu::with_items(app_handle, &[&file_submenu, &view_submenu, &lang_submenu]);

    Ok(ret?)
}

// --- Tauri commands ---

#[tauri::command]
pub fn set_editor_language(ctx: EditorWindowContext, language: String) -> Result<(), Error> {
    ctx.0.set_language(&language);
    Ok(())
}

#[tauri::command]
pub fn set_editor_wrap(ctx: EditorWindowContext, wrap: bool) -> Result<(), Error> {
    ctx.0.set_word_wrap(wrap);
    Ok(())
}

#[tauri::command]
pub fn ping_editor(ctx: EditorWindowContext) -> Result<(), Error> {
    ctx.0.publish_full();
    Ok(())
}
