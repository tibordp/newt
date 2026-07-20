//! Shell integration: the `newt` CLI inside built-in terminals remote-controls
//! the owning session over per-session HTTP (Unix domain socket / Windows
//! named pipe). See `design_docs/DESIGN_SHELL_INTEGRATION.md`.
//!
//! Unlike the host↔agent RPC, this protocol crosses versions: shells outlive
//! app restarts and upgrades, so unknown routes and malformed requests are
//! answered with HTTP errors, never panics.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use futures::Stream;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::terminal::TerminalHandle;
use crate::vfs::VfsPath;

pub const ENV_SOCK: &str = "NEWT_SHELL_SOCK";
pub const ENV_TERMINAL: &str = "NEWT_TERMINAL";
/// Set by the Windows `newt.cmd` shim, where argv[0] can't be `newt`.
pub const ENV_CLI: &str = "NEWT_CLI";

// ---------------------------------------------------------------------------
// Control-plane types. These also ride API_HOST_SHELL_CONTROL (bincode)
// between agent and host, where normal internal-ABI rules apply.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneSelector {
    Active,
    Other,
    Left,
    Right,
}

impl PaneSelector {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "other" => Some(Self::Other),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlRequest {
    Pwd {
        pane: PaneSelector,
    },
    /// `cd` and `focus`: non-strict navigate (a leaf path lands on the
    /// parent with the entry focused).
    Navigate {
        pane: PaneSelector,
        path: String,
        cwd: String,
    },
    /// Tier-1 registry command dispatch (same ids as keybindings/palette).
    Command {
        pane: PaneSelector,
        id: String,
    },
    ListCommands,
    /// Resolve a path argument to a VfsPath (data plane for `cat` reads the
    /// result on the session side that owns the VFS registry).
    ResolveFile {
        pane: PaneSelector,
        path: String,
        cwd: String,
    },
    /// Open the built-in viewer (or editor) on the host.
    Open {
        pane: PaneSelector,
        path: String,
        cwd: String,
        edit: bool,
    },
    /// `cp` / `mv` through the operations framework.
    Transfer {
        move_files: bool,
        sources: Vec<String>,
        dest: String,
        cwd: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandListEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlResponse {
    Ok,
    Text(String),
    Commands(Vec<CommandListEntry>),
    ResolvedFile(VfsPath),
}

pub type ControlResult = Result<ControlResponse, String>;

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

/// Session-side verb handler. The control plane always reaches the host
/// (directly in a local session, via API_HOST_SHELL_CONTROL from the agent);
/// the data plane reads on whichever side owns the session's VFS registry.
#[async_trait::async_trait]
pub trait ShellControlHandler: Send + Sync + 'static {
    async fn control(&self, req: ControlRequest) -> ControlResult;
    async fn read_file(&self, path: VfsPath) -> Result<ByteStream, String>;
}

/// Stream a file through a `FileReader` in 1 MiB chunks — the shared `cat`
/// data plane for both host (local session) and agent (remote session).
pub fn file_reader_stream(
    reader: Arc<dyn crate::file_reader::FileReader>,
    path: VfsPath,
) -> ByteStream {
    const CHUNK: u64 = 1024 * 1024;
    Box::pin(futures::stream::try_unfold(Some(0u64), move |state| {
        let reader = reader.clone();
        let path = path.clone();
        async move {
            let Some(offset) = state else {
                return Ok(None);
            };
            let chunk = reader
                .read_range(path, offset, CHUNK)
                .await
                .map_err(|e| e.to_string())?;
            if chunk.data.is_empty() {
                return Ok(None);
            }
            let next = offset + chunk.data.len() as u64;
            let next_state = (next < chunk.total_size).then_some(next);
            Ok(Some((Bytes::from(chunk.data), next_state)))
        }
    }))
}

// ---------------------------------------------------------------------------
// ShellIntegration: per-session temp dir (shim + socket), listener, env.
// ---------------------------------------------------------------------------

static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
                cli_binary.display()
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

fn full(bytes: impl Into<Bytes>) -> Body {
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
) -> Result<T, Response<Body>> {
    let bytes = req
        .into_body()
        .collect()
        .await
        .map_err(|e| status_response(StatusCode::BAD_REQUEST, format!("bad body: {e}")))?
        .to_bytes();
    serde_json::from_slice(&bytes)
        .map_err(|e| status_response(StatusCode::BAD_REQUEST, format!("bad request body: {e}")))
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
                Err(resp) => return resp,
            };
            ControlRequest::Navigate {
                pane,
                path: body.path,
                cwd: body.cwd,
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
        (&Method::POST, ["v1", verb @ ("open" | "edit")]) => {
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
                Err(resp) => return resp,
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
            let body: TransferBody = match read_json_body(req).await {
                Ok(b) => b,
                Err(resp) => return resp,
            };
            ControlRequest::Transfer {
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

// ---------------------------------------------------------------------------
// CLI client
// ---------------------------------------------------------------------------

const USAGE: &str = "newt — control the Newt session that owns this terminal

Usage:
  newt pwd [--pane <p>]              print the pane's current directory
  newt cd [path] [--pane <p>]        navigate the pane (bare: sync to $PWD)
  newt focus <path> [--pane <p>]     navigate to the parent and focus the entry
  newt cat <path> [--pane <p>]       stream a file through the session VFS
  newt open <path> [--pane <p>]      open in the built-in viewer
  newt edit <path> [--pane <p>]      open in the built-in editor
  newt cp <src>... <dest>            copy via the operations framework
  newt mv <src>... <dest>            move via the operations framework
  newt cmd [id] [--pane <p>]         run a command by registry id (bare: list)

Panes: active (default), other, left, right";

pub const VERBS: &[&str] = &[
    "pwd", "cd", "focus", "cat", "open", "edit", "cp", "mv", "cmd", "help", "--help", "-h",
];

/// True when this process should act as the shell-integration CLI.
/// `invoked_as_newt`: argv[0] basename is `newt` (Unix shim) — the Windows
/// `.cmd` shim sets NEWT_CLI instead. The main `newt` executable passes
/// `require_verb: true` so ordinary app launches are untouched.
pub fn is_cli_invocation(invoked_as_newt: bool, require_verb: bool) -> bool {
    if std::env::var_os(ENV_SOCK).is_none() {
        return false;
    }
    let shimmed = invoked_as_newt || std::env::var_os(ENV_CLI).is_some();
    if !shimmed {
        return false;
    }
    if require_verb {
        let verb = std::env::args().nth(1);
        matches!(verb.as_deref(), Some(v) if VERBS.contains(&v))
    } else {
        true
    }
}

/// Entry point for CLI mode: builds its own small runtime, never returns to
/// the caller's normal startup path.
pub fn run_cli() -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("newt: failed to start runtime: {e}");
            return 1;
        }
    };
    rt.block_on(run_cli_async(std::env::args().skip(1).collect()))
}

struct ParsedArgs {
    verb: String,
    pane: String,
    positional: Vec<String>,
}

fn parse_args(args: Vec<String>) -> Result<ParsedArgs, String> {
    let mut verb = None;
    let mut pane = "active".to_string();
    let mut positional = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--pane" => {
                pane = iter.next().ok_or("--pane requires a value")?;
                if PaneSelector::parse(&pane).is_none() {
                    return Err(format!("unknown pane: {pane}"));
                }
            }
            "--help" | "-h" => verb = verb.or(Some("help".to_string())),
            _ if verb.is_none() => verb = Some(arg),
            _ => positional.push(arg),
        }
    }
    Ok(ParsedArgs {
        verb: verb.unwrap_or_else(|| "help".to_string()),
        pane,
        positional,
    })
}

