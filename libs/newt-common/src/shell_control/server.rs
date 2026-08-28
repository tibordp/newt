//! ShellIntegration: the per-session control server — temp dir (shim +
//! socket), accept loop, HTTP routing, env injection.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use log::{debug, warn};
use serde::Deserialize;

use crate::terminal::TerminalHandle;

use super::{
    ControlRequest, ControlResponse, ENV_SOCK, ENV_TERMINAL, PaneSelector, SelectMode,
    ShellControlHandler,
};

static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Render `path` for the `newt.cmd` shim, undoing the `\\?\` verbatim
/// prefix. cmd.exe's parser rejects verbatim paths outright, but the agent
/// path arrives with one: Tauri canonicalises its own exe to resolve
/// symlinks (`tauri-utils` `starting_binary.rs`), and `fs::canonicalize` on
/// Windows always hands back the `\\?\` form — so the agent found relative
/// to the resource dir inherits it. Every other consumer is a Win32 API,
/// which takes verbatim paths happily; only the shell needs this. See the
/// same strip in `wsl_launch::to_wsl_path`.
///
/// Verbatim UNC needs unwrapping rather than merely trimming
/// (`\\?\UNC\server\share` → `\\server\share`): dropping four
/// characters would leave `UNC\server\share`, a *relative* path. cmd.exe
/// can execute a program by UNC path even though it cannot cd to one.
///
/// The body is platform-independent so it stays testable on the machines
/// this is usually written on; only the *compilation* is gated, since the
/// shim itself is Windows-only.
#[cfg(any(windows, test))]
fn shim_command_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    match raw.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => rest.to_string(),
        },
        None => raw.into_owned(),
    }
}

pub struct ShellIntegration {
    dir: std::path::PathBuf,
    /// Value for NEWT_SHELL_SOCK: socket path (Unix) or pipe name (Windows).
    sock: String,
    server: tokio::task::JoinHandle<()>,
}

impl ShellIntegration {
    /// Create the per-session directory (shim + socket), start the HTTP
    /// control server, and return the handle used for env injection.
    /// `cli_binary` is the binary the `newt` shim points at (the agent).
    /// Must run within a tokio runtime (the accept loop is spawned).
    pub fn start(
        cli_binary: &std::path::Path,
        handler: Arc<dyn ShellControlHandler>,
    ) -> Result<Arc<Self>, std::io::Error> {
        let tag = format!(
            "newt-shell-{}-{}",
            std::process::id(),
            INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(&tag);
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            std::os::unix::fs::symlink(cli_binary, dir.join("newt"))?;
        }
        #[cfg(windows)]
        {
            // argv[0] through a .cmd shim is the exe path, not `newt`, so the
            // shim marks CLI mode via NEWT_CLI instead.
            let shim = format!(
                "@echo off\r\nset \"NEWT_CLI=1\"\r\n\"{}\" %*\r\n",
                shim_command_path(cli_binary)
            );
            std::fs::write(dir.join("newt.cmd"), shim)?;
        }

        #[cfg(unix)]
        let (sock, server) = {
            let path = dir.join("newt.sock");
            let listener = tokio::net::UnixListener::bind(&path)?;
            let server = tokio::spawn(accept_loop_unix(listener, handler));
            (path.to_string_lossy().into_owned(), server)
        };
        #[cfg(windows)]
        let (sock, server) = {
            let name = format!(r"\\.\pipe\{}", tag);
            let server = tokio::spawn(accept_loop_pipe(name.clone(), handler));
            (name, server)
        };

        Ok(Arc::new(Self { dir, sock, server }))
    }

    /// The NEWT_SHELL_SOCK value (socket path / pipe name).
    pub fn sock_addr(&self) -> &str {
        &self.sock
    }

    /// Env overlay for a spawned terminal / command, including the PATH
    /// prepend. Computed per spawn so a changed parent PATH is picked up.
    pub fn spawn_env(&self, terminal: Option<TerminalHandle>) -> Vec<(String, String)> {
        let mut env = vec![(ENV_SOCK.to_string(), self.sock.clone())];
        if let Some(handle) = terminal {
            env.push((ENV_TERMINAL.to_string(), handle.0.to_string()));
        }
        let path = match std::env::var_os("PATH") {
            Some(existing) => {
                let mut parts = vec![self.dir.clone()];
                parts.extend(std::env::split_paths(&existing));
                std::env::join_paths(parts)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| self.dir.to_string_lossy().into_owned())
            }
            None => self.dir.to_string_lossy().into_owned(),
        };
        env.push(("PATH".to_string(), path));
        env
    }
}

impl Drop for ShellIntegration {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(unix)]
async fn accept_loop_unix(
    listener: tokio::net::UnixListener,
    handler: Arc<dyn ShellControlHandler>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let handler = handler.clone();
                tokio::spawn(serve_connection(stream, handler));
            }
            Err(e) => {
                warn!("shell control accept failed: {e}");
                break;
            }
        }
    }
}

#[cfg(windows)]
async fn accept_loop_pipe(name: String, handler: Arc<dyn ShellControlHandler>) {
    use tokio::net::windows::named_pipe::ServerOptions;
    let mut first = true;
    loop {
        let server = match ServerOptions::new()
            .first_pipe_instance(first)
            .create(&name)
        {
            Ok(s) => s,
            Err(e) => {
                warn!("shell control pipe create failed: {e}");
                break;
            }
        };
        first = false;
        if let Err(e) = server.connect().await {
            warn!("shell control pipe connect failed: {e}");
            continue;
        }
        let handler = handler.clone();
        tokio::spawn(serve_connection(server, handler));
    }
}

async fn serve_connection<S>(stream: S, handler: Arc<dyn ShellControlHandler>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |req| {
        let handler = handler.clone();
        async move { Ok::<_, std::convert::Infallible>(route(handler, req).await) }
    });
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .await
    {
        debug!("shell control connection ended: {e}");
    }
}

// ---------------------------------------------------------------------------
// HTTP routing
// ---------------------------------------------------------------------------

// Unsync: the cat stream wraps async-trait futures, which are Send but not
// Sync; hyper itself doesn't require Sync bodies.
type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::io::Error>;

pub(super) fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn status_response(status: StatusCode, message: impl Into<Bytes>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(full(message))
        .unwrap()
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
    #[serde(default)]
    cwd: String,
}

#[derive(Deserialize)]
struct SelectBody {
    #[serde(default)]
    patterns: Vec<String>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    cwd: String,
    mode: SelectMode,
}

#[derive(Deserialize)]
struct TransferBody {
    sources: Vec<String>,
    dest: String,
    #[serde(default)]
    cwd: String,
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

async fn read_json_body<T: serde::de::DeserializeOwned>(
    req: Request<hyper::body::Incoming>,
) -> Result<T, String> {
    let bytes = req
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("bad body: {e}"))?
        .to_bytes();
    serde_json::from_slice(&bytes).map_err(|e| format!("bad request body: {e}"))
}

async fn route(
    handler: Arc<dyn ShellControlHandler>,
    req: Request<hyper::body::Incoming>,
) -> Response<Body> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|q| q.to_string());
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let control = match (&method, segments.as_slice()) {
        (&Method::GET, ["v1", "panes", pane, "cwd"]) => match PaneSelector::parse(pane) {
            Some(pane) => ControlRequest::Pwd { pane },
            None => return status_response(StatusCode::NOT_FOUND, "unknown pane"),
        },
        (&Method::POST, ["v1", "panes", pane, verb @ ("cd" | "focus")]) => {
            let _ = verb; // cd and focus share non-strict navigate semantics
            let Some(pane) = PaneSelector::parse(pane) else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            let body: PathBody = match read_json_body(req).await {
                Ok(b) => b,
                Err(msg) => return status_response(StatusCode::BAD_REQUEST, msg),
            };
            ControlRequest::Navigate {
                pane,
                path: body.path,
                cwd: body.cwd,
            }
        }
        (&Method::POST, ["v1", "panes", pane, "select"]) => {
            let Some(pane) = PaneSelector::parse(pane) else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            let body: SelectBody = match read_json_body(req).await {
                Ok(b) => b,
                Err(msg) => return status_response(StatusCode::BAD_REQUEST, msg),
            };
            ControlRequest::Select {
                pane,
                patterns: body.patterns,
                names: body.names,
                cwd: body.cwd,
                mode: body.mode,
            }
        }
        (&Method::GET, ["v1", "commands"]) => ControlRequest::ListCommands,
        (&Method::POST, ["v1", "commands", id]) => {
            let pane = query_param(query.as_deref(), "pane")
                .as_deref()
                .map(PaneSelector::parse)
                .unwrap_or(Some(PaneSelector::Active));
            let Some(pane) = pane else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            ControlRequest::Command {
                pane,
                id: id.to_string(),
            }
        }
        (&Method::GET, ["v1", "panes", pane, "read"]) => {
            let Some(pane) = PaneSelector::parse(pane) else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            let Some(file) = query_param(query.as_deref(), "path") else {
                return status_response(StatusCode::BAD_REQUEST, "missing path");
            };
            let cwd = query_param(query.as_deref(), "cwd").unwrap_or_default();
            // Resolve on the control plane, then stream from the data plane.
            let resolved = handler
                .control(ControlRequest::ResolveFile {
                    pane,
                    path: file,
                    cwd,
                })
                .await;
            let vfs_path = match resolved {
                Ok(ControlResponse::ResolvedFile(p)) => p,
                Ok(_) => {
                    return status_response(StatusCode::INTERNAL_SERVER_ERROR, "bad resolve");
                }
                Err(e) => return status_response(StatusCode::NOT_FOUND, e),
            };
            return match handler.read_file(vfs_path).await {
                Ok(stream) => {
                    let body = StreamBody::new(futures::StreamExt::map(stream, |chunk| {
                        chunk.map(Frame::data).map_err(std::io::Error::other)
                    }));
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "application/octet-stream")
                        .body(BodyExt::boxed_unsync(body))
                        .unwrap()
                }
                Err(e) => status_response(StatusCode::NOT_FOUND, e),
            };
        }
        (&Method::POST, ["v1", verb @ ("view" | "edit")]) => {
            let edit = *verb == "edit";
            let pane = query_param(query.as_deref(), "pane")
                .as_deref()
                .map(PaneSelector::parse)
                .unwrap_or(Some(PaneSelector::Active));
            let Some(pane) = pane else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            let body: PathBody = match read_json_body(req).await {
                Ok(b) => b,
                Err(msg) => return status_response(StatusCode::BAD_REQUEST, msg),
            };
            ControlRequest::Open {
                pane,
                path: body.path,
                cwd: body.cwd,
                edit,
            }
        }
        (&Method::POST, ["v1", "operations", op @ ("copy" | "move")]) => {
            let move_files = *op == "move";
            let pane = query_param(query.as_deref(), "pane")
                .as_deref()
                .map(PaneSelector::parse)
                .unwrap_or(Some(PaneSelector::Active));
            let Some(pane) = pane else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            let body: TransferBody = match read_json_body(req).await {
                Ok(b) => b,
                Err(msg) => return status_response(StatusCode::BAD_REQUEST, msg),
            };
            ControlRequest::Transfer {
                pane,
                move_files,
                sources: body.sources,
                dest: body.dest,
                cwd: body.cwd,
            }
        }
        _ => return status_response(StatusCode::NOT_FOUND, "unknown route"),
    };

    match handler.control(control).await {
        Ok(ControlResponse::Ok) => status_response(StatusCode::OK, ""),
        Ok(ControlResponse::Text(text)) => status_response(StatusCode::OK, text),
        Ok(ControlResponse::Commands(commands)) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(full(serde_json::to_vec(&commands).unwrap_or_default()))
            .unwrap(),
        // ResolvedFile is internal to the read route above.
        Ok(ControlResponse::ResolvedFile(_)) => {
            status_response(StatusCode::INTERNAL_SERVER_ERROR, "unexpected response")
        }
        Err(e) => status_response(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ControlResult, ShellControlHandler};
    use super::*;
    use crate::filesystem::ByteStream;
    use crate::vfs::VfsPath;

    /// cmd.exe rejects `\\?\` outright, and the agent path arrives with one
    /// because Tauri canonicalises its exe. Platform-independent so the
    /// Windows-only shim can still be checked from a Unix host.
    #[test]
    fn shim_path_drops_the_verbatim_prefix() {
        assert_eq!(
            shim_command_path(std::path::Path::new(
                r"\\?\C:\Program Files\Newt\agents\newt-agent.exe"
            )),
            r"C:\Program Files\Newt\agents\newt-agent.exe"
        );
    }

    #[test]
    fn shim_path_unwraps_verbatim_unc_to_a_real_share() {
        // Trimming four characters would leave `UNC\server\...`, which cmd
        // would read as a relative path.
        assert_eq!(
            shim_command_path(std::path::Path::new(
                r"\\?\UNC\build\share\Newt\newt-agent.exe"
            )),
            r"\\build\share\Newt\newt-agent.exe"
        );
    }

    #[test]
    fn shim_path_leaves_ordinary_paths_alone() {
        assert_eq!(
            shim_command_path(std::path::Path::new(r"C:\Newt\newt-agent.exe")),
            r"C:\Newt\newt-agent.exe"
        );
        // A plain UNC path is already something cmd can execute.
        assert_eq!(
            shim_command_path(std::path::Path::new(r"\\build\share\newt-agent.exe")),
            r"\\build\share\newt-agent.exe"
        );
        assert_eq!(
            shim_command_path(std::path::Path::new("/usr/lib/newt/newt-agent")),
            "/usr/lib/newt/newt-agent"
        );
    }

    struct MockHandler;

    #[async_trait::async_trait]
    impl ShellControlHandler for MockHandler {
        async fn control(&self, req: ControlRequest) -> ControlResult {
            match req {
                ControlRequest::Pwd { .. } => Ok(ControlResponse::Text("/mock/dir".into())),
                ControlRequest::Navigate { path, .. } if path == "/boom" => {
                    Err("no such directory".into())
                }
                ControlRequest::Navigate { .. } => Ok(ControlResponse::Ok),
                ControlRequest::Select {
                    patterns,
                    names,
                    mode,
                    ..
                } => Ok(ControlResponse::Text(format!(
                    "{} {} {mode:?}",
                    patterns.len(),
                    names.len()
                ))),
                ControlRequest::ResolveFile { path, .. } => {
                    Ok(ControlResponse::ResolvedFile(VfsPath::new(
                        crate::vfs::VfsId::ROOT,
                        crate::vfs::path::PathBuf::from_wire_string(path),
                    )))
                }
                _ => Err("unhandled".into()),
            }
        }

        async fn read_file(&self, _path: VfsPath) -> Result<ByteStream, String> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(Bytes::from_static(b"hello ")),
                Ok(Bytes::from_static(b"world")),
            ])))
        }
    }

    #[cfg(unix)]
    async fn send(
        si: &ShellIntegration,
        method: Method,
        path: &str,
        body: &str,
    ) -> (StatusCode, Vec<u8>) {
        let stream = tokio::net::UnixStream::connect(si.sock_addr())
            .await
            .unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(conn);
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "newt")
            .body(full(body.to_string().into_bytes()))
            .unwrap();
        let resp = sender.send_request(req).await.unwrap();
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server_end_to_end() {
        let si = ShellIntegration::start(std::path::Path::new("/bin/true"), Arc::new(MockHandler))
            .unwrap();

        // The shim symlink exists in the per-session dir.
        assert!(
            std::fs::symlink_metadata(
                std::path::Path::new(si.sock_addr())
                    .parent()
                    .unwrap()
                    .join("newt")
            )
            .is_ok()
        );

        // pwd
        let (status, body) = send(&si, Method::GET, "/v1/panes/active/cwd", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"/mock/dir");

        // cd ok / cd error
        let (status, _) = send(
            &si,
            Method::POST,
            "/v1/panes/other/cd",
            r#"{"path":"/tmp","cwd":"/"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = send(
            &si,
            Method::POST,
            "/v1/panes/active/cd",
            r#"{"path":"/boom","cwd":"/"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body, b"no such directory");

        // cat streams through resolve + read_file
        let (status, body) = send(
            &si,
            Method::POST,
            "/v1/panes/left/select",
            r#"{"names":["a","b"],"mode":"add"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"0 2 Add");

        let (status, body) = send(&si, Method::GET, "/v1/panes/active/read?path=/f", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"hello world");

        // unknown routes / panes stay graceful
        let (status, _) = send(&si, Method::GET, "/v2/nope", "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = send(&si, Method::GET, "/v1/panes/middle/cwd", "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // env injection: socket + terminal handle + PATH prepend
        let env = si.spawn_env(Some(TerminalHandle(3)));
        let dir = std::path::Path::new(si.sock_addr()).parent().unwrap();
        assert!(env.contains(&(ENV_SOCK.to_string(), si.sock_addr().to_string())));
        assert!(env.contains(&(ENV_TERMINAL.to_string(), "3".to_string())));
        let path_var = env.iter().find(|(k, _)| k == "PATH").unwrap().1.clone();
        assert!(path_var.starts_with(dir.to_str().unwrap()));

        // Drop cleans up the per-session dir.
        let dir = dir.to_path_buf();
        drop(si);
        assert!(!dir.exists());
    }
}
