#[cfg(unix)]
use std::{ffi::CStr, mem::MaybeUninit, os::unix::process::ExitStatusExt, sync::atomic::AtomicU32};

#[cfg(unix)]
use parking_lot::Mutex;
#[cfg(unix)]
use pty_process::{OwnedReadPty, OwnedWritePty};
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(windows)]
use std::sync::atomic::AtomicU32;

#[cfg(windows)]
use parking_lot::Mutex;

#[cfg(any(unix, windows))]
use log::{debug, error, info};

use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    Error,
    rpc::{Communicator, StreamId, StreamReceiver, StreamSender},
};

#[cfg(windows)]
mod conpty;

#[cfg(unix)]
impl From<pty_process::Error> for Error {
    fn from(e: pty_process::Error) -> Self {
        Self {
            kind: crate::ErrorKind::Other,
            message: e.to_string(),
        }
    }
}

/// Bytes read in one PTY pull. Matches a typical terminal flush; bigger
/// reads block emitting until they fill, smaller reads waste syscalls.
#[cfg(any(unix, windows))]
const PTY_READ_BUFFER_SIZE: usize = 1024;

/// Buffer size for `getpwuid_r`. POSIX gives no upper bound; 1 KiB is the
/// long-standing convention and is enough for every reasonable passwd entry.
#[cfg(unix)]
const PASSWD_BUFFER_SIZE: usize = 1024;

/// A terminal, unique only within its session: each session's terminal
/// client counts from 0, a remote one in its own agent, so every window's
/// first terminal is 0. Resolve a handle through its session, never in a
/// process-wide table.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct TerminalHandle(pub u32);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, specta::Type)]
pub struct TerminalOptions {
    /// Cwd as a VFS path, not `std::path` — it crosses the RPC boundary
    /// and is converted to a native path by the side that spawns the PTY
    /// (the agent in a remote session), in its own OS.
    pub working_dir: Option<crate::vfs::path::PathBuf>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<Vec<(String, String)>>,
    /// A command line for the platform shell, run instead of `command`:
    /// the side that spawns the terminal picks the shell, since that's the
    /// OS it runs on.
    pub shell_command: Option<String>,
}

impl TerminalOptions {
    /// `shell_command` as the program and arguments that run it here.
    fn resolve_shell_command(mut self) -> Self {
        if let Some(line) = self.shell_command.take() {
            let (program, args) = crate::shell::run_via_shell(&line);
            self.command = Some(program);
            self.args = Some(args);
        }
        self
    }
}

#[async_trait::async_trait]
pub trait TerminalClient: Send + Sync {
    async fn create(&self, options: TerminalOptions) -> Result<TerminalHandle, Error>;
    async fn kill(&self, handle: TerminalHandle) -> Result<(), Error>;

    async fn resize(&self, handle: TerminalHandle, rows: u16, cols: u16) -> Result<(), Error>;
    async fn input(&self, handle: TerminalHandle, data: Vec<u8>) -> Result<(), Error>;
    async fn read(&self, handle: TerminalHandle) -> Result<Option<Vec<u8>>, Error>;
    async fn wait(&self, handle: TerminalHandle) -> Result<ExitStatus, Error>;
}

#[cfg(unix)]
struct LocalTerminal {
    pty_read: tokio::sync::Mutex<OwnedReadPty>,
    pty_write: tokio::sync::Mutex<OwnedWritePty>,
    child: tokio::sync::Mutex<tokio::process::Child>,
}

#[cfg(unix)]
struct LocalInner {
    handle: AtomicU32,
    terminals: Mutex<HashMap<TerminalHandle, Arc<LocalTerminal>>>,
    shell_integration: Option<Arc<crate::shell_control::ShellIntegration>>,
}

#[cfg(unix)]
pub struct Local(Arc<LocalInner>);

#[cfg(windows)]
struct WinInner {
    handle: AtomicU32,
    terminals: Mutex<HashMap<TerminalHandle, Arc<conpty::Conpty>>>,
    shell_integration: Option<Arc<crate::shell_control::ShellIntegration>>,
}

#[cfg(windows)]
pub struct Local(Arc<WinInner>);

impl Default for Local {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl Local {
    pub fn new() -> Self {
        Self::with_shell_integration(None)
    }

