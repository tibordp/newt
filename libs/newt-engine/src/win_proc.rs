//! Owned wrapper around a raw Win32 process `HANDLE`.
//!
//! Both Windows agent transports (WSL via `wslapi!WslLaunch`, elevated via
//! `ShellExecuteEx`) hand back a pre-existing process handle rather than a
//! `tokio::process::Child`. `std`/`tokio` can't adopt a handle into a
//! `Child` (there is no `Child::from_raw_handle`), so the watcher needs this
//! small shared wrapper instead. `Drop` terminates the process (mirrors the
//! other transports' `kill_on_drop`) so closing the window tears the agent
//! down.
//!
//! Windows-only module.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, INFINITE, TerminateProcess, WaitForSingleObject,
};

pub struct WinProcess {
    handle: HANDLE,
}

// The handle is owned solely by this struct; moving it across threads (the
// watcher task / blocking wait) is sound.
unsafe impl Send for WinProcess {}
unsafe impl Sync for WinProcess {}

impl WinProcess {
    /// Take ownership of `handle`. Caller must not close it elsewhere.
    ///
    /// # Safety
    /// `handle` must be a valid, owning process handle.
    pub unsafe fn from_raw(handle: HANDLE) -> Self {
        Self { handle }
    }

    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        use std::os::windows::process::ExitStatusExt;
        let handle = self.handle as usize;
        tokio::task::spawn_blocking(move || unsafe {
            let h = handle as HANDLE;
            WaitForSingleObject(h, INFINITE);
            let mut code: u32 = 0;
            GetExitCodeProcess(h, &mut code);
            std::process::ExitStatus::from_raw(code)
        })
        .await
        .map_err(std::io::Error::other)
    }
}

impl Drop for WinProcess {
    fn drop(&mut self) {
        unsafe {
            TerminateProcess(self.handle, 1);
            CloseHandle(self.handle);
        }
    }
}
