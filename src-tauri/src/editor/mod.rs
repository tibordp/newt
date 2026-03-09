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
}

impl Serialize for EditorState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("EditorState", 2)?;
        s.serialize_field("language", &*self.language.read())?;
        s.serialize_field("word_wrap", &*self.word_wrap.read())?;
        s.end()
    }
}

pub struct EditorWindow {
    window: WebviewWindow,
    publisher: Arc<UpdatePublisher<EditorState>>,
    menu: Menu<Wry>,
}

impl EditorWindow {
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
        let active_id = format!("editor_lang_{}", active_lang);
        for check in check_menu_items(&self.menu) {
            if check.id().as_ref().starts_with("editor_lang_") {
                let _ = check.set_checked(check.id().as_ref() == active_id.as_str());
            }
        }
    }

    fn update_wrap_check(&self, wrap: bool) {
        for check in check_menu_items(&self.menu) {
            if check.id().as_ref() == "editor_wrap" {
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

pub fn create_editor_window(
    app_handle: &tauri::AppHandle,
    _label: &str,
    window: &WebviewWindow,
) -> Result<Arc<EditorWindow>, Error> {
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

    let state = EditorState {
        language: RwLock::new("plaintext".to_string()),
        word_wrap: RwLock::new(false),
    };
    let publisher = Arc::new(UpdatePublisher::new(window.clone(), "editor", state));

    let editor = Arc::new(EditorWindow {
        window: window.clone(),
        publisher,
        menu,
    });

    // Register menu event handler
    let editor_weak = Arc::downgrade(&editor);
    app_handle.on_menu_event(move |_app_handle, event| {
        let editor = match editor_weak.upgrade() {
            Some(e) => e,
            None => return,
        };
        let id = event.id().0.as_str();
        if id == "editor_save" {
            let _ = editor.window.emit("editor-action", "save");
        } else if id == "editor_close" {
            let _ = editor.window.close();
        } else if id == "editor_wrap" {
            // CheckMenuItem auto-toggles; read the new checked state
            let checked = check_menu_items(&editor.menu)
                .find(|c| c.id().as_ref() == "editor_wrap")
                .and_then(|c| c.is_checked().ok());
            if let Some(checked) = checked {
                *editor.publisher.state().word_wrap.write() = checked;
                let _ = editor.publisher.publish_full();
            }
        } else if let Some(lang) = id.strip_prefix("editor_lang_") {
            editor.set_language(lang);
        }
    });

    Ok(editor)
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

fn build_menu(app_handle: &tauri::AppHandle) -> Result<Menu<Wry>, Error> {
    // File menu
    let save_item =
        MenuItem::with_id(app_handle, "editor_save", "Save", true, Some("CmdOrCtrl+S"))?;
    let close_item = MenuItem::with_id(
        app_handle,
        "editor_close",
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

    // View menu — word wrap as checkbox
    let wrap_item = CheckMenuItem::with_id(
        app_handle,
        "editor_wrap",
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
                format!("editor_lang_{}", id),
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

    Ok(Menu::with_items(
        app_handle,
        &[&file_submenu, &view_submenu, &lang_submenu],
    )?)
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