    pub fn with_shell_integration(
        shell_integration: Option<Arc<crate::shell_control::ShellIntegration>>,
    ) -> Self {
        Self(Arc::new(LocalInner {
            handle: AtomicU32::new(0),
            terminals: Mutex::new(HashMap::new()),
            shell_integration,
        }))
    }
}

#[cfg(windows)]
impl Local {
    pub fn new() -> Self {
        Self::with_shell_integration(None)
    }

    pub fn with_shell_integration(
        shell_integration: Option<Arc<crate::shell_control::ShellIntegration>>,
    ) -> Self {
        Self(Arc::new(WinInner {
            handle: AtomicU32::new(0),
            terminals: Mutex::new(HashMap::new()),
            shell_integration,
        }))
    }

    fn get(&self, handle: TerminalHandle) -> Result<Arc<conpty::Conpty>, Error> {
        self.0
            .terminals
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Error::custom("terminal not found"))
    }
}

#[cfg(windows)]
#[async_trait::async_trait]
impl TerminalClient for Local {
    async fn create(&self, options: TerminalOptions) -> Result<TerminalHandle, Error> {
        info!("terminal::create called with options: {:?}", options);
        let options = options.resolve_shell_command();
        let handle = TerminalHandle(
            self.0
                .handle
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );

        // No explicit command ⇒ the user's default shell. COMSPEC always
        // points at cmd.exe; it is the one shell guaranteed present.
        let (program, args) = match options.command {
            Some(cmd) => (cmd, options.args.unwrap_or_default()),
            None => (
                std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()),
                Vec::new(),
            ),
        };

        // Convert here — this runs in the process that owns the FS (the
        // agent in a remote session), so its own OS cfg is the right one.
        // `launch_cwd` (not `to_native`): cmd.exe rejects the verbatim
        // `\\?\C:\…` form and would silently open in %SystemRoot%.
        let cwd = options.working_dir.as_ref().map(|p| p.launch_cwd());

        // Shell-integration env first, so caller-specified env can still
        // override it.
        let mut env = self
            .0
            .shell_integration
            .as_ref()
            .map(|si| si.spawn_env(Some(handle)))
            .unwrap_or_default();
        env.extend(options.env.unwrap_or_default());
        let env = (!env.is_empty()).then_some(env);

        // ConPTY needs an initial size; the frontend issues a real resize
        // as soon as the xterm mounts, so the default is transient.
        let conpty = conpty::Conpty::spawn(&program, &args, env.as_deref(), cwd.as_deref(), 24, 80)
            .await
            .map_err(|e| {
                error!("conpty spawn failed: {e}");
                Error::from(e)
            })?;

        self.0.terminals.lock().insert(handle, Arc::new(conpty));
        info!("terminal {:?} created successfully", handle);
        Ok(handle)
    }

    async fn kill(&self, handle: TerminalHandle) -> Result<(), Error> {
        info!("terminal::kill {:?}", handle);
        // Dropping the Conpty tears down the pseudo-console and child.
        self.0
            .terminals
            .lock()
            .remove(&handle)
            .ok_or_else(|| Error::custom("terminal not found"))?;
        Ok(())
    }

    async fn resize(&self, handle: TerminalHandle, rows: u16, cols: u16) -> Result<(), Error> {
        debug!("terminal::resize {:?} rows={} cols={}", handle, rows, cols);
        self.get(handle)?.resize(rows, cols)?;
        Ok(())
    }

    async fn input(&self, handle: TerminalHandle, data: Vec<u8>) -> Result<(), Error> {
        self.get(handle)?.write(&data).await?;
        Ok(())
    }

    async fn read(&self, handle: TerminalHandle) -> Result<Option<Vec<u8>>, Error> {
        let conpty = self.get(handle)?;
        let mut buf = [0u8; PTY_READ_BUFFER_SIZE];
        match conpty.read(&mut buf).await? {
            Some(n) if n > 0 => Ok(Some(buf[..n].to_vec())),
            _ => Ok(None),
        }
    }

    async fn wait(&self, handle: TerminalHandle) -> Result<ExitStatus, Error> {
        let code = self.get(handle)?.wait().await?;
        Ok(ExitStatus {
            code: Some(code as i32),
            signal: None,
        })
    }
}

