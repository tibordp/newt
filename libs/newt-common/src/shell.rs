//! Shell invocation + program resolution.
//!
//! Two related concerns live here:
//!
//! * [`run_via_shell`] — craft the `(program, args)` that runs a command
//!   string through the platform's command interpreter (`sh -c …` on
//!   Unix, `cmd.exe /C …` on Windows). Every place that used to hardcode
//!   `"sh"` goes through this so the integrated terminal, the run-command
//!   operation, and custom connection commands all do the right thing per
//!   OS.
//!
//! * [`ShellFlavor::quote`] — quote a value as one word for the
//!   interpreter [`run_via_shell`] picks on a given OS.
//!
//! * [`resolve_program`] — resolve a bare program name to an absolute
//!   path against the inherited `PATH` (and a user-configured fallback)
//!   *without* mutating our own environment. Subprocesses the user expects
//!   to inherit *their* shell's `PATH` (notably the terminal) must not see
//!   our augmented version, which could reorder their preferred lookup.
//!
//! * [`ShellService`] — `~`/env expansion of user-typed paths on the
//!   shell's filesystem, remoted over RPC in remote sessions.

use crate::Error;
use crate::proc::NoConsoleWindow;
use crate::rpc::Communicator;

/// `~`/env expansion of a user-typed path on the shell's filesystem.
///
/// Returns a **VFS path**, not `std::path` — the result crosses the RPC
/// boundary, and the native→VFS decode happens here, on the side the
/// shell actually runs (the agent in a remote session), in its own OS.
/// `None` means the expansion isn't an absolute path (caller resolves it
/// relative to the pane instead).
#[async_trait::async_trait]
pub trait ShellService: Send + Sync {
    async fn shell_expand(&self, input: String)
    -> Result<Option<crate::vfs::path::PathBuf>, Error>;
}

/// Decode an expanded native path into a VFS path, but only if it is
/// absolute (a relative expansion has no meaningful VFS form here).
fn expanded_to_vfs(p: &std::path::Path) -> Option<crate::vfs::path::PathBuf> {
    p.is_absolute()
        .then(|| crate::vfs::path::PathBuf::from_native(p))
}

pub struct LocalShellService;

#[cfg(unix)]
#[async_trait::async_trait]
impl ShellService for LocalShellService {
    async fn shell_expand(
        &self,
        input: String,
    ) -> Result<Option<crate::vfs::path::PathBuf>, Error> {
        let expanded =
            tokio::task::spawn_blocking(move || expanduser::expanduser(input).map_err(Error::from))
                .await??;
        Ok(expanded_to_vfs(&expanded))
    }
}

#[cfg(windows)]
#[async_trait::async_trait]
impl ShellService for LocalShellService {
    async fn shell_expand(
        &self,
        input: String,
    ) -> Result<Option<crate::vfs::path::PathBuf>, Error> {
        // Windows has no pwd database, so only the bare `~` / `~/...` form is supported.
        // `~user/...` is left as-is.
        let expanded = if input == "~" {
            dirs::home_dir().ok_or_else(|| Error::custom("could not determine home directory"))?
        } else if let Some(rest) = input
            .strip_prefix("~/")
            .or_else(|| input.strip_prefix("~\\"))
        {
            let mut home = dirs::home_dir()
                .ok_or_else(|| Error::custom("could not determine home directory"))?;
            home.push(rest);
            home
        } else {
            std::path::PathBuf::from(input)
        };
        Ok(expanded_to_vfs(&expanded))
    }
}

pub struct ShellRemote {
    communicator: Communicator,
}

impl ShellRemote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl ShellService for ShellRemote {
    async fn shell_expand(
        &self,
        input: String,
    ) -> Result<Option<crate::vfs::path::PathBuf>, Error> {
        let ret: Result<Option<crate::vfs::path::PathBuf>, Error> = self
            .communicator
            .invoke(crate::api::API_SHELL_EXPAND, &input)
            .await?;
        Ok(ret?)
    }
}

