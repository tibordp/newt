//! ShellIntegration: the per-session control server — temp dir (shim +
//! socket), accept loop, HTTP routing, env injection.

use std::sync::Arc;

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
        // Private (0700 on unix) and randomly named.
        let dir = tempfile::Builder::new()
            .prefix("newt-shell-")
            .tempdir()?
            .keep();
        #[cfg(unix)]
        std::os::unix::fs::symlink(cli_binary, dir.join("newt"))?;
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
            let name = crate::pipe::unique_name("newt-shell");
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
        let server =
            match crate::pipe::create(ServerOptions::new().first_pipe_instance(first), &name) {
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

fn json_response(value: &impl serde::Serialize) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(full(serde_json::to_vec(value).unwrap_or_default()))
        .unwrap()
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

#[derive(Deserialize)]
struct DeleteBody {
    paths: Vec<String>,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    permanent: bool,
}

#[derive(Deserialize)]
struct MkdirBody {
    paths: Vec<String>,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    parents: bool,
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// A boolean query flag: `?wait=1` (or `true`); absent is false.
fn query_flag(query: Option<&str>, key: &str) -> bool {
    query_param(query, key).is_some_and(|v| v == "1" || v == "true")
}

/// The `?pane=` of a route that isn't under `/v1/panes/{pane}`; the active
/// pane when absent, `None` when unknown.
fn query_pane(query: Option<&str>) -> Option<PaneSelector> {
    query_param(query, "pane")
        .as_deref()
        .map(PaneSelector::parse)
        .unwrap_or(Some(PaneSelector::Active))
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
        (&Method::GET, ["v1", "panes", pane, "entries"]) => match PaneSelector::parse(pane) {
            Some(pane) => ControlRequest::List {
                pane,
                selection: query_flag(query.as_deref(), "selection"),
                all: query_flag(query.as_deref(), "all"),
            },
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
            let Some(pane) = query_pane(query.as_deref()) else {
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
            let Some(pane) = query_pane(query.as_deref()) else {
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
                wait: query_flag(query.as_deref(), "wait"),
            }
        }
        (&Method::POST, ["v1", "operations", op @ ("copy" | "move")]) => {
            let move_files = *op == "move";
            let Some(pane) = query_pane(query.as_deref()) else {
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
                wait: query_flag(query.as_deref(), "wait"),
            }
        }
        (&Method::POST, ["v1", "operations", "delete"]) => {
            let Some(pane) = query_pane(query.as_deref()) else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            let body: DeleteBody = match read_json_body(req).await {
                Ok(b) => b,
                Err(msg) => return status_response(StatusCode::BAD_REQUEST, msg),
            };
            ControlRequest::Delete {
                pane,
                paths: body.paths,
                cwd: body.cwd,
                recursive: body.recursive,
                permanent: body.permanent,
                wait: query_flag(query.as_deref(), "wait"),
            }
        }
        (&Method::POST, ["v1", "mkdir"]) => {
            let Some(pane) = query_pane(query.as_deref()) else {
                return status_response(StatusCode::NOT_FOUND, "unknown pane");
            };
            let body: MkdirBody = match read_json_body(req).await {
                Ok(b) => b,
                Err(msg) => return status_response(StatusCode::BAD_REQUEST, msg),
            };
            ControlRequest::MakeDirectory {
                pane,
                paths: body.paths,
                cwd: body.cwd,
                parents: body.parents,
            }
        }
        _ => return status_response(StatusCode::NOT_FOUND, "unknown route"),
    };

    match handler.control(control).await {
        Ok(ControlResponse::Ok) => status_response(StatusCode::OK, ""),
        Ok(ControlResponse::Text(text)) => status_response(StatusCode::OK, text),
        Ok(ControlResponse::Commands(commands)) => json_response(&commands),
        Ok(ControlResponse::Entries(entries)) => json_response(&entries),
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
                ControlRequest::List { selection, .. } => {
                    Ok(ControlResponse::Entries(vec![super::super::ListEntry {
                        name: if selection { "picked" } else { "all" }.into(),
                        path: "/mock/dir/x".into(),
                        kind: super::super::EntryKind::Dir,
                        size: None,
                        modified: Some(1),
                        mode: None,
                        user: None,
                        group: None,
                        target: None,
                    }]))
                }
                ControlRequest::Delete {
                    paths,
                    recursive,
                    permanent,
                    wait,
                    ..
                } => Ok(ControlResponse::Text(format!(
                    "{} {recursive} {permanent} {wait}",
                    paths.join(",")
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

        let (status, body) =
            send(&si, Method::GET, "/v1/panes/active/entries?selection=1", "").await;
        assert_eq!(status, StatusCode::OK);
        let entries: Vec<super::super::ListEntry> = serde_json::from_slice(&body).unwrap();
        assert_eq!(entries[0].name, "picked");
        assert_eq!(entries[0].kind, super::super::EntryKind::Dir);

        // Flags absent from an older CLI's body default to off.
        let (status, body) = send(
            &si,
            Method::POST,
            "/v1/operations/delete?wait=1",
            r#"{"paths":["a","b"],"recursive":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"a,b true false true");

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

    /// Holds a request open, reporting when it arrives and when it is
    /// dropped.
    struct PendingHandler {
        arrived: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        dropped: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    #[async_trait::async_trait]
    impl ShellControlHandler for PendingHandler {
        async fn control(&self, _req: ControlRequest) -> ControlResult {
            let _signal = DropSignal(self.dropped.lock().take());
            if let Some(tx) = self.arrived.lock().take() {
                let _ = tx.send(());
            }
            std::future::pending().await
        }

        async fn read_file(&self, _path: VfsPath) -> Result<ByteStream, String> {
            unreachable!()
        }
    }

    /// `--wait` cancels by dropping: a client that goes away mid-request
    /// must take the handler's future with it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_client_hanging_up_drops_the_request() {
        let (arrived_tx, arrived) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped) = tokio::sync::oneshot::channel();
        let si = ShellIntegration::start(
            std::path::Path::new("/bin/true"),
            Arc::new(PendingHandler {
                arrived: parking_lot::Mutex::new(Some(arrived_tx)),
                dropped: parking_lot::Mutex::new(Some(dropped_tx)),
            }),
        )
        .unwrap();

        let stream = tokio::net::UnixStream::connect(si.sock_addr())
            .await
            .unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        let conn = tokio::spawn(conn);
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/operations/copy?wait=1")
            .header("host", "newt")
            .body(full(r#"{"sources":["a"],"dest":"b"}"#))
            .unwrap();
        let response = tokio::spawn(async move { sender.send_request(req).await });

        arrived.await.unwrap();
        response.abort();
        conn.abort();

        tokio::time::timeout(std::time::Duration::from_secs(10), dropped)
            .await
            .expect("the handler's future outlived its client")
            .unwrap();
    }
}
