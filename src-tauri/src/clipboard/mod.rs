//! The system clipboard, one per process (on `GlobalContext`): text, and
//! files — copied or cut — in every format the platform's file managers
//! read, with word of other apps changing it where the platform gives one.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::AppHandle;

use crate::common::Error;

// Linux's formats; built elsewhere only to test them.
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod formats;

#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "linux", path = "linux.rs")]
mod platform;

/// What the clipboard holds, as of a read or a write: the same stamp later
/// means nothing has been put on it since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp(u64);

/// Files on the clipboard, and whether they were cut rather than copied.
pub struct CopiedFiles {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

type Listener = Box<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct Clipboard {
    platform: Arc<platform::Clipboard>,
    listeners: Arc<Mutex<Vec<Listener>>>,
}

impl Clipboard {
    pub fn new(app: &AppHandle) -> Self {
        let listeners: Arc<Mutex<Vec<Listener>>> = Arc::default();
        let changed = {
            let listeners = listeners.clone();
            Arc::new(move || {
                for listener in listeners.lock().iter() {
                    listener();
                }
            })
        };
        Self {
            platform: Arc::new(platform::Clipboard::new(app, changed)),
            listeners,
        }
    }

    pub async fn text(&self) -> Result<Option<String>, Error> {
        self.platform.text().await
    }

    pub async fn set_text(&self, text: String) -> Result<Stamp, Error> {
        self.platform.set_text(text).await
    }

    /// `paths` (absolute, on this machine) as files, `text` for apps that
    /// take text, and whether they are cut — all in one write.
    pub async fn set_files(
        &self,
        paths: Vec<PathBuf>,
        text: String,
        cut: bool,
    ) -> Result<Stamp, Error> {
        self.platform.set_files(paths, text, cut).await
    }

    /// The files on the clipboard, if it holds any.
    pub async fn files(&self) -> Result<Option<CopiedFiles>, Error> {
        self.platform.files().await
    }

    pub async fn stamp(&self) -> Stamp {
        self.platform.stamp().await
    }

    /// Empty the clipboard, as after pasting a cut. On Linux only what Newt
    /// put there can be taken off.
    pub async fn clear(&self) -> Result<(), Error> {
        self.platform.clear().await
    }

    /// Call `f` whenever the clipboard changes, Newt's own writes included.
    /// macOS doesn't say, so there it is never called; compare stamps.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        self.listeners.lock().push(Box::new(f));
    }
}

fn error(what: impl std::fmt::Display) -> Error {
    Error::Custom(format!("Clipboard: {what}"))
}
