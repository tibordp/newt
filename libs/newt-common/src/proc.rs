//! Subprocess spawn helpers: platform-specific flags applied at spawn
//! sites, exposed as no-ops elsewhere so call sites stay platform-agnostic.
//!
//! On Windows, a GUI-subsystem process (the release build links the
//! `windows` subsystem, so it has no console) that spawns a
//! console-subsystem child — `ssh`, `scp`, `docker`, `podman`, `kubectl`,
//! … — makes the OS allocate a fresh console window for that child, which
//! flashes up on screen. The dev build doesn't show this because it's
//! launched from a terminal and the child inherits that console.
//! Setting `CREATE_NO_WINDOW` on the child suppresses the allocation.

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

/// On Linux, arrange for the child to receive SIGTERM when the parent exits.
/// This ensures SSH/agent processes don't linger if Newt is killed.
/// On other platforms this is a no-op.
pub fn set_parent_death_signal(cmd: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_PDEATHSIG) is async-signal-safe and this is
        // the only thing we do in the pre_exec closure.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = cmd;
    }
}
