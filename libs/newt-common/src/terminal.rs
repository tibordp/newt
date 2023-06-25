use std::{
    collections::HashMap,
    ffi::CStr,
    mem::MaybeUninit,
    os::unix::process::ExitStatusExt,
    path::PathBuf,
    sync::{atomic::AtomicU32, Arc},
};

use parking_lot::Mutex;
use pty_process::{OwnedReadPty, OwnedWritePty};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{rpc::Communicator, Error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TerminalHandle(pub u32);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct TerminalOptions {
    pub working_dir: Option<PathBuf>,
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

struct LocalTerminal {
    pty_read: tokio::sync::Mutex<OwnedReadPty>,
    pty_write: tokio::sync::Mutex<OwnedWritePty>,
    child: tokio::sync::Mutex<tokio::process::Child>,
}

struct LocalInner {
    handle: AtomicU32,
    terminals: Mutex<HashMap<TerminalHandle, Arc<LocalTerminal>>>,
}

impl LocalInner {
    fn new() -> Self {
        Self {
            handle: AtomicU32::new(0),
            terminals: Mutex::new(HashMap::new()),
        }
    }
}

pub struct Local(Arc<LocalInner>);

impl Local {
    pub fn new() -> Self {
        Self(Arc::new(LocalInner::new()))
    }
}

#[async_trait::async_trait]
impl TerminalClient for Local {
    async fn create(&self, options: TerminalOptions) -> Result<TerminalHandle, Error> {
        let inner = self.0.clone();
        let ret = tokio::task::spawn_blocking(move || {
            let handle = TerminalHandle(
                inner
                    .handle
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            );

            let pty_master = pty_process::Pty::new()?;
            let pty_slave = pty_master.pts()?;

            let mut cmd = if let Some(command) = options.command {
                let mut cmd = pty_process::Command::new(command);
                if let Some(args) = options.args {
                    cmd.args(args);
                }
                cmd
            } else {
                let user = ShellUser::from_env()?;
                let mut cmd = pty_process::Command::new(&user.shell);
                cmd.env("USER", user.user);
                cmd.env("TERM", "xterm-256color");
                cmd.env("COLORTERM", "truecolor");
                cmd.env("HOME", user.home);
                cmd
            };
            cmd.kill_on_drop(true);
            if let Some(working_dir) = options.working_dir {
                cmd.current_dir(working_dir);
            }

            if let Some(env) = options.env {
                cmd.envs(env);
            }
            let child = cmd.spawn(&pty_slave);
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
            Ok(handle)
        })
        .await;

        ret?
    }

    async fn kill(&self, handle: TerminalHandle) -> Result<(), Error> {
        self.0
            .terminals
            .lock()
            .remove(&handle)
            .ok_or_else(|| Error::Custom("terminal not found".to_string()))?;

        Ok(())
    }

    async fn resize(&self, handle: TerminalHandle, rows: u16, cols: u16) -> Result<(), Error> {
        let terminal = self
            .0
            .terminals
            .lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Error::Custom("terminal not found".to_string()))?;

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
            .ok_or_else(|| Error::Custom("terminal not found".to_string()))?;

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
            .ok_or_else(|| Error::Custom("terminal not found".to_string()))?;

        let mut buf = [0u8; 1024];
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
            .ok_or_else(|| Error::Custom("terminal not found".to_string()))?;

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
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_RESIZE, &(handle, rows, cols))
            .await?;

        Ok(ret?)
    }
    async fn input(&self, handle: TerminalHandle, data: Vec<u8>) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_TERMINAL_INPUT, &(handle, data))
            .await?;

        Ok(ret?)
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
fn get_pw_entry(buf: &mut [i8; 1024]) -> Result<Passwd<'_>, Error> {
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
        return Err(Error::Custom("getpwuid_r failed".to_string()));
    }

    if res.is_null() {
        return Err(Error::Custom("pw not found".to_string()));
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
struct ShellUser {
    user: String,
    home: String,
    shell: String,
}

impl ShellUser {
    /// look for shell, username, longname, and home dir in the respective environment variables
    /// before falling back on looking in to `passwd`.
    fn from_env() -> Result<Self, Error> {
        let mut buf = [0; 1024];
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