#[cfg(unix)]
#[async_trait::async_trait]
impl TerminalClient for Local {
    async fn create(&self, options: TerminalOptions) -> Result<TerminalHandle, Error> {
        info!("terminal::create called with options: {:?}", options);
        let options = options.resolve_shell_command();
        let inner = self.0.clone();
        let ret = tokio::task::spawn_blocking(move || {
            let handle = TerminalHandle(
                inner
                    .handle
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );

            debug!("allocating PTY for handle {:?}", handle);
            let pty_master = pty_process::Pty::new()?;
            debug!("PTY master allocated, getting pts");
            let pty_slave = pty_master.pts()?;
            debug!("PTY slave obtained");

            let mut cmd = if let Some(command) = options.command {
                info!("spawning command: {}", command);
                let mut cmd = pty_process::Command::new(command);
                if let Some(args) = options.args {
                    cmd.args(args);
                }
                cmd
            } else {
                let user = ShellUser::from_env()?;
                let login = login_shell_flag(&user.shell);
                info!(
                    "spawning default shell: {} (login: {})",
                    user.shell,
                    login.is_some()
                );
                let mut cmd = pty_process::Command::new(&user.shell);
                cmd.args(login);
                cmd.env("USER", user.user);
                cmd.env("TERM", "xterm-256color");
                cmd.env("COLORTERM", "truecolor");
                cmd.env("HOME", user.home);
                cmd
            };
            cmd.kill_on_drop(true);
            if let Some(working_dir) = options.working_dir {
                debug!("setting working dir: {:?}", working_dir);
                // Convert here — this runs in the process that owns the
                // FS (the agent in a remote session), so its own OS cfg
                // is the right one.
                cmd.current_dir(working_dir.launch_cwd());
            }

            // Shell-integration env first, so caller-specified env can
            // still override it.
            if let Some(si) = &inner.shell_integration {
                cmd.envs(si.spawn_env(Some(handle)));
            }
            if let Some(env) = options.env {
                cmd.envs(env);
            }
            debug!("spawning child process");
            let child = cmd.spawn(&pty_slave);
            match &child {
                Ok(_) => info!("child process spawned successfully for handle {:?}", handle),
                Err(e) => error!("failed to spawn child process: {}", e),
            }
            let child = child?;
            let (read, write) = pty_master.into_split();
            inner.terminals.lock().insert(
                handle,
                Arc::new(LocalTerminal {
                    pty_read: tokio::sync::Mutex::new(read),
                    pty_write: tokio::sync::Mutex::new(write),
                    child: tokio::sync::Mutex::new(child),
                }),
            );
            info!("terminal {:?} created successfully", handle);
            Ok(handle)
        })
        .await;

        match &ret {
            Ok(Ok(handle)) => debug!("terminal::create returning handle {:?}", handle),
            Ok(Err(e)) => error!("terminal::create failed: {}", e),
            Err(e) => error!("terminal::create task panicked: {}", e),
        }
        ret?
    }

    async fn kill(&self, handle: TerminalHandle) -> Result<(), Error> {
        info!("terminal::kill {:?}", handle);
        self.0
            .terminals
            .lock()
            .remove(&handle)
            .ok_or_else(|| Error::custom("terminal not found"))?;

        Ok(())
    }

    async fn resize(&self, handle: TerminalHandle, rows: u16, cols: u16) -> Result<(), Error> {
        debug!("terminal::resize {:?} rows={} cols={}", handle, rows, cols);
        let terminal = self
            .0
            .terminals
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Error::custom("terminal not found"))?;

        terminal
            .pty_write
            .lock()
            .await
            .resize(pty_process::Size::new(rows, cols))?;

        Ok(())
    }

    async fn input(&self, handle: TerminalHandle, data: Vec<u8>) -> Result<(), Error> {
        let terminal = self
            .0
            .terminals
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Error::custom("terminal not found"))?;

        terminal.pty_write.lock().await.write_all(&data).await?;

        Ok(())
    }

    async fn read(&self, handle: TerminalHandle) -> Result<Option<Vec<u8>>, Error> {
        let terminal = self
            .0
            .terminals
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Error::custom("terminal not found"))?;

        let mut buf = [0u8; PTY_READ_BUFFER_SIZE];
        let len = terminal.pty_read.lock().await.read(&mut buf).await?;

        if len > 0 {
            Ok(Some(buf[..len].to_vec()))
        } else {
            Ok(None)
        }
    }

    async fn wait(&self, handle: TerminalHandle) -> Result<ExitStatus, Error> {
        let terminal = self
            .0
            .terminals
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Error::custom("terminal not found"))?;

        let status = terminal.child.lock().await.wait().await?;

        Ok(ExitStatus {
            code: status.code(),
            signal: status.signal(),
        })
    }
}

