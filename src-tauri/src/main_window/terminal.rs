use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::path::Path;

use tauri::Window;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

use crate::common::Error;

use super::TerminalHandle;

pub enum Command {
    Resize(u16, u16),
    Input(Vec<u8>),
}

#[derive(serde::Serialize, Clone)]
pub struct TerminalData {
    pub handle: TerminalHandle,
    pub data: Vec<u8>,
}

#[derive(serde::Serialize, Clone)]
pub struct Terminal {
    pub handle: TerminalHandle,
    pub defunct: bool,
    #[serde(skip)]
    sender: UnboundedSender<Command>,
}

impl Terminal {
    pub async fn create(
        window: Window,
        handle: TerminalHandle,
        working_dir: Option<&Path>,
    ) -> Result<Self, Error> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let pty_master = pty_process::Pty::new()?;
        let pty_slave = pty_master.pts()?;

        let user = ShellUser::from_env()?;
        let mut cmd = pty_process::Command::new(&user.shell);
        cmd.env("USER", user.user);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("HOME", user.home);

        if let Some(working_dir) = working_dir {
            cmd.current_dir(working_dir);
        }
        let child = cmd.spawn(&pty_slave)?;

        tokio::spawn(async move {
            match run_terminal(pty_master, child, window, handle, receiver).await {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("terminal error: {}", e);
                }
            }
        });

        Ok(Self { handle, defunct: false, sender })
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), Error> {
        match self.sender.send(Command::Resize(rows, cols)) {
            Ok(()) => Ok(()),
            Err(_e) => Err(Error::Custom("terminal error".to_string())),
        }
    }

    pub fn input(&self, data: Vec<u8>) -> Result<(), Error> {
        match self.sender.send(Command::Input(data)) {
            Ok(()) => Ok(()),
            Err(_e) => Err(Error::Custom("terminal error".to_string())),
        }
    }
}

async fn run_terminal(
    mut pty: pty_process::Pty,
    mut child: tokio::process::Child,
    window: Window,
    handle: TerminalHandle,
    mut mailbox: UnboundedReceiver<Command>,
) -> Result<(), Error> {
    let mut buf = [0u8; 1024];

    loop {
        tokio::select! {
            Ok(_status) = child.wait() => {
                break;
            }
            maybe_len = pty.read(&mut buf) => {
                let Ok(len) = maybe_len else {
                    continue;
                };
                if len == 0 {
                    continue;
                }

                window.emit("terminal_data", TerminalData {
                    handle: handle,
                    data: buf[..len].to_vec(),
                })?;
            }
            Some(cmd) = mailbox.recv() => {
                match cmd {
                    Command::Resize(rows, cols) => {
                        pty.resize(pty_process::Size::new(rows, cols))?;
                    }
                    Command::Input(data) => {
                        pty.write_all(&data).await?;
                    }
                }
            }
        }
    }
    eprintln!("terminal exited");
    Ok(())
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
