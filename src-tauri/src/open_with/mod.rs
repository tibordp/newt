//! Open With: the applications the system offers for a file, opening a
//! file in one of them, and the system's own chooser for any other —
//! `SHOpenWithDialog` on Windows, the desktop portal's on Linux, and on
//! macOS, which has none for other applications to show, the open panel
//! on Applications that Finder's "Other…" is.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

/// An application that can open a file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, specta::Type)]
pub struct OpenWithApp {
    /// What `open_with` takes: the application bundle's path on macOS,
    /// the handler's executable on Windows, the desktop file id on Linux.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// The applications the system offers for files named like `name`, the
/// default first, then by name. Blocking: call off the async runtime.
pub fn apps_for(name: &str) -> Vec<OpenWithApp> {
    let mut apps = platform::apps_for(name);
    apps.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    apps.dedup_by(|a, b| a.id == b.id);
    apps
}

/// Open `path` in the application `app` (an `OpenWithApp::id`). Blocking:
/// call off the async runtime.
pub fn open_with(path: &std::path::Path, app: &str) -> Result<(), String> {
    platform::open_with(path, app)
}

/// Show the system's chooser for `path` over `window`; whatever is chosen
/// opens the file. Returns once the chooser is up, not when it closes.
/// Blocking: call off the async runtime.
pub fn choose(window: &tauri::WebviewWindow, path: std::path::PathBuf) -> Result<(), String> {
    platform::choose(window, path)
}
