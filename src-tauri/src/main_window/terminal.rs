use std::path::Path;
use std::sync::Arc;

use log::info;

use newt_common::terminal::TerminalClient;
use newt_common::terminal::TerminalOptions;
use tauri::Window;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use crate::common::Error;

use super::MainWindowContext;

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

#[derive(serde::Serialize)]
pub struct Terminal {
    pub handle: TerminalHandle,
    pub defunct: bool,
    #[serde(skip)]
    terminal_client: Arc<dyn TerminalClient>,
}

impl Terminal {
    pub async fn create(
        context: MainWindowContext,
        window: Window,
        working_dir: Option<&Path>,
    ) -> Result<Self, Error> {
        let terminal_client = context.terminal_client();
        let handle = terminal_client
            .create(TerminalOptions {
                working_dir: working_dir.map(|p| p.to_path_buf()),
                ..Default::default()
            })
            .await?;

        tauri::async_runtime::spawn({
            let terminal_client = terminal_client.clone();
            async move {
                let reader = tauri::async_runtime::spawn({
                    let terminal_client = terminal_client.clone();
                    async move {
                        while let Some(data) = terminal_client.read(handle).await? {
                            window.emit("terminal_data", TerminalData { handle, data })?;
                        }

                        Ok::<_, Error>(())
                    }
                });

                let exited = terminal_client.wait(handle);
                tokio::select! {
                    _ = exited => {},
                    _ = reader => {},
                }

                terminal_client.kill(handle).await?;
                info!("Terminal exited.");

                context.with_update(|c| {
                    c.terminals.remove(handle);
                    c.display_options.0.write().active_terminal = None;
                    if c.terminals.len() == 0 {
                        c.display_options.0.write().panes_focused = true;
                    }
                    Ok(())
                })?;

                Ok::<_, Error>(())
            }
        });

        Ok(Self {
            handle,
            terminal_client,
            defunct: false,
        })
    }

    pub async fn resize(&self, rows: u16, cols: u16) -> Result<(), Error> {
        self.terminal_client.resize(self.handle, rows, cols).await?;

        Ok(())
    }

    pub async fn input(&self, data: Vec<u8>) -> Result<(), Error> {
        self.terminal_client.input(self.handle, data).await?;

        Ok(())
    }
}
