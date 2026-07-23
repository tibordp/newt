//! Launch the agent inside a WSL distribution via `wslapi!WslLaunch`.
//!
//! `wslapi.dll` is loaded at runtime (`LoadLibraryW`), never linked, so a
//! machine without WSL just fails this one transport instead of failing to
//! start. The bundled Linux-musl agent already lives on the Windows
//! filesystem; from inside WSL it is reachable at `/mnt/<drive>/…` and is
//! directly executable (DrvFs default-mounts world-exec), so we exec it
//! straight — no bootstrap, no upload.
//!
//! `WslLaunch` returns a raw Win32 process `HANDLE` plus whatever stdio
//! handles we hand it. The process is owned via the shared
//! [`super::win_proc::WinProcess`] wrapper. The stdio pipe handles *can* be
//! adopted (`File::from_raw_handle`); we wrap them in `tokio::fs::File`,
//! whose read/write run on tokio's blocking pool — the simplest correct
//! bridge for anonymous pipes (no overlapped I/O).
//!
//! Windows-only module.

use std::mem::size_of;
use std::os::windows::io::FromRawHandle;
use std::ptr::null_mut;

use newt_common::agent_resolver::AgentResolver;
use tokio::io::{AsyncRead, AsyncWrite};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Pipes::CreatePipe;

use super::win_proc::WinProcess;
use crate::common::Error;

/// `HRESULT WslLaunch(PCWSTR, PCWSTR, BOOL, HANDLE, HANDLE, HANDLE, HANDLE*)`.
type WslLaunchFn = unsafe extern "system" fn(
    *const u16,
    *const u16,
    i32,
    HANDLE,
    HANDLE,
    HANDLE,
    *mut HANDLE,
) -> i32;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Resolve `wslapi!WslLaunch`, loading `wslapi.dll` once (kept resident for
/// the process lifetime). `None` when WSL isn't installed.
fn wsl_launch_fn() -> Option<WslLaunchFn> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<usize>> = OnceLock::new();
    let cached = CACHE.get_or_init(|| unsafe {
        let name = wide("wslapi.dll");
        let module = LoadLibraryW(name.as_ptr());
        if module.is_null() {
            return None;
        }
        // Intentionally leak the module — it must outlive every launched
        // distro and the process exits soon after the last one anyway.
        GetProcAddress(module, c"WslLaunch".as_ptr() as *const u8).map(|f| f as usize)
    });
    cached.map(|v| unsafe { std::mem::transmute::<usize, WslLaunchFn>(v) })
}

/// `WslLaunch`'s relay process attaches to the caller's console, and a
/// windows-subsystem build has none — so the OS would allocate a fresh
/// *visible* console window per launch. Pre-own a hidden one instead:
/// `AllocConsoleWithOptions(NO_WINDOW)` where available (Win11 24H2+),
/// else `AllocConsole` + hide (one brief flash, first WSL session only).
/// No-op when a console already exists (console-subsystem dev builds,
/// launches from a terminal).
fn ensure_hidden_console() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

        if !GetConsoleWindow().is_null() {
            return;
        }

        // From consoleapi.h; absent from windows-sys 0.59, and it has to
        // be resolved dynamically regardless — older Windows lacks it.
        #[repr(C)]
        struct AllocConsoleOptions {
            mode: u32, // 2 = ALLOC_CONSOLE_MODE_NO_WINDOW
            use_show_window: i32,
            show_window: u16,
        }
        type AllocConsoleWithOptionsFn =
            unsafe extern "system" fn(*const AllocConsoleOptions, *mut u32) -> i32;
        let kernel32 = GetModuleHandleW(wide("kernel32.dll").as_ptr());
        if !kernel32.is_null()
            && let Some(f) =
                GetProcAddress(kernel32, c"AllocConsoleWithOptions".as_ptr() as *const u8)
        {
            let f = std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                AllocConsoleWithOptionsFn,
            >(f);
            let opts = AllocConsoleOptions {
                mode: 2,
                use_show_window: 0,
                show_window: 0,
            };
            let mut result = 0u32;
            if f(&opts, &mut result) >= 0 {
                return;
            }
        }

        if AllocConsole() != 0 {
            let hwnd = GetConsoleWindow();
            if !hwnd.is_null() {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    });
}