async fn run_cli_async(args: Vec<String>) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("newt: {e}");
            return 1;
        }
    };
    if parsed.verb == "help" || parsed.verb == "--help" || parsed.verb == "-h" {
        println!("{USAGE}");
        return 0;
    }
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pane = parsed.pane;

    let (method, path, body): (Method, String, Option<serde_json::Value>) =
        match parsed.verb.as_str() {
            "pwd" => (Method::GET, format!("/v1/panes/{pane}/cwd"), None),
            "cd" | "focus" => {
                let target = match parsed.positional.first() {
                    Some(p) => p.clone(),
                    None if parsed.verb == "cd" => cwd.clone(),
                    None => {
                        eprintln!("newt: focus requires a path");
                        return 1;
                    }
                };
                (
                    Method::POST,
                    format!("/v1/panes/{pane}/{}", parsed.verb),
                    Some(serde_json::json!({ "path": target, "cwd": cwd })),
                )
            }
            "cat" => {
                let Some(target) = parsed.positional.first() else {
                    eprintln!("newt: cat requires a path");
                    return 1;
                };
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("path", target)
                    .append_pair("cwd", &cwd)
                    .finish();
                (Method::GET, format!("/v1/panes/{pane}/read?{query}"), None)
            }
            "open" | "edit" => {
                let Some(target) = parsed.positional.first() else {
                    eprintln!("newt: {} requires a path", parsed.verb);
                    return 1;
                };
                (
                    Method::POST,
                    format!("/v1/{}?pane={pane}", parsed.verb),
                    Some(serde_json::json!({ "path": target, "cwd": cwd })),
                )
            }
            "cp" | "mv" => {
                if parsed.positional.len() < 2 {
                    eprintln!("newt: {} requires sources and a destination", parsed.verb);
                    return 1;
                }
                let mut sources = parsed.positional.clone();
                let dest = sources.pop().unwrap();
                let op = if parsed.verb == "mv" { "move" } else { "copy" };
                (
                    Method::POST,
                    format!("/v1/operations/{op}"),
                    Some(serde_json::json!({ "sources": sources, "dest": dest, "cwd": cwd })),
                )
            }
            "cmd" => match parsed.positional.first() {
                Some(id) => (Method::POST, format!("/v1/commands/{id}?pane={pane}"), None),
                None => (Method::GET, "/v1/commands".to_string(), None),
            },
            other => {
                eprintln!("newt: unknown verb: {other}\n\n{USAGE}");
                return 1;
            }
        };

    let raw_output = parsed.verb == "cat";
    let list_commands = parsed.verb == "cmd" && parsed.positional.is_empty();
    request(&method, &path, body, raw_output, list_commands).await
}

