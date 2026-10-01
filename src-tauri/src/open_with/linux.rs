use std::path::{Path, PathBuf};

use gio::prelude::*;

use super::OpenWithApp;

fn content_type(name: &str) -> gio::glib::GString {
    gio::content_type_guess(Some(Path::new(name)), &[]).0
}

pub fn apps_for(name: &str) -> Vec<OpenWithApp> {
    let content_type = content_type(name);
    let default = gio::AppInfo::default_for_type(&content_type, false).and_then(|app| app.id());
    gio::AppInfo::all_for_type(&content_type)
        .into_iter()
        .filter_map(|app| {
            let id = app.id()?;
            Some(OpenWithApp {
                is_default: default.as_ref() == Some(&id),
                id: id.to_string(),
                name: app.display_name().to_string(),
            })
        })
        .collect()
}

pub fn open_with(path: &Path, app: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let app_info = gio::AppInfo::all_for_type(&content_type(&name))
        .into_iter()
        .chain(gio::AppInfo::all())
        .find(|candidate| candidate.id().is_some_and(|id| id == app))
        .ok_or_else(|| format!("{app} is no longer installed"))?;
    app_info
        .launch(&[gio::File::for_path(path)], None::<&gio::AppLaunchContext>)
        .map_err(|e| e.to_string())
}

/// The desktop portal's `OpenURI.OpenFile` with `ask`: GNOME's and KDE's
/// own app chooser, which then opens the file in what was chosen.
pub fn choose(_window: &tauri::WebviewWindow, path: PathBuf) -> Result<(), String> {
    let connection = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)
        .map_err(|e| e.to_string())?;
    let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let fds = gio::UnixFDList::new();
    // The list keeps a duplicate of the descriptor; ours closes on drop.
    let index = fds.append(file).map_err(|e| e.to_string())?;
    let options = gio::glib::VariantDict::new(None);
    options.insert_value("ask", &true.to_variant());
    let parameters = gio::glib::Variant::tuple_from_iter([
        // No parent window handle: the portal has no way to take one from
        // a webview's toplevel here, and centres the chooser instead.
        "".to_variant(),
        gio::glib::variant::Handle(index).to_variant(),
        options.end(),
    ]);
    connection
        .call_with_unix_fd_list_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.OpenURI",
            "OpenFile",
            Some(&parameters),
            Some(gio::glib::VariantTy::new("(o)").unwrap()),
            gio::DBusCallFlags::NONE,
            -1,
            Some(&fds),
            None::<&gio::Cancellable>,
        )
        .map(drop)
        .map_err(|e| format!("no app chooser available (xdg-desktop-portal): {e}"))
}
