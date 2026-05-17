//! Subprocess spawn helpers.
//!
//! On Windows, a GUI-subsystem process (the release build links the
//! `windows` subsystem, so it has no console) that spawns a
//! console-subsystem child — `ssh`, `scp`, `docker`, `podman`, `kubectl`,
//! … — makes the OS allocate a fresh console window for that child, which
//! flashes up on screen. The dev build doesn't show this because it's
//! launched from a terminal and the child inherits that console.
//!
//! Setting `CREATE_NO_WINDOW` on the child suppresses the allocation. This
//! trait applies it uniformly at every spawn site; on non-Windows it is a
//! no-op so call sites stay platform-agnostic.

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Suppress the console window Windows would otherwise allocate for a
/// console-subsystem child spawned from a GUI-subsystem process.
pub trait NoConsoleWindow {
    fn no_console_window(&mut self) -> &mut Self;
}

impl NoConsoleWindow for std::process::Command {
    #[cfg(windows)]
    fn no_console_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_console_window(&mut self) -> &mut Self {
        self
    }
}

impl NoConsoleWindow for tokio::process::Command {
    #[cfg(windows)]
    fn no_console_window(&mut self) -> &mut Self {
        // `creation_flags` is an inherent method on tokio's Command,
        // available only on Windows (mirrors std's CommandExt).
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_console_window(&mut self) -> &mut Self {
        self
    }
}
