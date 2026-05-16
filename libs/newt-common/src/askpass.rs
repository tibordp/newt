use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// Prompt type as reported by SSH_ASKPASS_PROMPT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptType {
    /// Password / passphrase (SSH_ASKPASS_PROMPT="" or unset)
    Secret,
    /// Host-key confirmation (SSH_ASKPASS_PROMPT="confirm")
    Confirm,
    /// Informational (SSH_ASKPASS_PROMPT="none")
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskpassRequest {
    pub prompt_type: PromptType,
    pub prompt: String,
}

/// `None` means the user cancelled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskpassResponse(pub Option<String>);

/// Whether the prompt should be rendered with secret (masked) input.
///
/// OpenSSH leaves `SSH_ASKPASS_PROMPT` unset for host-key confirmations, so
/// the type defaults to `Secret`; we fall back to a prompt-text heuristic for
/// the "(yes/no/[fingerprint])" case.
pub fn is_secret_prompt(req: &AskpassRequest) -> bool {
    match req.prompt_type {
        PromptType::Confirm | PromptType::Info => false,
        PromptType::Secret => !req.prompt.contains("(yes/no/[fingerprint])"),
    }
}

/// Write a length-prefixed bincode message to a sync writer.
pub fn write_msg(w: &mut impl Write, msg: &impl Serialize) -> std::io::Result<()> {
    let data = bincode::serialize(msg).map_err(std::io::Error::other)?;
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(&data)?;
    w.flush()
}

/// Read a length-prefixed bincode message from a sync reader.
pub fn read_msg<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> std::io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    bincode::deserialize(&buf).map_err(std::io::Error::other)
}

/// Async versions for the tokio side.
pub mod tokio {
    use super::*;
    use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub async fn write_msg(
        w: &mut (impl ::tokio::io::AsyncWrite + Unpin),
        msg: &impl Serialize,
    ) -> std::io::Result<()> {
        let data = bincode::serialize(msg).map_err(std::io::Error::other)?;
        w.write_all(&(data.len() as u32).to_be_bytes()).await?;
        w.write_all(&data).await?;
        w.flush().await
    }

    pub async fn read_msg<T: for<'de> Deserialize<'de>>(
        r: &mut (impl ::tokio::io::AsyncRead + Unpin),
    ) -> std::io::Result<T> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await?;
        bincode::deserialize(&buf).map_err(std::io::Error::other)
    }
}

/// Provides askpass prompts. Symmetric across host and agent: each side has
/// a concrete implementation (Tauri-backed UI in the host, a `Remote` proxy
/// over RPC in the agent), and both the askpass listener and the
/// `API_HOST_ASKPASS` dispatcher consume the same trait object.
#[async_trait::async_trait]
pub trait AskpassProvider: Send + Sync {
    async fn prompt(&self, req: AskpassRequest) -> AskpassResponse;
}

/// `AskpassProvider` implementation that proxies prompts back to the host
/// via the `API_HOST_ASKPASS` reverse RPC. Used by the agent.
pub struct Remote {
    communicator: std::sync::Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
}

impl Remote {
    pub fn new(
        communicator: std::sync::Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
    ) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl AskpassProvider for Remote {
    async fn prompt(&self, req: AskpassRequest) -> AskpassResponse {
        let Some(comm) = self.communicator.get().cloned() else {
            log::warn!("askpass: host communicator not available");
            return AskpassResponse(None);
        };
        match comm
            .invoke::<AskpassRequest, AskpassResponse>(crate::api::API_HOST_ASKPASS, &req)
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                log::warn!("askpass: host RPC failed: {}", e);
                AskpassResponse(None)
            }
        }
    }
}

/// Per-connection askpass handler shared by the unix (UDS) and windows
/// (named pipe) listeners: read one length-prefixed `AskpassRequest`,
/// forward it to `provider.prompt()`, write the `AskpassResponse` back.
#[cfg(any(unix, windows))]
async fn serve_askpass_conn<S>(stream: S, provider: std::sync::Arc<dyn AskpassProvider>)
where
    S: ::tokio::io::AsyncRead + ::tokio::io::AsyncWrite + Send + 'static,
{
    let (mut reader, mut writer) = ::tokio::io::split(stream);
    let request: AskpassRequest = match self::tokio::read_msg(&mut reader).await {
        Ok(r) => r,
        Err(_) => return,
    };
    let response = provider.prompt(request).await;
    let _ = self::tokio::write_msg(&mut writer, &response).await;
}

