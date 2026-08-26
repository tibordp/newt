//! The `newt` CLI: argv parsing and the HTTP client side of the control
//! protocol. Both binaries call [`run_cli`] as their first act when
//! [`is_cli_invocation`] says this process is a shim invocation.

use http_body_util::BodyExt;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;

use super::server::full;
use super::{CommandListEntry, ENV_CLI, ENV_SOCK, PaneSelector, SelectMode};

use clap::{Args, CommandFactory, Parser, Subcommand};

const AFTER_HELP: &str = "\
Paths:
  Arguments accept native paths, `~`, and the URLs of mounted VFSes
  (s3://bucket/key, sftp://host/path, archive paths). cd, focus, cp and mv
  resolve relative paths against the shell's working directory; cat, view
  and edit resolve them against the pane (what you see in the file list),
  so they work inside archives and S3 mounts.

Exit codes:
  0  ok        1  error        2  no Newt session owns this terminal

Examples:
  newt cd                     sync the active pane to the shell's cwd
  newt cd ~/src --pane other  navigate the other pane
  newt cat s3://bucket/key    stream a file through the session's mounts
  newt select '*.rs' --add    add every .rs entry to the selection
  git ls-files | newt select  select the tracked files of the pane's directory";

/// Control the Newt session that owns this terminal.
#[derive(Parser, Debug)]
#[command(
    name = "newt",
    bin_name = "newt",
    disable_version_flag = true,
    after_help = AFTER_HELP,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
    #[command(flatten)]
    pane: PaneArg,
}

#[derive(Args, Debug, Clone, Copy)]
struct PaneArg {
    /// Pane to act on
    #[arg(
        long,
        global = true,
        default_value = "active",
        value_name = "PANE",
        value_parser = parse_pane,
        help = "Pane to act on: active, other, left or right"
    )]
    pane: PaneSelector,
}

fn parse_pane(s: &str) -> Result<PaneSelector, String> {
    PaneSelector::parse(s).ok_or_else(|| "expected active, other, left or right".to_string())
}

