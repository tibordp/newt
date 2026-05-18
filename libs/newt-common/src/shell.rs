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