/// What the client side of a remote terminal sends down its input stream.
#[derive(serde::Serialize, serde::Deserialize)]
enum TerminalInput {
    Data(serde_bytes::ByteBuf),
    Resize { rows: u16, cols: u16 },
}

/// Serve a terminal created for the other side of the connection: the
/// PTY's output into `output`, and `input`'s keystrokes and resizes into
/// the PTY. Each half ends with its stream.
pub(crate) fn serve(
    terminal: Arc<dyn TerminalClient>,
    handle: TerminalHandle,
    mut output: StreamSender,
    mut input: StreamReceiver,
) {
    let pty = terminal.clone();
    tokio::spawn(async move {
        loop {
            let read = tokio::select! {
                biased;
                _ = output.cancelled() => return,
                read = pty.read(handle) => read,
            };
            match read {
                Ok(Some(data)) => {
                    if output.send(data.into()).await.is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = output.finish().await;
                    return;
                }
                Err(e) => return output.reset(e),
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(input)) = input.recv_item().await {
            let result = match input {
                TerminalInput::Data(data) => terminal.input(handle, data.into_vec()).await,
                TerminalInput::Resize { rows, cols } => terminal.resize(handle, rows, cols).await,
            };
            if let Err(e) = result {
                log::error!("terminal input failed: {}", e);
            }
        }
    });
}

/// A terminal on the other side of the connection: its output and input
/// each ride a stream opened when it's created.
pub struct Remote {
    communicator: Communicator,
    terminals: parking_lot::Mutex<HashMap<TerminalHandle, Arc<RemoteTerminal>>>,
}

struct RemoteTerminal {
    output: tokio::sync::Mutex<StreamReceiver>,
    input: tokio::sync::Mutex<StreamSender>,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self {
            communicator,
            terminals: Default::default(),
        }
    }

    fn get(&self, handle: TerminalHandle) -> Result<Arc<RemoteTerminal>, Error> {
        self.terminals
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Error::custom("terminal not found"))
    }

    async fn send_input(&self, handle: TerminalHandle, input: TerminalInput) -> Result<(), Error> {
        let terminal = self.get(handle)?;
        let mut sender = terminal.input.lock().await;
        sender.send_item(&input).await
    }
}

#[async_trait::async_trait]
impl TerminalClient for Remote {
    async fn create(&self, options: TerminalOptions) -> Result<TerminalHandle, Error> {
        let (output_id, output) = self.communicator.open_stream();
        let ret: Result<(TerminalHandle, StreamId), Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_CREATE, &(options, output_id))
            .await?;
        let (handle, input_id) = ret?;
        let terminal = RemoteTerminal {
            output: tokio::sync::Mutex::new(output),
            input: tokio::sync::Mutex::new(self.communicator.stream_sender(input_id)),
        };
        self.terminals.lock().insert(handle, Arc::new(terminal));

        Ok(handle)
    }
    async fn kill(&self, handle: TerminalHandle) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_KILL, &handle)
            .await?;
        self.terminals.lock().remove(&handle);

        Ok(ret?)
    }

    async fn resize(&self, handle: TerminalHandle, rows: u16, cols: u16) -> Result<(), Error> {
        self.send_input(handle, TerminalInput::Resize { rows, cols })
            .await
    }
    async fn input(&self, handle: TerminalHandle, data: Vec<u8>) -> Result<(), Error> {
        self.send_input(handle, TerminalInput::Data(data.into()))
            .await
    }
    async fn read(&self, handle: TerminalHandle) -> Result<Option<Vec<u8>>, Error> {
        let terminal = self.get(handle)?;
        let mut output = terminal.output.lock().await;
        output
            .recv()
            .await
            .transpose()
            .map(|data| data.map(Vec::from))
    }
    async fn wait(&self, handle: TerminalHandle) -> Result<ExitStatus, Error> {
        let ret: Result<ExitStatus, Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_WAIT, &handle)
            .await?;

        Ok(ret?)
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct Passwd<'a> {
    name: &'a str,
    dir: &'a str,
    shell: &'a str,
}