/// Run a prepared command to completion and capture stdout. On failure the
/// error string is the trimmed stderr, or the exit code when stderr is
/// empty. `timeout` bounds the whole run — pass it when probing external
/// tools that may hang; `None` for commands that legitimately run long
/// (e.g. `git status` on a large repo). The caller keeps ownership of
/// process configuration (`kill_on_drop`, stdin, …).
pub async fn run_capture(
    cmd: &mut tokio::process::Command,
    timeout: Option<std::time::Duration>,
) -> Result<Vec<u8>, String> {
    let fut = cmd.no_console_window().output();
    let out = match timeout {
        Some(t) => tokio::time::timeout(t, fut)
            .await
            .map_err(|_| "timed out".to_string())?,
        None => fut.await,
    }
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("exit {:?}", out.status.code())
        } else {
            stderr
        });
    }
    Ok(out.stdout)
}

/// Program + arguments that run `command` through the platform command
/// interpreter — the same thing the user would get by typing it into
/// their default shell.
///
/// * Unix: `sh -c <command>`
/// * Windows: `%COMSPEC% /C <command>` (cmd.exe)
pub fn run_via_shell(command: &str) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        // Delayed expansion off whatever the registry says, so a quoted `!`
        // stays literal (see `ShellFlavor::Cmd`).
        (
            shell,
            vec!["/V:OFF".to_string(), "/C".to_string(), command.to_string()],
        )
    }
    #[cfg(not(windows))]
    {
        (
            "sh".to_string(),
            vec!["-c".to_string(), command.to_string()],
        )
    }
}

/// The interpreter [`run_via_shell`] uses on a given OS, for quoting
/// values into a command line that will run there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFlavor {
    /// POSIX `sh`, which is dash on Debian-likes: no `$'…'`.
    Posix,
    /// `cmd.exe /V:OFF /C`.
    Cmd,
}

impl ShellFlavor {
    pub fn of(style: crate::vfs::PathStyle) -> Self {
        match style {
            crate::vfs::PathStyle::Unix => ShellFlavor::Posix,
            crate::vfs::PathStyle::Windows => ShellFlavor::Cmd,
        }
    }

    /// `value` as a single word for this interpreter, its characters
    /// taken literally. `Err` for what can't be expressed: a line break
    /// for cmd.exe, which ends the command there.
    pub fn quote(self, value: &str) -> Result<String, String> {
        match self {
            ShellFlavor::Posix => Ok(posix_quote(value)),
            ShellFlavor::Cmd => cmd_quote(value),
        }
    }
}

fn posix_quote(value: &str) -> String {
    let inert = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !value.is_empty() && value.chars().all(inert) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// cmd.exe has no escape that works inside double quotes, so `%` (which
/// it expands there) and `"` (which ends them) are written between
/// quoted runs with a caret: `"50"^%" off"`. A `%"…"^%` pair that could
/// still look like a variable names one containing quotes, which can't be
/// set. Backslashes before a quote are doubled for the program's own
/// argv parsing (`CommandLineToArgvW`).
fn cmd_quote(value: &str) -> Result<String, String> {
    let inert = |c: char| c.is_ascii_alphanumeric() || "_@+=:,./\\-".contains(c);
    if !value.is_empty() && value.chars().all(inert) {
        return Ok(value.to_string());
    }
    if value.contains(['\r', '\n']) {
        return Err(format!(
            "{value:?} has a line break, which cmd.exe can't take"
        ));
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in value.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push(c);
                continue;
            }
            '%' => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push_str("\"^%\"");
            }
            '"' => {
                // `\"` for argv, its quote caret-escaped so cmd doesn't
                // toggle on it.
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push_str("\"\\^\"\"");
            }
            _ => out.push(c),
        }
        backslashes = 0;
    }
    out.extend(std::iter::repeat_n('\\', backslashes));
    out.push('"');
    Ok(out)
}

