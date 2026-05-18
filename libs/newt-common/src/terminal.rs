#[cfg(unix)]
use std::{
    collections::HashMap,
    ffi::CStr,
    mem::MaybeUninit,
    os::unix::process::ExitStatusExt,
    sync::{Arc, atomic::AtomicU32},
};

#[cfg(unix)]
use parking_lot::Mutex;
#[cfg(unix)]
use pty_process::{OwnedReadPty, OwnedWritePty};
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(windows)]
use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicU32},
};

#[cfg(windows)]
use parking_lot::Mutex;

#[cfg(any(unix, windows))]
use log::{debug, error, info};

use crate::{Error, rpc::Communicator};

/// Bytes read in one PTY pull. Matches a typical terminal flush; bigger
/// reads block emitting until they fill, smaller reads waste syscalls.
#[cfg(any(unix, windows))]
const PTY_READ_BUFFER_SIZE: usize = 1024;

/// Buffer size for `getpwuid_r`. POSIX gives no upper bound; 1 KiB is the
/// long-standing convention and is enough for every reasonable passwd entry.
#[cfg(unix)]
const PASSWD_BUFFER_SIZE: usize = 1024;

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
}

#[cfg(unix)]
impl LocalInner {
    fn new() -> Self {
        Self {
            handle: AtomicU32::new(0),
            terminals: Mutex::new(HashMap::new()),
        }
    }
}

#[cfg(unix)]
pub struct Local(Arc<LocalInner>);

#[cfg(windows)]
struct WinInner {
    handle: AtomicU32,
    terminals: Mutex<HashMap<TerminalHandle, Arc<crate::conpty::Conpty>>>,
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
        Self(Arc::new(LocalInner::new()))
    }
}

#[cfg(windows)]
impl Local {
    pub fn new() -> Self {
        Self(Arc::new(WinInner {
            handle: AtomicU32::new(0),
            terminals: Mutex::new(HashMap::new()),
        }))
    }

    fn get(&self, handle: TerminalHandle) -> Result<Arc<crate::conpty::Conpty>, Error> {
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
        let handle = TerminalHandle(
            self.0
                .handle
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
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
        let cwd = options
            .working_dir
            .as_ref()
            .map(|p| crate::vfs::local::launch_cwd(p));

        // ConPTY needs an initial size; the frontend issues a real resize
        // as soon as the xterm mounts, so the default is transient.
        let conpty = crate::conpty::Conpty::spawn(
            &program,
            &args,
            options.env.as_deref(),
            cwd.as_deref(),
            24,
            80,
        )
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
        let inner = self.0.clone();
        let ret = tokio::task::spawn_blocking(move || {
            let handle = TerminalHandle(
                inner
                    .handle
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
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
                info!("spawning default shell: {}", user.shell);
                let mut cmd = pty_process::Command::new(&user.shell);
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
                cmd.current_dir(crate::vfs::local::launch_cwd(&working_dir));
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

pub struct Remote {
    communicator: Communicator,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl TerminalClient for Remote {
    async fn create(&self, options: TerminalOptions) -> Result<TerminalHandle, Error> {
        let ret: Result<TerminalHandle, Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_CREATE, &options)
            .await?;

        Ok(ret?)
    }
    async fn kill(&self, handle: TerminalHandle) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_KILL, &handle)
            .await?;

        Ok(ret?)
    }

    async fn resize(&self, handle: TerminalHandle, rows: u16, cols: u16) -> Result<(), Error> {
        // Fire-and-forget on the high-priority outbox lane, same as `input`.
        // Resize ordering relative to keystrokes is preserved because both go
        // through the FIFO high-priority queue.
        self.communicator
            .signal(crate::api::API_TERMINAL_RESIZE, &(handle, rows, cols))
    }
    async fn input(&self, handle: TerminalHandle, data: Vec<u8>) -> Result<(), Error> {
        // Fire-and-forget on the high-priority outbox lane: keystrokes must
        // not be queued behind bulk `notify` streams (e.g. VFS write chunks),
        // and the frontend has no use for an ack — loss is observed via the
        // absence of echoed output.
        self.communicator
            .signal(crate::api::API_TERMINAL_INPUT, &(handle, data))
    }
    async fn read(&self, handle: TerminalHandle) -> Result<Option<Vec<u8>>, Error> {
        let ret: Result<Option<Vec<u8>>, Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_READ, &handle)
            .await?;

        Ok(ret?)
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
    // Create zeroed passwd struct.
    let mut entry: MaybeUninit<libc::passwd> = MaybeUninit::uninit();

    let mut res: *mut libc::passwd = std::ptr::null_mut();

    // Try and read the pw file.
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

    // Sanity check.
    assert_eq!(entry.pw_uid, uid);

    // Build a borrowed Passwd struct.
    Ok(Passwd {
        name: unsafe { CStr::from_ptr(entry.pw_name).to_str().unwrap() },
        dir: unsafe { CStr::from_ptr(entry.pw_dir).to_str().unwrap() },
        shell: unsafe { CStr::from_ptr(entry.pw_shell).to_str().unwrap() },
    })
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
            Err(_) => match pw {
                Ok(ref pw) => pw.name.to_owned(),
                Err(err) => return Err(err),
            },
        };

        let home = match std::env::var("HOME") {
            Ok(home) => home,
            Err(_) => match pw {
                Ok(ref pw) => pw.dir.to_owned(),
                Err(err) => return Err(err),
            },
        };

        let shell = match std::env::var("SHELL") {
            Ok(shell) => shell,
            Err(_) => match pw {
                Ok(ref pw) => pw.shell.to_owned(),
                Err(err) => return Err(err),
            },
        };

        Ok(Self { user, home, shell })
    }
}