/// Return a Passwd struct with pointers into the provided buf.
///
/// # Unsafety
///
/// If `buf` is changed while `Passwd` is alive, bad thing will almost certainly happen.
#[cfg(unix)]
fn get_pw_entry(buf: &mut [i8; PASSWD_BUFFER_SIZE]) -> Result<Passwd<'_>, Error> {
    let mut entry: MaybeUninit<libc::passwd> = MaybeUninit::uninit();
    let mut res: *mut libc::passwd = std::ptr::null_mut();

    let uid = unsafe { libc::getuid() };
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            &mut res,
        )
    };
    let entry = unsafe { entry.assume_init() };

    if status < 0 {
        return Err(Error::custom("getpwuid_r failed"));
    }

    if res.is_null() {
        return Err(Error::custom("pw not found"));
    }

    assert_eq!(entry.pw_uid, uid);

    Ok(Passwd {
        name: unsafe { CStr::from_ptr(entry.pw_name).to_str().unwrap() },
        dir: unsafe { CStr::from_ptr(entry.pw_dir).to_str().unwrap() },
        shell: unsafe { CStr::from_ptr(entry.pw_shell).to_str().unwrap() },
    })
}

/// `-l`, when this platform's default shell needs to be a login shell.
///
/// Only macOS. A GUI process there is launched by launchd with a bare `PATH`;
/// everything `path_helper` contributes (`/etc/paths.d`, the cryptex dirs,
/// `/Library/Apple/usr/bin`) arrives via `/etc/profile`, which a non-login
/// shell never reads. Terminal.app, iTerm2 and Ghostty all spawn login shells
/// for exactly this reason, as does VS Code — whose macOS terminal profiles
/// carry `-l` while its Linux ones deliberately don't.
///
/// Everywhere else the ambient environment is already right, so a login shell
/// would only re-source profiles for no gain: a Linux desktop session gets its
/// environment from PAM/systemd, and an agent gets one from its login-shell
/// bootstrap (see `connect::spawn_bootstrap`).
///
/// Returns `None` for shells that have no login flag rather than failing the
/// spawn. A macOS *agent* double-sources its profile — bootstrap plus this —
/// which costs some duplicate `PATH` entries and nothing else.
#[cfg(unix)]
fn login_shell_flag(shell: &str) -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let name = std::path::Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(shell);
    // The set VS Code gives `-l` to. Others (tmux, pwsh, nu) either reject it
    // or aren't login shells in the first place.
    matches!(name, "bash" | "zsh" | "fish" | "sh").then_some("-l")
}

/// User information that is required for a new shell session.
#[cfg(unix)]
struct ShellUser {
    user: String,
    home: String,
    shell: String,
}

#[cfg(unix)]
impl ShellUser {
    /// look for shell, username, longname, and home dir in the respective environment variables
    /// before falling back on looking in to `passwd`.
    fn from_env() -> Result<Self, Error> {
        let mut buf = [0; PASSWD_BUFFER_SIZE];
        let pw = get_pw_entry(&mut buf);

        let user = match std::env::var("USER") {
            Ok(user) => user,
            Err(_) => pw.as_ref().map_err(Error::clone)?.name.to_owned(),
        };

        let home = match std::env::var("HOME") {
            Ok(home) => home,
            Err(_) => pw.as_ref().map_err(Error::clone)?.dir.to_owned(),
        };

        let shell = match std::env::var("SHELL") {
            Ok(shell) => shell,
            Err(_) => pw.as_ref().map_err(Error::clone)?.shell.to_owned(),
        };

        Ok(Self { user, home, shell })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::login_shell_flag;

    #[test]
    fn login_flag_only_on_macos() {
        let flag = login_shell_flag("/bin/bash");
        if cfg!(target_os = "macos") {
            assert_eq!(flag, Some("-l"));
        } else {
            // A Linux desktop session and an agent both already have a login
            // environment; re-sourcing profiles would be pure duplication.
            assert_eq!(flag, None);
        }
    }

    #[test]
    fn resolves_by_basename_not_full_path() {
        assert_eq!(
            login_shell_flag("/opt/homebrew/bin/zsh"),
            login_shell_flag("zsh")
        );
    }

    #[test]
    fn shells_without_a_login_flag_are_left_alone() {
        // Passing `-l` to these fails the spawn outright.
        assert_eq!(login_shell_flag("/usr/bin/tmux"), None);
        assert_eq!(login_shell_flag("pwsh"), None);
    }
}