async fn request(
    method: &Method,
    path: &str,
    body: Option<serde_json::Value>,
    raw_output: bool,
    list_commands: bool,
) -> i32 {
    let Some(sock) = std::env::var_os(ENV_SOCK) else {
        eprintln!("newt: no Newt session (NEWT_SHELL_SOCK is not set)");
        return 2;
    };

    let stream = match connect(&sock).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("newt: no Newt session ({e})");
            return 2;
        }
    };

    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("newt: connection failed: {e}");
            return 2;
        }
    };
    tokio::spawn(conn);

    let body_bytes = body
        .map(|b| serde_json::to_vec(&b).unwrap_or_default())
        .unwrap_or_default();
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "newt")
        .header("content-type", "application/json")
        .body(full(body_bytes))
        .unwrap();

    let response = match sender.send_request(req).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("newt: request failed: {e}");
            return 2;
        }
    };

    let status = response.status();
    if raw_output && status.is_success() {
        // Stream body straight to stdout (cat).
        use tokio::io::AsyncWriteExt;
        let mut body = response.into_body();
        let mut stdout = tokio::io::stdout();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => {
                    if let Some(data) = frame.data_ref()
                        && stdout.write_all(data).await.is_err()
                    {
                        return 1; // broken pipe (e.g. | head)
                    }
                }
                Err(e) => {
                    eprintln!("newt: read failed: {e}");
                    return 1;
                }
            }
        }
        let _ = stdout.flush().await;
        return 0;
    }

    let bytes = match response.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            eprintln!("newt: response failed: {e}");
            return 2;
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    if !status.is_success() {
        let msg = if text.is_empty() {
            status.to_string()
        } else {
            text.into_owned()
        };
        eprintln!("newt: {msg}");
        return 1;
    }
    if list_commands {
        match serde_json::from_slice::<Vec<CommandListEntry>>(&bytes) {
            Ok(commands) => {
                for c in commands {
                    println!("{:<28} {}", c.id, c.name);
                }
            }
            Err(_) => println!("{text}"),
        }
    } else if !text.is_empty() {
        println!("{text}");
    }
    0
}

#[cfg(unix)]
async fn connect(
    sock: &std::ffi::OsStr,
) -> std::io::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> {
    tokio::net::UnixStream::connect(sock).await
}

#[cfg(windows)]
async fn connect(
    sock: &std::ffi::OsStr,
) -> std::io::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> {
    use tokio::net::windows::named_pipe::ClientOptions;
    // The pipe can be momentarily busy between two accepted connections;
    // retry briefly (standard named-pipe client pattern).
    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..50 {
        match ClientOptions::new().open(sock) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("pipe busy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_basics() {
        let p = parse_args(vec!["cd".into(), "/tmp".into()]).unwrap();
        assert_eq!(p.verb, "cd");
        assert_eq!(p.positional, vec!["/tmp"]);
        assert_eq!(p.pane, "active");

        let p = parse_args(vec!["pwd".into(), "--pane".into(), "other".into()]).unwrap();
        assert_eq!(p.verb, "pwd");
        assert_eq!(p.pane, "other");

        assert!(parse_args(vec!["pwd".into(), "--pane".into(), "bogus".into()]).is_err());

        let p = parse_args(vec!["cp".into(), "a".into(), "b".into(), "dest/".into()]).unwrap();
        assert_eq!(p.positional, vec!["a", "b", "dest/"]);
    }

    #[test]
    fn cli_invocation_guard() {
        // No env → never CLI mode. (Deliberately not testing the env-set
        // cases here: process env is shared across the test binary.)
        assert!(!is_cli_invocation(true, false));
        assert!(!is_cli_invocation(false, false));
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