/// Look up `name` first in the inherited `PATH`, then in `extra_path` as a
/// fallback. Returns the bare `name` (so the eventual `Command::spawn`
/// fails with a clean not-found error) if nothing matches.
pub fn resolve_program(name: &str, extra_path: &[String]) -> std::path::PathBuf {
    if std::path::Path::new(name).is_absolute() || name.contains('/') {
        return std::path::PathBuf::from(name);
    }

    if let Some(found) = search_path_env(name) {
        return found;
    }

    for dir in extra_path {
        let expanded = expand_tilde(dir);
        let candidate = std::path::Path::new(&expanded).join(name);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }

    std::path::PathBuf::from(name)
}

fn search_path_env(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        let mut out = std::path::PathBuf::from(home);
        out.push(rest);
        return out.to_string_lossy().into_owned();
    }
    s.to_string()
}

#[cfg(unix)]
fn is_executable_file(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => m.permissions().mode() & 0o111 != 0,
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(p: &std::path::Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_passes_through() {
        let p = resolve_program("/bin/sh", &[]);
        assert_eq!(p, std::path::PathBuf::from("/bin/sh"));
    }

    #[test]
    fn finds_shell_on_path() {
        // A program universally on PATH for the platform's test env.
        #[cfg(unix)]
        let name = "sh";
        #[cfg(windows)]
        let name = "cmd.exe";
        let p = resolve_program(name, &[]);
        assert!(p.is_absolute(), "expected absolute path, got {p:?}");
    }

    #[test]
    fn extra_path_fallback() {
        // No match anywhere — should return the bare name.
        let p = resolve_program("definitely-not-installed-xyzzy", &["/tmp".into()]);
        assert_eq!(
            p,
            std::path::PathBuf::from("definitely-not-installed-xyzzy")
        );
    }

    #[test]
    fn tilde_expands() {
        if let Some(home) = std::env::var_os("HOME") {
            let out = expand_tilde("~/foo");
            let expected: std::path::PathBuf = [
                std::path::PathBuf::from(home),
                std::path::PathBuf::from("foo"),
            ]
            .iter()
            .collect();
            assert_eq!(out, expected.to_string_lossy());
        }
    }

    #[cfg(unix)]
    #[test]
    fn posix_quoting_survives_sh() {
        for value in [
            "plain.txt",
            "my file.txt",
            "x'; echo INJECTED #",
            "$(id)`id`\\n\"q\"",
            "",
            "~home",
            "line\nbreak",
            "ünï cødé",
        ] {
            let quoted = ShellFlavor::Posix.quote(value).unwrap();
            let out = std::process::Command::new("sh")
                .args(["-c", &format!("printf %s {quoted}")])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), value, "{quoted}");
        }
    }

    #[test]
    fn cmd_quoting() {
        let q = |v| ShellFlavor::Cmd.quote(v).unwrap();
        assert_eq!(q("C:\\dir\\file.txt"), "C:\\dir\\file.txt");
        assert_eq!(q("a&calc&.txt"), "\"a&calc&.txt\"");
        assert_eq!(q("50% off"), "\"50\"^%\" off\"");
        assert_eq!(q("say \"hi\""), "\"say \"\\^\"\"hi\"\\^\"\"\"");
        assert_eq!(q("C:\\my dir\\"), "\"C:\\my dir\\\\\"");
        assert_eq!(q(""), "\"\"");
        assert!(ShellFlavor::Cmd.quote("a\nb").is_err());
    }

    #[test]
    fn run_via_shell_shape() {
        let (prog, args) = run_via_shell("echo hi");
        #[cfg(windows)]
        {
            assert!(prog.to_ascii_lowercase().contains("cmd"));
            assert_eq!(args, ["/V:OFF", "/C", "echo hi"]);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(prog, "sh");
            assert_eq!(args, vec!["-c".to_string(), "echo hi".to_string()]);
        }
    }
}
