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
    #[serde(skip)]
    sender: UnboundedSender<Command>,
}


impl Terminal {
    pub async fn create(window: Window, handle: TerminalHandle, rows: u16, cols: u16) -> Result<Self, Error> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let pty = pty_process::Pty::new()?;
        pty.resize(pty_process::Size::new(rows, cols))?;

        let mut cmd = pty_process::Command::new("/bin/bash");
        let child = cmd.spawn(&pty.pts()?)?;

        tokio::spawn(async move {
            match run_terminal(pty, child, window, handle, receiver).await {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("terminal error: {}", e);
                }
            }
        });

        Ok(Self {
            handle,
            sender,
        })
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), Error> {
        match self.sender.send(Command::Resize(rows, cols)) {
            Ok(()) => Ok(()),
            Err(e) => Err(Error::Custom("terminal error".to_string()))
        }
    }

    pub fn input(&self, data: Vec<u8>) -> Result<(), Error> {
        match self.sender.send(Command::Input(data)) {
            Ok(()) => Ok(()),
            Err(e) => Err(Error::Custom("terminal error".to_string()))
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
