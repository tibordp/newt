//! The `newt` CLI: argv parsing and the HTTP client side of the control
//! protocol. Both binaries call [`run_cli`] as their first act when
//! [`is_cli_invocation`] says this process is a shim invocation.

use http_body_util::BodyExt;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;

use super::server::full;
use super::{CommandListEntry, ENV_CLI, ENV_SOCK, EntryKind, ListEntry, PaneSelector, SelectMode};

use clap::{Args, CommandFactory, Parser, Subcommand};

const AFTER_HELP: &str = "\
Paths:
  Arguments accept native paths, `~`, and the URLs of mounted VFSes
  (s3://bucket/key, sftp://host/path, archive paths). cd, focus, cp, mv, rm
  and mkdir resolve relative paths against the shell's working directory;
  cat, view and edit resolve them against the pane (what you see in the file
  list), so they work inside archives and S3 mounts.

Exit codes:
  0  ok        1  error        2  no Newt session owns this terminal

Examples:
  newt cd                     sync the active pane to the shell's cwd
  newt cd ~/src --pane other  navigate the other pane
  newt cat s3://bucket/key    stream a file through the session's mounts
  newt select '*.rs' --add    add every .rs entry to the selection
  git ls-files | newt select  select the tracked files of the pane's directory
  newt ls -sp0 | xargs -0 du  the selected entries, as paths
  newt cp --wait a.iso /mnt   copy, and return once the copy has finished
  EDITOR='newt edit --wait'   edit commit messages in the built-in editor";

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

    /// List the pane's entries, or its selection
    #[command(long_about = "\
List the pane's entries as it lists them — in its order, with its filter
and hidden-file setting applied, without `..` — one name per line. Works in
archives and S3 prefixes, where the shell has nothing to list.

With --selection, only what a command such as Copy would act on: the
selected entries, or the entry under the cursor when nothing is selected.
With --all, every entry, the ones the pane hides included.")]
    #[command(after_help = "\
Examples:
  newt ls -lh                     sizes and dates, as ls -lh shows them
  newt ls -s                      names of the selected entries
  newt ls -sp0 | xargs -0 wc -l   the selection, as paths for another tool
  newt ls --json | jq '.[] | select(.size > 1e9) | .name'")]
    #[command(disable_help_flag = true)]
    Ls {
        /// Only the selection (the cursor entry when nothing is selected)
        #[arg(short, long)]
        selection: bool,
        /// Also the entries the pane hides (hidden files, a filter's misses)
        #[arg(short, long, conflicts_with = "selection")]
        all: bool,
        /// Long format: mode, owner, size and modification time
        #[arg(short, long)]
        long: bool,
        /// With --long, sizes in K, M, G…
        #[arg(short = 'h', long)]
        human_readable: bool,
        /// Print display paths instead of names
        #[arg(short, long)]
        paths: bool,
        /// Separate entries with NUL instead of a newline
        #[arg(short = '0', long, conflicts_with = "long")]
        null: bool,
        /// Print a JSON array of {name, path, kind, size, modified, mode, user, group, target}
        #[arg(long, conflicts_with_all = ["paths", "null", "long"])]
        json: bool,
        /// Print help (-h is --human-readable here, as in ls)
        #[arg(long, action = clap::ArgAction::HelpLong)]
        help: Option<bool>,
    },

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
the pane. With --wait, return only once the window has closed, which makes
`newt edit --wait` usable as $EDITOR (git commit, crontab -e, …).")]
    Edit {
        /// File to edit (pane-relative)
        path: String,
        /// Return once the editor window has closed
        #[arg(long)]
        wait: bool,
    },

    /// Copy files through the operations panel
    #[command(long_about = "\
Enqueue a copy through the operations framework and print the operation
id; progress, conflicts and cancellation show up in the operations panel.
Several sources need an existing directory as DEST. A single source may
name a new file (copied under that name); a trailing slash on DEST insists
on a directory.

With --wait, return once the copy has ended instead: exit 0 when it
finished, 1 with its error when it failed or was cancelled. Interrupting
newt (Ctrl-C) cancels the copy.")]
    Cp {
        /// Files or directories to copy
        #[arg(required = true, value_name = "SRC")]
        sources: Vec<String>,
        /// Destination directory, or new name for a single source
        #[arg(value_name = "DEST")]
        dest: String,
        /// Return once the copy has ended
        #[arg(long)]
        wait: bool,
    },

    /// Move files through the operations panel
    #[command(long_about = "\
Enqueue a move through the operations framework and print the operation
id. Destination rules are those of `newt cp`; moving a single source to a
new name in the same directory is a plain rename. --wait as for `newt cp`.")]
    Mv {
        /// Files or directories to move
        #[arg(required = true, value_name = "SRC")]
        sources: Vec<String>,
        /// Destination directory, or new name for a single source
        #[arg(value_name = "DEST")]
        dest: String,
        /// Return once the move has ended
        #[arg(long)]
        wait: bool,
    },

    /// Delete files through the operations panel
    #[command(long_about = "\
Enqueue a delete through the operations framework and print the operation
id. Items go to the trash when `behavior.delete_to_trash` is on (the
default) and the filesystem has one; with the setting on and no trash
there, nothing is deleted unless --permanent is given. Directories need
--recursive. Nothing asks for confirmation. --wait as for `newt cp`.")]
    Rm {
        /// Files or directories to delete
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<String>,
        /// Delete directories and their contents
        #[arg(short, long)]
        recursive: bool,
        /// Delete permanently instead of moving to the trash
        #[arg(long)]
        permanent: bool,
        /// Return once the delete has ended
        #[arg(long)]
        wait: bool,
    },

    /// Create directories through the session's filesystem
    #[command(long_about = "\
Create each PATH as a directory, on whatever filesystem it names — an S3
prefix, an SFTP mount, the remote host of an SSH session.")]
    Mkdir {
        /// Directories to create
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<String>,
        /// Create missing parents; an existing directory is no error
        #[arg(short, long)]
        parents: bool,
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
`newt cmd swap_panes`. A user command can also be named by its title
(`newt cmd 'Open in Vim'`, ignoring case). Any open dialog is closed first,
exactly as if the key had been pressed. Without ID, lists every id with
its name.")]
    Cmd {
        /// Command id, or a user command's title (see the bare listing)
        id: Option<String>,
    },

    /// Print a shell completion script
    #[command(long_about = "\
Print a completion script for SHELL. Load it from the shell's startup file
only inside Newt's terminals, where `newt` is this CLI:

  [ -n \"$NEWT_SHELL_SOCK\" ] && source <(newt completions bash)")]
    Completions {
        /// Shell to complete for
        shell: clap_complete::Shell,
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

/// How a successful response is printed.
enum Output {
    /// The body as a line of text, when there is any.
    Text,
    /// The body streamed to stdout as is (`cat`).
    Raw,
    Commands,
    Entries(EntryFormat),
}

#[derive(Clone, Copy)]
struct EntryFormat {
    paths: bool,
    null: bool,
    json: bool,
    long: bool,
    human_readable: bool,
}

/// The `&wait=1` query suffix when waiting, else nothing.
fn wait_query(wait: bool) -> &'static str {
    if wait { "&wait=1" } else { "" }
}

async fn run_cli_async(cli: Cli) -> i32 {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pane = cli.pane.pane.as_str();
    let mut output = Output::Text;

    let (method, path, body): (Method, String, Option<serde_json::Value>) = match cli.verb {
        Verb::Pwd => (Method::GET, format!("/v1/panes/{pane}/cwd"), None),
        Verb::Ls {
            selection,
            all,
            long,
            human_readable,
            paths,
            null,
            json,
            help: _,
        } => {
            output = Output::Entries(EntryFormat {
                paths,
                null,
                json,
                long,
                human_readable,
            });
            let query = if all {
                "?all=1"
            } else if selection {
                "?selection=1"
            } else {
                ""
            };
            (
                Method::GET,
                format!("/v1/panes/{pane}/entries{query}"),
                None,
            )
        }
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
            output = Output::Raw;
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
        Verb::Edit { path, wait } => (
            Method::POST,
            format!("/v1/edit?pane={pane}{}", wait_query(wait)),
            Some(serde_json::json!({ "path": path, "cwd": cwd })),
        ),
        Verb::Cp {
            sources,
            dest,
            wait,
        } => (
            Method::POST,
            format!("/v1/operations/copy?pane={pane}{}", wait_query(wait)),
            Some(serde_json::json!({ "sources": sources, "dest": dest, "cwd": cwd })),
        ),
        Verb::Mv {
            sources,
            dest,
            wait,
        } => (
            Method::POST,
            format!("/v1/operations/move?pane={pane}{}", wait_query(wait)),
            Some(serde_json::json!({ "sources": sources, "dest": dest, "cwd": cwd })),
        ),
        Verb::Rm {
            paths,
            recursive,
            permanent,
            wait,
        } => (
            Method::POST,
            format!("/v1/operations/delete?pane={pane}{}", wait_query(wait)),
            Some(serde_json::json!({
                "paths": paths,
                "cwd": cwd,
                "recursive": recursive,
                "permanent": permanent,
            })),
        ),
        Verb::Mkdir { paths, parents } => (
            Method::POST,
            format!("/v1/mkdir?pane={pane}"),
            Some(serde_json::json!({ "paths": paths, "cwd": cwd, "parents": parents })),
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
            output = Output::Commands;
            (Method::GET, "/v1/commands".to_string(), None)
        }
        Verb::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "newt", &mut std::io::stdout());
            return 0;
        }
    };

    request(&method, &path, body, output).await
}

