use std::path::Path;
use std::sync::Arc;

use log::info;

use newt_common::terminal::TerminalClient;
use newt_common::terminal::TerminalOptions;
use tauri::Emitter;
use tauri::WebviewWindow;

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
    /// Create a Terminal wrapper from an already-created handle.
    /// Spawns the reader/waiter task just like `create`.
    pub fn from_handle(
        context: MainWindowContext,
        window: WebviewWindow,
        handle: TerminalHandle,
    ) -> Self {
        let terminal_client = context.terminal_client().expect("terminal_client required");
        Self::spawn_reader(context, window, handle, terminal_client.clone());
        Self {
            handle,
            terminal_client,
            defunct: false,
        }
    }

    pub async fn create(
        context: MainWindowContext,
        window: WebviewWindow,
        working_dir: Option<&Path>,
    ) -> Result<Self, Error> {
        let terminal_client = context.terminal_client()?;
        let handle = terminal_client
            .create(TerminalOptions {
                working_dir: working_dir.map(|p| p.to_path_buf()),
                ..Default::default()
            })
            .await?;

        Self::spawn_reader(context, window, handle, terminal_client.clone());

        Ok(Self {
            handle,
            terminal_client,
            defunct: false,
        })
    }

    fn spawn_reader(
        context: MainWindowContext,
        window: WebviewWindow,
        handle: TerminalHandle,
        terminal_client: Arc<dyn TerminalClient>,
    ) {
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
                    let mut opts = c.display_options.0.write();
                    if opts.active_terminal == Some(handle) {
                        opts.active_terminal = c.terminals.first_handle();
                    }
                    if c.terminals.is_empty() {
                        opts.terminal_panel_visible = false;
                        opts.panes_focused = true;
                    }
                    Ok(())
                })?;

                Ok::<_, Error>(())
            }
        });
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
