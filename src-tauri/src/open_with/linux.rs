use std::path::{Path, PathBuf};

use gio::glib::translate::{IntoGlib, ToGlibPtr, from_glib_full};
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
    // gio 0.18's `call_with_unix_fd_list_sync` wraps the reply's fd list as
    // if it were never NULL, but GLib leaves it NULL for a reply without
    // descriptors — OpenFile's — and the binding panics. Later gio returns
    // it as an `Option` (0.22 does); until the GTK stack Tauri pins moves
    // past 0.18, call GLib directly and decline the reply's descriptors.
    let reply_type = gio::glib::VariantTy::new("(o)").unwrap();
    // SAFETY: every pointer comes from a live value borrowed for the call;
    // a NULL `out_fd_list` tells GLib not to return descriptors.
    let error = unsafe {
        let mut error = std::ptr::null_mut();
        let reply = gio::ffi::g_dbus_connection_call_with_unix_fd_list_sync(
            connection.to_glib_none().0,
            "org.freedesktop.portal.Desktop".to_glib_none().0,
            "/org/freedesktop/portal/desktop".to_glib_none().0,
            "org.freedesktop.portal.OpenURI".to_glib_none().0,
            "OpenFile".to_glib_none().0,
            parameters.to_glib_none().0,
            reply_type.to_glib_none().0,
            gio::DBusCallFlags::NONE.into_glib(),
            -1,
            fds.to_glib_none().0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut error,
        );
        if error.is_null() {
            drop(from_glib_full::<_, gio::glib::Variant>(reply));
            return Ok(());
        }
        from_glib_full::<_, gio::glib::Error>(error)
    };
    Err(format!(
        "no app chooser available (xdg-desktop-portal): {error}"
    ))
}