async fn request(
    method: &Method,
    path: &str,
    body: Option<serde_json::Value>,
    output: Output,
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
    if matches!(output, Output::Raw) && status.is_success() {
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
    match output {
        Output::Commands => match serde_json::from_slice::<Vec<CommandListEntry>>(&bytes) {
            Ok(commands) => {
                for c in commands {
                    println!("{:<28} {}", c.id, c.name);
                }
            }
            Err(_) => println!("{text}"),
        },
        Output::Entries(format) if format.json => println!("{text}"),
        Output::Entries(format) => {
            let entries = match serde_json::from_slice::<Vec<ListEntry>>(&bytes) {
                Ok(entries) => entries,
                Err(e) => {
                    eprintln!("newt: unexpected listing: {e}");
                    return 1;
                }
            };
            return print_entries(&entries, format);
        }
        Output::Text | Output::Raw => {
            if !text.is_empty() {
                println!("{text}");
            }
        }
    }
    0
}

fn print_entries(entries: &[ListEntry], format: EntryFormat) -> i32 {
    use std::io::Write;
    let lines = if format.long {
        long_lines(
            entries,
            format,
            jiff::Timestamp::now(),
            &jiff::tz::TimeZone::system(),
        )
    } else {
        entries
            .iter()
            .map(|entry| entry_label(entry, format.paths).to_string())
            .collect()
    };
    let terminator = if format.null { b"\0" } else { b"\n" };
    let mut stdout = std::io::stdout().lock();
    for line in lines {
        if stdout.write_all(line.as_bytes()).is_err() || stdout.write_all(terminator).is_err() {
            return 1; // broken pipe (e.g. | head)
        }
    }
    if stdout.flush().is_err() { 1 } else { 0 }
}

fn entry_label(entry: &ListEntry, paths: bool) -> &str {
    if paths { &entry.path } else { &entry.name }
}

/// `ls -l`'s columns: mode, user, group, size, time, name. An owner column
/// is left out when no entry has one (S3, archives); a missing value
/// otherwise prints as `-`.
fn long_lines(
    entries: &[ListEntry],
    format: EntryFormat,
    now: jiff::Timestamp,
    tz: &jiff::tz::TimeZone,
) -> Vec<String> {
    let dash = || "-".to_string();
    let rows: Vec<[String; 5]> = entries
        .iter()
        .map(|entry| {
            let size = match entry.size {
                Some(size) if format.human_readable => human_size(size),
                Some(size) => size.to_string(),
                None => dash(),
            };
            let time = entry
                .modified
                .map_or_else(dash, |seconds| ls_time(seconds, now, tz));
            [
                mode_string(entry),
                entry.user.clone().unwrap_or_else(dash),
                entry.group.clone().unwrap_or_else(dash),
                size,
                time,
            ]
        })
        .collect();
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (user_width, group_width, size_width, time_width) =
        (width(1), width(2), width(3), width(4));
    let show_user = entries.iter().any(|entry| entry.user.is_some());
    let show_group = entries.iter().any(|entry| entry.group.is_some());
    rows.into_iter()
        .zip(entries)
        .map(|([mode, user, group, size, time], entry)| {
            let mut line = mode;
            if show_user {
                line.push_str(&format!(" {user:<user_width$}"));
            }
            if show_group {
                line.push_str(&format!(" {group:<group_width$}"));
            }
            line.push_str(&format!(" {size:>size_width$} {time:<time_width$} "));
            line.push_str(entry_label(entry, format.paths));
            if let Some(target) = &entry.target {
                line.push_str(" -> ");
                line.push_str(target);
            }
            line
        })
        .collect()
}

/// `drwxr-xr-x`: the type from the mode's type bits (or the entry's kind
/// when the filesystem reports no mode), then the permissions, `?` when
/// unknown.
fn mode_string(entry: &ListEntry) -> String {
    let file_type = match entry.mode.map(|mode| mode & 0o170000) {
        Some(0o040000) => 'd',
        Some(0o120000) => 'l',
        Some(0o010000) => 'p',
        Some(0o140000) => 's',
        Some(0o020000) => 'c',
        Some(0o060000) => 'b',
        Some(0o100000) => '-',
        _ => match entry.kind {
            EntryKind::Dir => 'd',
            EntryKind::Symlink => 'l',
            EntryKind::Special => '?',
            EntryKind::File => '-',
        },
    };
    let Some(mode) = entry.mode else {
        return format!("{file_type}?????????");
    };
    let bit = |mask: u32, c: char| if mode & mask != 0 { c } else { '-' };
    // The execute slot also carries setuid, setgid and sticky: lowercase
    // over an execute bit, uppercase without one.
    let exec = |x: u32, special: u32, set: char| match (mode & x != 0, mode & special != 0) {
        (true, true) => set,
        (false, true) => set.to_ascii_uppercase(),
        (true, false) => 'x',
        (false, false) => '-',
    };
    [
        file_type,
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        exec(0o100, 0o4000, 's'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        exec(0o010, 0o2000, 's'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        exec(0o001, 0o1000, 't'),
    ]
    .iter()
    .collect()
}

/// `ls -h`: powers of 1024, rounded up, one decimal below 10.
fn human_size(bytes: u64) -> String {
    const UNITS: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];
    if bytes < 1024 {
        return bytes.to_string();
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let tenths = (value * 10.0).ceil() / 10.0;
    if tenths < 10.0 {
        return format!("{tenths:.1}{}", UNITS[unit]);
    }
    let whole = value.ceil();
    if whole >= 1024.0 && unit < UNITS.len() - 1 {
        return format!("1.0{}", UNITS[unit + 1]);
    }
    format!("{whole}{}", UNITS[unit])
}

/// `ls -l`'s time: month, day and time within the last six months; month,
/// day and year before that, and in the future.
fn ls_time(seconds: i64, now: jiff::Timestamp, tz: &jiff::tz::TimeZone) -> String {
    const HALF_YEAR: i64 = 365 * 24 * 3600 / 2;
    let Ok(time) = jiff::Timestamp::from_second(seconds) else {
        return "-".to_string();
    };
    let age = now.as_second() - seconds;
    let format = if (0..HALF_YEAR).contains(&age) {
        "%b %e %H:%M"
    } else {
        "%b %e  %Y"
    };
    time.to_zoned(tz.clone()).strftime(format).to_string()
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
            matches!(cli.verb, Verb::Cp { sources, dest, wait: false } if sources == ["a", "b"] && dest == "dest/")
        );
        assert!(matches!(
            parse(&["mv", "--wait", "a", "b"]).unwrap().verb,
            Verb::Mv { wait: true, .. }
        ));

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
    fn parse_ls_rm_mkdir() {
        assert!(matches!(
            parse(&["ls", "-sp0"]).unwrap().verb,
            Verb::Ls {
                selection: true,
                paths: true,
                null: true,
                json: false,
                ..
            }
        ));
        assert!(matches!(
            parse(&["ls", "--json"]).unwrap().verb,
            Verb::Ls { json: true, .. }
        ));
        // JSON carries both name and path, and has its own framing.
        assert!(parse(&["ls", "--json", "-p"]).is_err());
        assert!(parse(&["ls", "--json", "-0"]).is_err());

        assert!(matches!(
            parse(&["rm", "-r", "--wait", "a", "b"]).unwrap().verb,
            Verb::Rm { paths, recursive: true, permanent: false, wait: true } if paths == ["a", "b"]
        ));
        assert!(parse(&["rm"]).is_err());
        assert!(matches!(
            parse(&["mkdir", "-p", "x/y"]).unwrap().verb,
            Verb::Mkdir { parents: true, .. }
        ));
        assert!(matches!(
            parse(&["edit", "--wait", "f"]).unwrap().verb,
            Verb::Edit { wait: true, .. }
        ));
        assert!(matches!(
            parse(&["completions", "zsh"]).unwrap().verb,
            Verb::Completions { .. }
        ));
        assert!(parse(&["completions", "tcsh"]).is_err());
    }

    fn entry(kind: EntryKind, mode: Option<u32>) -> ListEntry {
        ListEntry {
            name: "x".into(),
            path: "/d/x".into(),
            kind,
            size: Some(1),
            modified: None,
            mode,
            user: None,
            group: None,
            target: None,
        }
    }

    #[test]
    fn ls_flags() {
        assert!(matches!(
            parse(&["ls", "-alh"]).unwrap().verb,
            Verb::Ls {
                all: true,
                long: true,
                human_readable: true,
                ..
            }
        ));
        assert!(parse(&["ls", "-as"]).is_err());
        assert!(parse(&["ls", "-l0"]).is_err());
        // `-h` is ls's human-readable; help is long-only here.
        let err = parse(&["ls", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn modes_render_like_ls() {
        let mode = |kind, mode| mode_string(&entry(kind, Some(mode)));
        assert_eq!(mode(EntryKind::Dir, 0o040755), "drwxr-xr-x");
        assert_eq!(mode(EntryKind::File, 0o104755), "-rwsr-xr-x");
        assert_eq!(mode(EntryKind::Dir, 0o041777), "drwxrwxrwt");
        assert_eq!(mode(EntryKind::File, 0o102644), "-rw-r-Sr--");
        assert_eq!(mode_string(&entry(EntryKind::Dir, None)), "d?????????");
    }

    #[test]
    fn human_sizes_round_up_like_ls() {
        assert_eq!(human_size(1023), "1023");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1025), "1.1K");
        assert_eq!(human_size(10 * 1024), "10K");
        assert_eq!(human_size(1024 * 1024 - 1), "1.0M");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0G");
    }

    #[test]
    fn times_show_the_year_once_old() {
        let tz = jiff::tz::TimeZone::UTC;
        let now = jiff::Timestamp::from_second(1_790_000_000).unwrap();
        assert_eq!(ls_time(1_789_990_000, now, &tz), "Sep 21 11:26");
        assert_eq!(ls_time(1_700_000_000, now, &tz), "Nov 14  2023");
        assert_eq!(ls_time(1_800_000_000, now, &tz), "Jan 15  2027");
    }

    #[test]
    fn long_lines_align_and_drop_unknown_owners() {
        let format = EntryFormat {
            paths: false,
            null: false,
            json: false,
            long: true,
            human_readable: false,
        };
        let mut link = entry(EntryKind::Symlink, Some(0o120777));
        link.name = "l".into();
        link.size = Some(12345);
        link.target = Some("x".into());
        let lines = long_lines(
            &[entry(EntryKind::File, Some(0o100644)), link],
            format,
            jiff::Timestamp::UNIX_EPOCH,
            &jiff::tz::TimeZone::UTC,
        );
        assert_eq!(lines, ["-rw-r--r--     1 - x", "lrwxrwxrwx 12345 - l -> x"]);
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
        for expected in [
            "pwd", "ls", "cd", "view", "select", "rm", "mkdir", "cmd", "help", "--help",
        ] {
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