impl PaneSelector {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Other => "other",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Subcommand, Debug)]
enum Verb {
    /// Print the pane's current directory
    #[command(long_about = "\
Print the pane's current directory as a display path: a native path on the
root filesystem, a URL (s3://…, sftp://…) on a mounted VFS. Round-trips
through `newt cd`.")]
    Pwd,

    /// Navigate the pane (bare: sync it to the shell's cwd)
    #[command(long_about = "\
Navigate the pane. Without a path, the pane follows the shell's working
directory. A path naming a file lands on its parent directory with the file
focused. A URL must belong to an already-mounted VFS; nothing is mounted on
your behalf.")]
    Cd {
        /// Directory or file to go to (default: the shell's cwd)
        path: Option<String>,
    },

    /// Navigate to a file's directory and focus it
    #[command(long_about = "\
Navigate to the parent directory of PATH and put the cursor on the entry.
The same as `newt cd` with a file path, but a path is required.")]
    Focus {
        /// File or directory to focus
        path: String,
    },

    /// Stream a file to stdout through the session's filesystem
    #[command(long_about = "\
Stream a file to stdout through the session's filesystem. Relative paths
resolve against the pane, so `newt cat README.md` works inside an archive
or an S3 prefix. Bytes come from wherever the session runs (the remote
host in an SSH session).")]
    Cat {
        /// File to read (pane-relative)
        path: String,
    },

    /// Open a file in the built-in viewer
    #[command(long_about = "\
Open PATH in the built-in viewer window (text, hex, image, audio, video or
PDF by content). Relative paths resolve against the pane.")]
    View {
        /// File to view (pane-relative)
        path: String,
    },

    /// Open a file in the built-in editor
    #[command(long_about = "\
Open PATH in the built-in editor window. Relative paths resolve against
the pane.")]
    Edit {
        /// File to edit (pane-relative)
        path: String,
    },

    /// Copy files through the operations panel
    #[command(long_about = "\
Enqueue a copy through the operations framework and print the operation
id; progress, conflicts and cancellation show up in the operations panel.
Several sources need an existing directory as DEST. A single source may
name a new file (copied under that name); a trailing slash on DEST insists
on a directory.")]
    Cp {
        /// Files or directories to copy
        #[arg(required = true, value_name = "SRC")]
        sources: Vec<String>,
        /// Destination directory, or new name for a single source
        #[arg(value_name = "DEST")]
        dest: String,
    },

    /// Move files through the operations panel
    #[command(long_about = "\
Enqueue a move through the operations framework and print the operation
id. Destination rules are those of `newt cp`; moving a single source to a
new name in the same directory is a plain rename.")]
    Mv {
        /// Files or directories to move
        #[arg(required = true, value_name = "SRC")]
        sources: Vec<String>,
        /// Destination directory, or new name for a single source
        #[arg(value_name = "DEST")]
        dest: String,
    },

    /// Select entries in the pane by pattern or from stdin
    #[command(long_about = "\
Select entries in the pane. Each PATTERN is a case-insensitive glob (`*.rs`,
`IMG_*.{jpg,png}`) matched against the visible entries' names, or with a
leading `/` a regular expression (`/^foo.*\\.c$`); the matches of several
patterns are combined, so an unquoted glob the shell has already expanded
into file names works too. Without PATTERN, names are read from stdin, one
per line: a bare name is an entry as listed in the pane, a name containing
a separator is resolved against the shell's working directory and counts
only if it lands in the pane's directory.

The selection is replaced unless --add or --remove is given. Prints how
many entries matched.")]
    #[command(after_help = "\
Examples:
  newt select '*.log'            select every .log entry
  newt select *.c *.h            unquoted: the shell expands, newt unions
  newt select '/^\\d{4}-'         regex: entries starting with four digits
  newt select --remove '*.bak'   drop .bak entries from the selection
  ls -t | head -5 | newt select  the five most recently modified entries
  git ls-files | newt select     tracked files of the pane's directory")]
    Select {
        /// Globs (or /regexes) to match entry names against
        #[arg(value_name = "PATTERN")]
        patterns: Vec<String>,
        /// Add matches to the current selection
        #[arg(long)]
        add: bool,
        /// Remove matches from the current selection
        #[arg(long, conflicts_with = "add")]
        remove: bool,
    },

    /// Run a command by its registry id (bare: list them)
    #[command(long_about = "\
Run a command from the command registry — the same ids that keybindings,
the command palette and user commands use, e.g. `newt cmd refresh` or
`newt cmd swap_panes`. Any open dialog is closed first, exactly as if the
key had been pressed. Without ID, lists every id with its name.")]
    Cmd {
        /// Command id (see the bare listing)
        id: Option<String>,
    },
}

/// Names that select CLI mode in the main `newt` executable, where an
/// ordinary app launch must not be mistaken for a shim invocation.
pub fn verbs() -> Vec<String> {
    let mut verbs: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    verbs.extend(["help", "--help", "-h"].map(String::from));
    verbs
}

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
        matches!(verb, Some(v) if verbs().contains(&v))
    } else {
        true
    }
}

/// Entry point for CLI mode: builds its own small runtime, never returns to
/// the caller's normal startup path.
pub fn run_cli() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // Help and version print themselves; usage errors already carry
        // the "error:" prefix and usage line.
        Err(e) => {
            return if e.use_stderr() {
                e.print().ok();
                1
            } else {
                e.print().ok();
                0
            };
        }
    };
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
    rt.block_on(run_cli_async(cli))
}

/// Non-empty stdin lines, or an error when stdin is a terminal (nothing
/// piped, no pattern given).
fn names_from_stdin() -> Result<Vec<String>, String> {
    use std::io::{BufRead, IsTerminal};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err("select needs a PATTERN, or names on stdin".into());
    }
    let mut names = Vec::new();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("reading stdin: {e}"))?;
        if !line.is_empty() {
            names.push(line);
        }
    }
    Ok(names)
}

