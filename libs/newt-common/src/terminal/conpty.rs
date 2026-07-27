//! Windows pseudo-console (ConPTY) backend.
//!
//! This is the Windows analogue of the Unix `pty-process` crate: a thin,
//! direct binding over the ConPTY API (`CreatePseudoConsole` /
//! `ResizePseudoConsole` / `ClosePseudoConsole`) with no third-party PTY
//! wrapper. Two design constraints drive the shape here:
//!
//! 1. **Close to the metal.** We call the Win32 APIs directly via
//!    `windows-sys`; the only abstraction is tokio's named-pipe type for
//!    the I/O endpoints.
//! 2. **Properly async, no wasted threads.** ConPTY communicates over
//!    pipes. Anonymous `CreatePipe` handles cannot do overlapped I/O, so
//!    instead we create *named* pipes whose server end is owned by tokio
//!    (`NamedPipeServer`, registered on the IOCP reactor) and whose client
//!    end is handed to ConPTY as a plain synchronous handle. Reading and
//!    writing the terminal is then fully async with zero dedicated
//!    threads. Process exit is observed via `RegisterWaitForSingleObject`
//!    (an OS thread-pool wait, not a thread we own) that flips a
//!    `tokio::sync::watch` the I/O paths observe.

use std::ffi::c_void;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{Mutex, watch};

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_NONE, OPEN_EXISTING};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcessId, GetExitCodeProcess,
    InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    RegisterWaitForSingleObject, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UnregisterWaitEx, UpdateProcThreadAttribute, WT_EXECUTEONLYONCE,
};

/// Process still running (ConPTY child exit codes are `u32`; this is the
/// `STILL_ACTIVE` sentinel, also `STATUS_PENDING`).
const STILL_ACTIVE: u32 = 259;

/// Per-process unique suffix source for pipe names.
static PIPE_SEQ: AtomicU64 = AtomicU64::new(0);

/// UTF-16, NUL-terminated.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Quote one argument per the Win32 `CommandLineToArgvW` rules so a child
/// using the C runtime / `argv` parser sees exactly `arg`.
fn append_quoted(arg: &str, out: &mut String) {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| c == ' ' || c == '\t' || c == '\n' || c == '\u{b}' || c == '"');
    if !needs_quotes {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Escape all pending backslashes (they precede a quote) plus
                // the quote itself.
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Trailing backslashes precede the closing quote: double them.
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
}

fn build_command_line(program: &str, args: &[String]) -> Vec<u16> {
    let mut s = String::new();
    append_quoted(program, &mut s);
    for a in args {
        s.push(' ');
        append_quoted(a, &mut s);
    }
    wide(&s)
}

/// Build a `CREATE_UNICODE_ENVIRONMENT` block: the current environment with
/// `overrides` applied (case-insensitive on the variable name, as Windows
/// treats env names). `None` ⇒ inherit the parent block verbatim.
fn build_env_block(overrides: Option<&[(String, String)]>) -> Option<Vec<u16>> {
    let overrides = overrides?;
    // Case-insensitive key ordering is what the OS expects for the block.
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (k, v) in std::env::vars() {
        map.insert(k.to_uppercase(), (k, v));
    }
    for (k, v) in overrides {
        map.insert(k.to_uppercase(), (k.clone(), v.clone()));
    }
    let mut block: Vec<u16> = Vec::new();
    for (_, (k, v)) in map {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0); // double-NUL terminator
    Some(block)
}

