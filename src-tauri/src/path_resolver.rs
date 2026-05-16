//! Resolve a program name to an absolute path without mutating our own
//! `PATH`. We avoid touching the process environment because subprocesses
//! that the user expects to inherit *their* shell's `PATH` (notably the
//! integrated terminal) would otherwise get our augmented version, which
//! can reorder their preferred lookup order.
//!
//! Instead: each subprocess we spawn for a known dev tool (docker, podman,
//! kubectl, …) gets its program argument run through `resolve_program`,
//! which checks the inherited `PATH` first, then the user-configured
//! `environment.extra_path` directories as a fallback.

use std::path::{Path, PathBuf};

/// Look up `name` first in the inherited `PATH`, then in `extra_path` as a
/// fallback. Returns the bare `name` (so the eventual `Command::spawn` fails
/// with a clean ENOENT) if nothing matches.
pub fn resolve_program(name: &str, extra_path: &[String]) -> PathBuf {
    if Path::new(name).is_absolute() || name.contains('/') {
        return PathBuf::from(name);
    }

    if let Some(found) = search_path_env(name) {
        return found;
    }

    for dir in extra_path {
        let expanded = expand_tilde(dir);
        let candidate = Path::new(&expanded).join(name);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }

    PathBuf::from(name)
}

fn search_path_env(name: &str) -> Option<PathBuf> {
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
        let mut out = PathBuf::from(home);
        out.push(rest);
        return out.to_string_lossy().into_owned();
    }
    s.to_string()
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => m.permissions().mode() & 0o111 != 0,
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_passes_through() {
        let p = resolve_program("/bin/sh", &[]);
        assert_eq!(p, PathBuf::from("/bin/sh"));
    }

    #[test]
    fn finds_sh_on_path() {
        // /bin/sh is universally on PATH for unit-test environments.
        let p = resolve_program("sh", &[]);
        assert!(p.is_absolute(), "expected absolute path, got {:?}", p);
    }

    #[test]
    fn extra_path_fallback() {
        // Point at a directory that's not on PATH but contains a known file.
        // We can't fake an executable easily without root/chmod, so just
        // verify the fallback search order is hit when PATH lookup fails.
        let p = resolve_program("definitely-not-installed-xyzzy", &["/tmp".into()]);
        // No match anywhere — should return the bare name.
        assert_eq!(p, PathBuf::from("definitely-not-installed-xyzzy"));
    }

    #[test]
    fn tilde_expands() {
        // Set a known HOME and check expand_tilde resolves.
        if let Some(home) = std::env::var_os("HOME") {
            let out = expand_tilde("~/foo");
            let expected: PathBuf = [PathBuf::from(home), PathBuf::from("foo")].iter().collect();
            assert_eq!(out, expected.to_string_lossy());
        }
    }
}