async fn run_cli_async(cli: Cli) -> i32 {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pane = cli.pane.pane.as_str();
    let mut raw_output = false;
    let mut list_commands = false;

    let (method, path, body): (Method, String, Option<serde_json::Value>) = match cli.verb {
        Verb::Pwd => (Method::GET, format!("/v1/panes/{pane}/cwd"), None),
        Verb::Cd { path } => (
            Method::POST,
            format!("/v1/panes/{pane}/cd"),
            Some(serde_json::json!({ "path": path.unwrap_or_else(|| cwd.clone()), "cwd": cwd })),
        ),
        Verb::Focus { path } => (
            Method::POST,
            format!("/v1/panes/{pane}/focus"),
            Some(serde_json::json!({ "path": path, "cwd": cwd })),
        ),
        Verb::Cat { path } => {
            raw_output = true;
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("path", &path)
                .append_pair("cwd", &cwd)
                .finish();
            (Method::GET, format!("/v1/panes/{pane}/read?{query}"), None)
        }
        Verb::View { path } => (
            Method::POST,
            format!("/v1/view?pane={pane}"),
            Some(serde_json::json!({ "path": path, "cwd": cwd })),
        ),
        Verb::Edit { path } => (
            Method::POST,
            format!("/v1/edit?pane={pane}"),
            Some(serde_json::json!({ "path": path, "cwd": cwd })),
        ),
        Verb::Cp { sources, dest } => (
            Method::POST,
            "/v1/operations/copy".to_string(),
            Some(serde_json::json!({ "sources": sources, "dest": dest, "cwd": cwd })),
        ),
        Verb::Mv { sources, dest } => (
            Method::POST,
            "/v1/operations/move".to_string(),
            Some(serde_json::json!({ "sources": sources, "dest": dest, "cwd": cwd })),
        ),
        Verb::Select {
            patterns,
            add,
            remove,
        } => {
            let names = if !patterns.is_empty() {
                Vec::new()
            } else {
                match names_from_stdin() {
                    Ok(names) => names,
                    Err(e) => {
                        eprintln!("newt: {e}");
                        return 1;
                    }
                }
            };
            let mode = if add {
                SelectMode::Add
            } else if remove {
                SelectMode::Remove
            } else {
                SelectMode::Replace
            };
            (
                Method::POST,
                format!("/v1/panes/{pane}/select"),
                Some(serde_json::json!({
                    "patterns": patterns,
                    "names": names,
                    "cwd": cwd,
                    "mode": mode,
                })),
            )
        }
        Verb::Cmd { id: Some(id) } => {
            (Method::POST, format!("/v1/commands/{id}?pane={pane}"), None)
        }
        Verb::Cmd { id: None } => {
            list_commands = true;
            (Method::GET, "/v1/commands".to_string(), None)
        }
    };

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

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("newt").chain(args.iter().copied()))
    }

    #[test]
    fn parse_basics() {
        let cli = parse(&["cd", "/tmp"]).unwrap();
        assert!(matches!(cli.verb, Verb::Cd { path: Some(p) } if p == "/tmp"));
        assert_eq!(cli.pane.pane, PaneSelector::Active);

        let cli = parse(&["pwd", "--pane", "other"]).unwrap();
        assert!(matches!(cli.verb, Verb::Pwd));
        assert_eq!(cli.pane.pane, PaneSelector::Other);

        // --pane is global: accepted before the verb too.
        let cli = parse(&["--pane", "left", "pwd"]).unwrap();
        assert_eq!(cli.pane.pane, PaneSelector::Left);

        assert!(parse(&["pwd", "--pane", "bogus"]).is_err());
        assert!(parse(&["focus"]).is_err());
        assert!(parse(&["cp", "only-one"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn parse_transfer_and_select() {
        let cli = parse(&["cp", "a", "b", "dest/"]).unwrap();
        assert!(
            matches!(cli.verb, Verb::Cp { sources, dest } if sources == ["a", "b"] && dest == "dest/")
        );

        let cli = parse(&["select", "--remove", "*.o", "*.a"]).unwrap();
        assert!(
            matches!(cli.verb, Verb::Select { patterns, add: false, remove: true } if patterns == ["*.o", "*.a"])
        );
        assert!(matches!(
            parse(&["select"]).unwrap().verb,
            Verb::Select { patterns, .. } if patterns.is_empty()
        ));
        assert!(parse(&["select", "--add", "--remove", "x"]).is_err());
    }

    #[test]
    fn help_is_available_everywhere() {
        for args in [vec!["--help"], vec!["select", "--help"], vec!["cp", "-h"]] {
            let err = parse(&args).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        }
        Cli::command().debug_assert();
    }

    #[test]
    fn verbs_track_the_subcommands() {
        let v = verbs();
        for expected in ["pwd", "cd", "view", "select", "cmd", "help", "--help"] {
            assert!(v.iter().any(|x| x == expected), "{expected}");
        }
        assert!(!v.iter().any(|x| x == "open"));
    }

    #[test]
    fn cli_invocation_guard() {
        // No env → never CLI mode. (Deliberately not testing the env-set
        // cases here: process env is shared across the test binary.)
        assert!(!is_cli_invocation(true, false));
        assert!(!is_cli_invocation(false, false));
    }
}