/// Open the ConPTY-side (client) end of one of our named pipes as a plain
/// **synchronous** handle. ConPTY does its own blocking I/O on this handle
/// from conhost's threads; only *our* (server) end needs to be overlapped.
fn open_pipe_client(name: &str, access: u32) -> io::Result<HANDLE> {
    let wname = wide(name);
    // SAFETY: `wname` is a valid NUL-terminated wide string that outlives
    // the call; all other arguments are well-formed per CreateFileW.
    let h = unsafe {
        CreateFileW(
            wname.as_ptr(),
            access,
            FILE_SHARE_NONE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(h)
}

/// A broken / disconnected pipe is the normal end-of-stream condition for
/// a ConPTY output pipe once the console is torn down.
fn is_pipe_end(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
        // ERROR_BROKEN_PIPE / ERROR_PIPE_NOT_CONNECTED / ERROR_NO_DATA
        || matches!(e.raw_os_error(), Some(109) | Some(233) | Some(232))
}

fn map_read(res: io::Result<usize>) -> io::Result<Option<usize>> {
    match res {
        Ok(0) => Ok(None),
        Ok(n) => Ok(Some(n)),
        Err(e) if is_pipe_end(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Set to `true` when the child process exits. A `watch` (not a `Notify`)
/// because *both* `read` and `wait` must observe the transition, and the
/// value must persist so a late observer still sees it. Shared (via `Arc`)
/// between the `Conpty` and the OS thread-pool wait callback.
struct ExitState {
    tx: watch::Sender<bool>,
}

/// Trampoline invoked by the system thread pool when the child exits.
///
/// `ctx` is a `*const ExitState` obtained from `Arc::into_raw`; we only
/// *borrow* it here (the matching `Arc::from_raw` happens in `Drop` after
/// `UnregisterWaitEx` has guaranteed this callback is no longer running).
unsafe extern "system" fn exit_trampoline(ctx: *mut c_void, _timed_out: u8) {
    let state = unsafe { &*(ctx as *const ExitState) };
    let _ = state.tx.send(true);
}

/// A live pseudo-console with its child process.
///
/// Raw handles are stored as integers so the struct is `Send + Sync`
/// (HANDLE/HPCON are raw pointers); they are only ever used from methods
/// below, on the thread that owns the value or behind the tokio mutexes.
pub struct Conpty {
    hpc: isize, // HPCON
    process: usize,
    wait_handle: usize,
    /// Guards the one-shot `ClosePseudoConsole`: called both on child exit
    /// (to flush + EOF the output pipe) and from `Drop`.
    closed: AtomicBool,
    /// The `Arc::into_raw` pointer handed to the wait callback as context;
    /// reclaimed exactly once in `Drop`.
    exit_ctx: *const ExitState,
    /// Held purely to keep the `watch::Sender` (inside `ExitState`) alive
    /// for the console's lifetime; read back only via `exit_ctx` in `Drop`.
    #[allow(dead_code)]
    exit: Arc<ExitState>,
    /// Observes the exit flag; cloned per `read`/`wait` caller.
    exit_rx: watch::Receiver<bool>,
    /// Our async write end (child stdin).
    writer: Mutex<NamedPipeServer>,
    /// Our async read end (child stdout/stderr, ConPTY-rendered VT).
    reader: Mutex<NamedPipeServer>,
}

// SAFETY: the integer-encoded handles are only used through the Win32
// calls in this module; `exit_ctx` is never dereferenced via a shared
// `&Conpty` (only in `Drop` and the wait trampoline, both ordering-
// guarded by `UnregisterWaitEx`), and the pipe servers + watch channel
// are themselves Send+Sync. No interior raw-pointer aliasing escapes.
unsafe impl Send for Conpty {}
unsafe impl Sync for Conpty {}

impl Conpty {
    /// Spawn `program` (with `args`) attached to a fresh pseudo-console of
    /// `rows`×`cols`. `env` overrides are layered onto the inherited
    /// environment; `cwd` is the child's working directory.
    pub async fn spawn(
        program: &str,
        args: &[String],
        env: Option<&[(String, String)]>,
        cwd: Option<&std::path::Path>,
        rows: u16,
        cols: u16,
    ) -> io::Result<Self> {
        let pid = unsafe { GetCurrentProcessId() };
        let seq = PIPE_SEQ.fetch_add(1, Ordering::Relaxed);
        let in_name = format!(r"\\.\pipe\newt-conpty-{pid}-{seq}-in");
        let out_name = format!(r"\\.\pipe\newt-conpty-{pid}-{seq}-out");

        // Server ends are ours (overlapped, reactor-backed). We write the
        // child's input via `in_*`, read its output via `out_*`.
        let in_server = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(&in_name)?;
        let out_server = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(&out_name)?;

        // ConPTY reads child stdin from `conpty_in` and writes child output
        // to `conpty_out` (synchronous client handles). The whole
        // open → CreatePseudoConsole → spawn → close sequence is kept
        // synchronous and `await`-free: the raw handles aren't `Send`
        // (the async fn's future must be), and — crucially — the
        // ConPTY-side handles must stay open until *after* the child is
        // created, otherwise the child silently falls back to inheriting
        // the parent console instead of attaching to the pseudo-console.
        // The pipe is connected at the NPFS level the instant
        // `CreateFileW` returns, so finishing the tokio server-side
        // handshake afterwards is sound (any early conhost output is just
        // kernel-buffered until we connect and read it).
        let (hpc, process) = {
            let conpty_in = open_pipe_client(&in_name, GENERIC_READ)?;
            let conpty_out = match open_pipe_client(&out_name, GENERIC_WRITE) {
                Ok(h) => h,
                Err(e) => {
                    unsafe { CloseHandle(conpty_in) };
                    return Err(e);
                }
            };
            // SAFETY: handles are valid; phpc points at a live HPCON slot.
            let mut hpc: HPCON = 0;
            let size = COORD {
                X: cols as i16,
                Y: rows as i16,
            };
            let hr = unsafe { CreatePseudoConsole(size, conpty_in, conpty_out, 0, &mut hpc) };
            if hr != 0 {
                unsafe {
                    CloseHandle(conpty_in);
                    CloseHandle(conpty_out);
                }
                return Err(io::Error::other(format!(
                    "CreatePseudoConsole failed: 0x{hr:08x}"
                )));
            }

            let process = match unsafe { spawn_child(hpc, program, args, env, cwd) } {
                Ok(p) => p,
                Err(e) => {
                    unsafe {
                        ClosePseudoConsole(hpc);
                        CloseHandle(conpty_in);
                        CloseHandle(conpty_out);
                    }
                    return Err(e);
                }
            };

            // Child created and attached; our copies are no longer needed
            // (CreatePseudoConsole duplicated what conhost keeps).
            unsafe {
                CloseHandle(conpty_in);
                CloseHandle(conpty_out);
            }
            // Encode as integers so nothing `!Send` crosses the awaits
            // below (the async fn's future must be `Send`).
            (hpc, process as usize)
        };

        // Finish the tokio server-side handshake (clients already
        // connected at CreateFileW time).
        in_server.connect().await?;
        out_server.connect().await?;

        // Async exit notification via the OS thread pool.
        let (tx, exit_rx) = watch::channel(false);
        let exit = Arc::new(ExitState { tx });
        let exit_ctx = Arc::into_raw(exit.clone());
        let mut wait_handle: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is a valid process handle; `exit_ctx` stays
        // alive until Drop reclaims it after UnregisterWaitEx.
        let ok = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                process as HANDLE,
                Some(exit_trampoline),
                exit_ctx as *const c_void,
                u32::MAX, // INFINITE
                WT_EXECUTEONLYONCE,
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            unsafe {
                drop(Arc::from_raw(exit_ctx));
                ClosePseudoConsole(hpc);
                TerminateProcess(process as HANDLE, 1);
                CloseHandle(process as HANDLE);
            }
            return Err(e);
        }

        Ok(Self {
            hpc,
            process,
            wait_handle: wait_handle as usize,
            closed: AtomicBool::new(false),
            exit_ctx,
            exit,
            exit_rx,
            writer: Mutex::new(in_server),
            reader: Mutex::new(out_server),
        })
    }

    /// Write bytes to the child's input.
    pub async fn write(&self, data: &[u8]) -> io::Result<()> {
        self.writer.lock().await.write_all(data).await
    }

    /// Close the pseudo-console exactly once. This is what makes EOF
    /// deterministic: `ClosePseudoConsole` tells conhost to flush every
    /// byte it has buffered and *then* close the output pipe's write end,
    /// so our reader sees the real end-of-stream after the last output —
    /// no timeouts, no races with conhost's render latency.
    fn close_console(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            // SAFETY: hpc is a live HPCON, closed at most once (the swap
            // guard) and never used afterwards.
            unsafe { ClosePseudoConsole(self.hpc) };
        }
    }

    /// Read a chunk of child output. `Ok(None)` signals EOF.
    ///
    /// Unlike a Unix PTY master, the ConPTY output pipe is owned by
    /// conhost, not the child: it does **not** break on its own when the
    /// child exits, only when we `ClosePseudoConsole`. ConPTY exposes no
    /// "client drained" signal, so the moment the child exits we close the
    /// console — that call makes conhost flush *everything* it has
    /// buffered and then break the pipe. The reads here deliver that
    /// flushed tail and then a clean EOF. This is fully deterministic: no
    /// timer, no teardown latency, and (verified by the
    /// `bursty_then_exit_preserves_tail` test) no trailing-output loss
    /// even when a child floods output and exits immediately. While the
    /// child is alive a normal blocking read is used (no polling, no
    /// wasted thread).
    ///
    /// The one residual gap is inherent to ConPTY and unfixable from here:
    /// a child that exits within roughly one render tick of its *first*
    /// write, before conhost has read its stdout at all, can still lose
    /// that output. Real shells stay alive and never hit this.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let mut guard = self.reader.lock().await;

        if *self.exit_rx.borrow() {
            // Child has exited: close the console, which makes conhost
            // flush everything it has buffered and then break the output
            // pipe. Subsequent reads deliver that tail and then a clean,
            // deterministic EOF — no timer, no teardown latency.
            self.close_console();
            return map_read(guard.read(buf).await);
        }

        let mut exit_rx = self.exit_rx.clone();
        tokio::select! {
            res = guard.read(buf) => map_read(res),
            _ = exit_rx.changed() => {
                self.close_console();
                map_read(guard.read(buf).await)
            }
        }
    }

    /// Resize the pseudo-console.
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        let size = COORD {
            X: cols as i16,
            Y: rows as i16,
        };
        // SAFETY: hpc is a live HPCON for this Conpty's lifetime.
        let hr = unsafe { ResizePseudoConsole(self.hpc, size) };
        if hr != 0 {
            return Err(io::Error::other(format!(
                "ResizePseudoConsole failed: 0x{hr:08x}"
            )));
        }
        Ok(())
    }

    /// Await child exit and return its exit code.
    pub async fn wait(&self) -> io::Result<u32> {
        // The watch value persists, so this resolves immediately even if
        // the child exited before `wait` was first called.
        let mut rx = self.exit_rx.clone();
        while !*rx.borrow_and_update() {
            // Sender lives as long as the wait registration; the only way
            // `changed()` errors is teardown, which also means "gone".
            if rx.changed().await.is_err() {
                break;
            }
        }
        let mut code: u32 = STILL_ACTIVE;
        // SAFETY: process handle valid until Drop.
        let ok = unsafe { GetExitCodeProcess(self.process as HANDLE, &mut code) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code)
    }
}

impl Drop for Conpty {
    fn drop(&mut self) {
        // Idempotent with the on-exit close in `read`; asks the child to
        // exit by tearing down its console.
        self.close_console();
        unsafe {
            // Block until any in-flight wait callback has finished and no
            // further callbacks can fire — this makes reclaiming `exit_ctx`
            // and closing the process handle safe.
            UnregisterWaitEx(self.wait_handle as HANDLE, INVALID_HANDLE_VALUE);
            drop(Arc::from_raw(self.exit_ctx));

            // Backstop in case the child ignored the broken console.
            TerminateProcess(self.process as HANDLE, 1);
            CloseHandle(self.process as HANDLE);
        }
    }
}

/// `CreateProcessW` with the pseudo-console thread attribute. Returns the
/// child process handle (the thread handle is closed here).
///
/// # Safety
/// `hpc` must be a live HPCON.
unsafe fn spawn_child(
    hpc: HPCON,
    program: &str,
    args: &[String],
    env: Option<&[(String, String)]>,
    cwd: Option<&std::path::Path>,
) -> io::Result<HANDLE> {
    // Proc-thread attribute list sized for one attribute.
    let mut attr_size: usize = 0;
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size);
    }
    // The attribute list holds pointer-sized fields and must be
    // pointer-aligned; a `Vec<u8>` is only 1-aligned by contract (it works
    // by accident on the system allocator). Back it with `usize` storage,
    // rounded up to cover `attr_size` bytes, for guaranteed alignment.
    let nwords = attr_size.div_ceil(std::mem::size_of::<usize>()).max(1);
    let mut attr_buf = vec![0usize; nwords];
    let attr_list = attr_buf.as_mut_ptr() as *mut c_void;
    // SAFETY: buffer is pointer-aligned and ≥ the size the first call
    // reported.
    if unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: attribute list initialized; hpc lives for the call.
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const c_void,
            std::mem::size_of::<HPCON>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        let e = io::Error::last_os_error();
        unsafe { DeleteProcThreadAttributeList(attr_list) };
        return Err(e);
    }

    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attr_list;
    // Without this, the child inherits the *parent's* console/std handles
    // and the pseudo-console attribute is silently ineffective. Pointing
    // the std handles at INVALID_HANDLE_VALUE forces it to attach to the
    // pseudo-console instead. (Matches wezterm / Windows Terminal.)
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

    let mut cmdline = build_command_line(program, args);
    let env_block = build_env_block(env);
    let cwd_wide = cwd.map(|p| wide(&p.to_string_lossy()));

    let mut flags = EXTENDED_STARTUPINFO_PRESENT;
    let env_ptr = match &env_block {
        Some(b) => {
            flags |= CREATE_UNICODE_ENVIRONMENT;
            b.as_ptr() as *const c_void
        }
        None => std::ptr::null(),
    };
    let cwd_ptr = cwd_wide
        .as_ref()
        .map(|w| w.as_ptr())
        .unwrap_or(std::ptr::null());

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: all pointers are valid for the duration of the call;
    // cmdline is a writable NUL-terminated buffer as CreateProcessW wants.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0, // bInheritHandles = FALSE; ConPTY wires the child's stdio
            flags,
            env_ptr,
            cwd_ptr,
            &si.StartupInfo as *const _,
            &mut pi,
        )
    };
    unsafe { DeleteProcThreadAttributeList(attr_list) };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { CloseHandle(pi.hThread) };
    Ok(pi.hProcess)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: spawn a real child under a pseudo-console, read its
    /// rendered output to EOF (exercising the post-exit drain, since
    /// ConPTY never breaks the pipe on its own), and collect the exit
    /// code. The child lives ~1s (via `ping`) so conhost actually renders
    /// its output — a microsecond-lived child hits ConPTY's documented
    /// cold-start truncation race, which is not what we're testing here.
    #[tokio::test]
    async fn echo_roundtrip() {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        // Interactive shell — the real product scenario. It stays alive and
        // streams, so conhost actually renders; we drive it over stdin.
        let conpty = Conpty::spawn(&comspec, &[], None, None, 24, 80)
            .await
            .expect("spawn pseudo-console");

        conpty
            .write(b"echo conpty_marker\r\nexit\r\n")
            .await
            .expect("write stdin");

        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        while let Some(n) = conpty.read(&mut buf).await.expect("read") {
            out.extend_from_slice(&buf[..n]);
            if out.len() > 64 * 1024 {
                break; // safety net so a misbehaving test can't run away
            }
        }
        let code = conpty.wait().await.expect("wait");

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("conpty_marker"), "output was: {text:?}");
        assert_eq!(code, 0, "expected clean exit");
    }

    /// Worst case for the exit/drain logic: a non-interactive child that
    /// floods output and then exits *immediately* after its last write.
    /// If the final marker survives here, trailing output isn't being
    /// truncated at console teardown.
    #[tokio::test]
    async fn bursty_then_exit_preserves_tail() {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let conpty = Conpty::spawn(
            &comspec,
            &[
                "/c".to_string(),
                "for /l %i in (1,1,300) do @echo line%i & echo TAIL_MARKER_Z9".to_string(),
            ],
            None,
            None,
            24,
            80,
        )
        .await
        .expect("spawn pseudo-console");

        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while let Some(n) = conpty.read(&mut buf).await.expect("read") {
            out.extend_from_slice(&buf[..n]);
            if out.len() > 1 << 20 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("TAIL_MARKER_Z9"),
            "trailing output truncated; tail was: {:?}",
            &text[text.len().saturating_sub(120)..]
        );
    }

    /// `wait` must resolve even when called *after* the child has already
    /// exited (the watch value persists; this is the common ordering).
    #[tokio::test]
    async fn wait_after_exit() {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let conpty = Conpty::spawn(
            &comspec,
            &["/c".to_string(), "exit 7".to_string()],
            None,
            None,
            24,
            80,
        )
        .await
        .expect("spawn pseudo-console");

        // Drain so the child certainly finishes before we wait.
        let mut buf = [0u8; 1024];
        while conpty.read(&mut buf).await.expect("read").is_some() {}

        assert_eq!(conpty.wait().await.expect("wait"), 7);
    }
}
