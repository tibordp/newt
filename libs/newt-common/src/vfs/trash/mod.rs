//! Moving an item to the trash of the user running this process. What the
//! trash can't take is refused with `ErrorKind::NotSupported` — never copied
//! to another filesystem's trash, never deleted in its place — so the
//! operation can ask whether to delete it permanently instead.

use std::path::Path;

use crate::{Error, ErrorKind};

// Also in macOS test builds, so its tests run there too.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
mod freedesktop;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(all(unix, not(target_os = "macos")))]
pub async fn trash(path: &Path) -> Result<(), Error> {
    freedesktop::trash(path).await
}

#[cfg(target_os = "macos")]
pub async fn trash(path: &Path) -> Result<(), Error> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || macos::trash(&path)).await?
}

#[cfg(windows)]
pub async fn trash(path: &Path) -> Result<(), Error> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || windows::trash(&path)).await?
}

fn refused(message: impl Into<String>) -> Error {
    Error {
        kind: ErrorKind::NotSupported,
        message: message.into(),
    }
}
