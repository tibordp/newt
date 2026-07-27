//! The `newt` CLI: argv parsing and the HTTP client side of the control
//! protocol. Both binaries call [`run_cli`] as their first act when
//! [`is_cli_invocation`] says this process is a shim invocation.

use http_body_util::BodyExt;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;

use super::server::full;
use super::{CommandListEntry, ENV_CLI, ENV_SOCK, PaneSelector};

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
}
