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
        .then(|| crate::vfs::native::local_path_from_native(p))
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
        (shell, vec!["/C".to_string(), command.to_string()])
    }
    #[cfg(not(windows))]
    {
        (
            "sh".to_string(),
            vec!["-c".to_string(), command.to_string()],
        )
    }
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

    #[test]
    fn run_via_shell_shape() {
        let (prog, args) = run_via_shell("echo hi");
        #[cfg(windows)]
        {
            assert!(prog.to_ascii_lowercase().contains("cmd"));
            assert_eq!(args, vec!["/C".to_string(), "echo hi".to_string()]);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(prog, "sh");
            assert_eq!(args, vec!["-c".to_string(), "echo hi".to_string()]);
        }
    }
}
