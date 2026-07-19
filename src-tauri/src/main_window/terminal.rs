use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use log::info;

use newt_common::terminal::TerminalClient;
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

pub struct Terminal {
    pub handle: TerminalHandle,
    defunct: AtomicBool,
    #[allow(dead_code)]
    terminal_client: Arc<dyn TerminalClient>,
}

impl Terminal {
    pub fn is_defunct(&self) -> bool {
        self.defunct.load(Ordering::Relaxed)
    }
}

impl serde::Serialize for Terminal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Terminal", 2)?;
        s.serialize_field("handle", &self.handle)?;
        s.serialize_field("defunct", &self.is_defunct())?;
        s.end()
    }
}

impl Terminal {
    /// Create a Terminal wrapper from an already-created handle. Does not
    /// start pumping output — the caller must insert the terminal into
    /// session state and then call [`Terminal::spawn_reader`]. Inserting
    /// first guarantees the reader's exit cleanup always finds the entry
    /// (a fast-exiting process could otherwise race the insert, leaving a
    /// zombie terminal behind).
    pub fn from_handle(context: &MainWindowContext, handle: TerminalHandle) -> Self {
        let terminal_client = context.terminal_client().expect("terminal_client required");
        Self {
            handle,
            terminal_client,
            defunct: AtomicBool::new(false),
        }
    }

    /// Start the output-pump / exit-watcher task. See [`Terminal::from_handle`]
    /// for the required ordering.
    pub fn spawn_reader(&self, context: MainWindowContext, window: WebviewWindow) {
        Self::spawn_reader_task(context, window, self.handle, self.terminal_client.clone());
    }

    fn spawn_reader_task(
        context: MainWindowContext,
        window: WebviewWindow,
        handle: TerminalHandle,
        terminal_client: Arc<dyn TerminalClient>,
    ) {
        tauri::async_runtime::spawn({
            let terminal_client = terminal_client.clone();
            async move {
                let window_clone = window.clone();
                let reader = tauri::async_runtime::spawn({
                    let terminal_client = terminal_client.clone();
                    async move {
                        while let Some(data) = terminal_client.read(handle).await? {
                            // `emit` goes to *every* window; terminal handles
                            // are per-session and both start at 0, so a
                            // broadcast lands in the other window's terminal 0.
                            window_clone.emit_to(
                                window_clone.label(),
                                "terminal_data",
                                TerminalData { handle, data },
                            )?;
                        }

                        Ok::<_, Error>(())
                    }
                });

                let exited = terminal_client.wait(handle);
                let exit_status = tokio::select! {
                    status = exited => Some(status),
                    _ = reader => None,
                };

                terminal_client.kill(handle).await?;
                info!("Terminal exited.");

                let keep_open = context.preferences().load().behavior.keep_terminal_open;

                if keep_open {
                    // Print exit message to the terminal
                    let msg = match exit_status {
                        Some(Ok(ref s)) if s.signal.is_some() => {
                            format!(
                                "\r\n\x1b[90m[Process killed by signal {}. Press Enter to close.]\x1b[0m",
                                s.signal.unwrap()
                            )
                        }
                        Some(Ok(ref s)) => {
                            let code = s.code.unwrap_or(0);
                            format!(
                                "\r\n\x1b[90m[Process exited with code {code}. Press Enter to close.]\x1b[0m"
                            )
                        }
                        _ => {
                            "\r\n\x1b[90m[Process exited. Press Enter to close.]\x1b[0m".to_string()
                        }
                    };
                    let _ = window.emit_to(
                        window.label(),
                        "terminal_data",
                        TerminalData {
                            handle,
                            data: msg.into_bytes(),
                        },
                    );
                }

                context.with_update(|c| {
                    if keep_open {
                        if let Some(term) = c.terminals.get(handle) {
                            term.defunct.store(true, Ordering::Relaxed);
                        }
                    } else {
                        c.terminals.remove(handle);
                        let mut opts = c.display_options.0.write();
                        if opts.active_terminal == Some(handle) {
                            opts.active_terminal = c.terminals.first_handle();
                        }
                        if c.terminals.is_empty() {
                            opts.terminal_panel_visible = false;
                            opts.panes_focused = true;
                        }
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