/// Askpass listener that forwards requests from the `newt-agent` helper
/// binary (selected via `SSH_ASKPASS=<agent-binary>` +
/// `NEWT_ASKPASS_SOCK=<endpoint>`) to an `AskpassProvider`.
///
/// Transport is platform-native: a Unix-domain socket on unix, a Windows
/// named pipe on windows. `socket_path` carries the endpoint either way
/// (a socket path, or a `\\.\pipe\…` name) and is passed through verbatim
/// as `NEWT_ASKPASS_SOCK`; the agent's askpass mode dials it back.
#[cfg(unix)]
pub mod listener {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tokio::sync::oneshot;

    use super::AskpassProvider;

    static SOCKET_NONCE: AtomicU64 = AtomicU64::new(0);

    /// Handle for a running askpass listener. Drop to close the listener
    /// and remove the socket file.
    pub struct AskpassListener {
        pub socket_path: PathBuf,
        shutdown_tx: Option<oneshot::Sender<()>>,
    }

    impl Drop for AskpassListener {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
        }
    }

    /// Spawn a Unix domain socket askpass listener that forwards each
    /// `AskpassRequest` to `provider.prompt()` and writes the response back.
    pub fn spawn(provider: Arc<dyn AskpassProvider>) -> std::io::Result<AskpassListener> {
        let nonce = SOCKET_NONCE.fetch_add(1, Ordering::Relaxed);
        let sock_path = std::env::temp_dir().join(format!(
            "newt-askpass-{}-{}.sock",
            std::process::id(),
            nonce
        ));

        let _ = std::fs::remove_file(&sock_path);

        let listener = std::os::unix::net::UnixListener::bind(&sock_path)?;
        listener.set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(listener)?;

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let cleanup_path = sock_path.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let (stream, _) = match accept {
                            Ok(conn) => conn,
                            Err(_) => break,
                        };
                        tokio::spawn(super::serve_askpass_conn(stream, provider.clone()));
                    }
                }
            }

            let _ = std::fs::remove_file(&cleanup_path);
        });

        Ok(AskpassListener {
            socket_path: sock_path,
            shutdown_tx: Some(shutdown_tx),
        })
    }
}

#[cfg(windows)]
pub mod listener {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tokio::net::windows::named_pipe::ServerOptions;
    use tokio::sync::oneshot;

    use super::AskpassProvider;

    static PIPE_NONCE: AtomicU64 = AtomicU64::new(0);

    /// Handle for a running askpass listener. Drop to stop accepting; the
    /// pipe name is reclaimed by the OS once all instances close.
    pub struct AskpassListener {
        pub socket_path: PathBuf,
        shutdown_tx: Option<oneshot::Sender<()>>,
    }

    impl Drop for AskpassListener {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
        }
    }

    /// Spawn a named-pipe askpass listener. Mirrors the unix UDS listener:
    /// each accepted connection is handed one `AskpassRequest` and replies
    /// with the `AskpassResponse`.
    pub fn spawn(provider: Arc<dyn AskpassProvider>) -> std::io::Result<AskpassListener> {
        let nonce = PIPE_NONCE.fetch_add(1, Ordering::Relaxed);
        let pipe_name = format!(r"\\.\pipe\newt-askpass-{}-{}", std::process::id(), nonce);

        // Create the first instance synchronously so the name exists before
        // `spawn` returns — the caller sets it as NEWT_ASKPASS_SOCK and
        // launches the child immediately, which would otherwise race the
        // listener task and hit ERROR_FILE_NOT_FOUND.
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)?;

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let pipe_name_loop = pipe_name.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    res = server.connect() => {
                        if res.is_err() {
                            break;
                        }
                        // The connected instance becomes this client's
                        // stream; stand up the next instance before serving
                        // so a concurrent client isn't refused.
                        let connected = server;
                        server = match ServerOptions::new().create(&pipe_name_loop) {
                            Ok(s) => s,
                            Err(_) => break,
                        };
                        tokio::spawn(super::serve_askpass_conn(connected, provider.clone()));
                    }
                }
            }
        });

        Ok(AskpassListener {
            socket_path: PathBuf::from(pipe_name),
            shutdown_tx: Some(shutdown_tx),
        })
    }
}