/// Translate a Windows drive path to its default-automount WSL path:
/// `C:\a\b` → `/mnt/c/a/b`. Errors on UNC / non-drive paths (the bundled
/// agent always lives under a local drive). Assumes the default
/// `[automount] root = /mnt`.
fn to_wsl_path(p: &std::path::Path) -> Result<String, Error> {
    let s = p
        .to_str()
        .ok_or_else(|| Error::Custom("agent path is not valid UTF-8".into()))?;
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    let b = s.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || (b[2] != b'\\' && b[2] != b'/')
    {
        return Err(Error::Custom(format!(
            "cannot translate {:?} to a WSL /mnt path (expected a drive path)",
            s
        )));
    }
    let drive = (b[0] as char).to_ascii_lowercase();
    let rest = s[2..].replace('\\', "/");
    Ok(format!("/mnt/{}{}", drive, rest))
}

/// Stdio + process handed back from [`spawn_wsl`]. The caller wires
/// `stdout`/`stdin` into the RPC stream and `stderr` into the connection
/// log, exactly as the `Command`-spawned transports do.
pub struct WslSpawn {
    pub process: WinProcess,
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    pub stderr: Box<dyn AsyncRead + Send + Unpin>,
}

/// One anonymous pipe. Both ends start inheritable; the caller clears the
/// inherit flag on whichever end the parent keeps.
unsafe fn make_pipe() -> Result<(HANDLE, HANDLE), Error> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &sa, 0) } == 0 {
        return Err(Error::Custom("CreatePipe failed".into()));
    }
    Ok((read, write))
}

unsafe fn clear_inherit(h: HANDLE) {
    unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) };
}

unsafe fn adopt_read(h: HANDLE) -> Box<dyn AsyncRead + Send + Unpin> {
    let f = unsafe { std::fs::File::from_raw_handle(h as _) };
    Box::new(tokio::fs::File::from_std(f))
}

unsafe fn adopt_write(h: HANDLE) -> Box<dyn AsyncWrite + Send + Unpin> {
    let f = unsafe { std::fs::File::from_raw_handle(h as _) };
    Box::new(tokio::fs::File::from_std(f))
}

/// Launch the agent inside `distro` via `WslLaunch`.
pub async fn spawn_wsl(
    distro: &str,
    agent_resolver: &dyn AgentResolver,
) -> Result<WslSpawn, Error> {
    let launch = wsl_launch_fn()
        .ok_or_else(|| Error::Custom("WSL is not available on this system".into()))?;
    ensure_hidden_console();

    // Distro arch always equals the Windows host arch: WSL2's kernel is
    // host-arch, and WSL1 executes native ELF with no emulation (ARM64
    // Windows runs aarch64 distros) — so the host arch is the correct
    // agent triple.
    let triple = if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-musl"
    } else {
        "x86_64-unknown-linux-musl"
    };
    let agent_path = agent_resolver.find_agent_binary(triple)?;
    let wsl_path = to_wsl_path(&agent_path)?;
    // WslLaunch runs the command via the user's shell; single-quote so a
    // spaced install path survives word splitting.
    let command = format!("'{}'", wsl_path.replace('\'', r"'\''"));

    unsafe {
        // stdin: child reads, parent writes. stdout/stderr: child writes,
        // parent reads.
        let (child_in, parent_in) = make_pipe()?;
        let (parent_out, child_out) = make_pipe()?;
        let (parent_err, child_err) = make_pipe()?;
        clear_inherit(parent_in);
        clear_inherit(parent_out);
        clear_inherit(parent_err);

        let distro_w = wide(distro);
        let command_w = wide(&command);
        let mut proc: HANDLE = null_mut();
        let hr = launch(
            distro_w.as_ptr(),
            command_w.as_ptr(),
            0, // useCurrentWorkingDirectory = FALSE
            child_in,
            child_out,
            child_err,
            &mut proc,
        );

        // The child-side ends were duplicated into the WSL session by
        // WslLaunch; the parent must drop them so EOF propagates.
        CloseHandle(child_in);
        CloseHandle(child_out);
        CloseHandle(child_err);

        if hr < 0 || proc.is_null() {
            CloseHandle(parent_in);
            CloseHandle(parent_out);
            CloseHandle(parent_err);
            return Err(Error::Custom(format!(
                "WslLaunch failed for distro {:?} (hr=0x{:08X})",
                distro, hr as u32
            )));
        }

        Ok(WslSpawn {
            process: WinProcess::from_raw(proc),
            stdin: adopt_write(parent_in),
            stdout: adopt_read(parent_out),
            stderr: adopt_read(parent_err),
        })
    }
}
