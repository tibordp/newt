//! Conversions at the `vfs::path` ↔ `std::path` boundary.
//!
//! Native (`std::path`) forms must only ever exist in the process that
//! physically owns the filesystem — the host locally, the agent in a
//! remote session, each compiled for its own platform. Nothing here may
//! cross the RPC boundary.

use std::path::{Path as StdPath, PathBuf as StdPathBuf};

use super::path::{Path, PathBuf};

/// Render a LocalVfs path into a host-native `std::path::PathBuf` safe to
/// feed to `std::fs` / `opener` / any Win32 or POSIX consumer.
///
/// * Unix: `/foo/bar` → `/foo/bar`.
/// * Windows: `/?/C:/Users/Tibor` → `\\?\C:\Users\Tibor`;
///   `/?/UNC/server/share/foo` → `\\?\UNC\server\share\foo`.
pub fn to_native(path: &Path) -> StdPathBuf {
    #[cfg(windows)]
    {
        let comps: Vec<&str> = path.components().collect();
        let mut s = String::from(r"\\");
        for (i, c) in comps.iter().enumerate() {
            if i > 0 {
                s.push('\\');
            }
            s.push_str(c);
        }
        // `\\?\C:` / `\\?\UNC\server\share` name the *volume*, not its
        // root directory — `std::fs` and the change watcher reject those.
        // The root dir needs a trailing separator (`\\?\C:\`). Deeper
        // paths must NOT have one.
        if comps.first() == Some(&"?") {
            let root_depth = if comps.get(1) == Some(&"UNC") { 4 } else { 2 };
            if comps.len() == root_depth {
                s.push('\\');
            }
        }
        StdPathBuf::from(s)
    }
    #[cfg(not(windows))]
    {
        StdPathBuf::from(path.as_wire_str())
    }
}

/// Native path suitable as a **spawned process's working directory**.
///
/// [`to_native`] returns the verbatim (`\\?\…`) form, which `std::fs` and
/// the change watcher need but which `cmd.exe` chokes on: it reads
/// `\\?\C:\…` as a UNC path, refuses to `cd` there, and silently starts
/// in `%SystemRoot%` instead — so local terminals would open in the wrong
/// directory. Strip the verbatim prefix so an ordinary local directory
/// becomes a plain `C:\…` the shell accepts. Genuine network locations
/// are intentionally left as conventional UNC (`\\server\share\…`) so the
/// shell shows its own "UNC paths are not supported" notice rather than us
/// masking it. Over-long paths (> `MAX_PATH`) keep the verbatim form,
/// since stripping it wouldn't help `cmd` anyway and other shells can
/// still use it.
///
/// On non-Windows this is just [`to_native`].
pub fn launch_cwd(path: &Path) -> StdPathBuf {
    #[cfg(windows)]
    {
        const MAX_PATH: usize = 260;
        let s = to_native(path).to_string_lossy().into_owned();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            // Verbatim UNC → conventional UNC; cmd will (rightly) warn.
            StdPathBuf::from(format!(r"\\{rest}"))
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            if rest.len() <= MAX_PATH {
                StdPathBuf::from(rest)
            } else {
                StdPathBuf::from(s)
            }
        } else {
            StdPathBuf::from(s)
        }
    }
    #[cfg(not(windows))]
    {
        to_native(path)
    }
}

/// Decode a host-native `std::path::Path` into LocalVfs path components.
///
/// * Unix: walks `Normal` components — `/home/user` → `["home", "user"]`.
/// * Windows: emits the `"?"` sentinel then drive (`["?", "C:", …]`) or
///   UNC (`["?", "UNC", "server", "share", …]`) info, then `Normal`
///   components. Verbatim (`\\?\…`) and conventional forms collapse to
///   the same components.
///
/// Used at the boundary between native-path APIs
/// (`std::env::current_dir`, `dirs::*`, drag-and-drop) and `VfsPath`.
pub fn local_path_from_native(path: &StdPath) -> PathBuf {
    PathBuf::from_components(local_segments_from_native(path))
}

/// [`local_path_from_native`] for *user-typed* input: the same decoding,
/// behind vetting the native decode doesn't do. Only distinctively
/// absolute Windows shapes are claimed — `X:` + separator/end (drive) or
/// a leading `\\` (UNC/verbatim). Drive-relative (`C:foo`) and plain
/// relative inputs return `None` instead of being silently absolutized,
/// as do `..` segments (the native decode drops them, which is only
/// sound for canonicalized paths) and the non-filesystem `\\?\`/`\\.\`
/// namespaces beyond verbatim drive/UNC forms.
///
/// Windows-hosted by construction: the caller gates on a Windows-styled
/// `mount_meta`, which for a client-local mount implies this process
/// *is* the Windows side — so `std::path` parses the syntax natively.
/// Compiled to `None` elsewhere rather than let `std::path` misread the
/// input as a single relative component.
#[cfg(windows)]
pub fn local_path_from_typed_display(input: &str) -> Option<PathBuf> {
    let (s, verbatim) = match input.strip_prefix(r"\\?\") {
        Some(rest) => (rest, true),
        None => (input, false),
    };
    let b = s.as_bytes();
    let drive = b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/');
    if !drive {
        // UNC: require server + share, and reject the `?`/`.` device
        // namespaces (`//?/…` is not parsed as verbatim by std::path).
        let rest = if verbatim {
            s.strip_prefix("UNC\\").or_else(|| s.strip_prefix("UNC/"))?
        } else {
            s.strip_prefix(r"\\").or_else(|| s.strip_prefix("//"))?
        };
        let mut parts = rest.split(['/', '\\']).filter(|p| !p.is_empty());
        let server = parts.next()?;
        parts.next()?;
        if server == "?" || server == "." {
            return None;
        }
    }
    if s.split(['/', '\\']).any(|seg| seg == "..") {
        return None;
    }
    Some(local_path_from_native(StdPath::new(input)))
}

#[cfg(not(windows))]
pub fn local_path_from_typed_display(_input: &str) -> Option<PathBuf> {
    None
}

pub fn local_segments_from_native(path: &StdPath) -> Vec<String> {
    use std::path::Component;

    let mut segments = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => segments.push(s.to_string_lossy().into_owned()),
            Component::Prefix(_prefix) => {
                #[cfg(windows)]
                {
                    use std::path::Prefix;
                    segments.push("?".to_string());
                    match _prefix.kind() {
                        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                            // `d` is the drive letter byte (e.g. `b'C'`).
                            segments.push(format!("{}:", char::from(d).to_ascii_uppercase()));
                        }
                        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                            segments.push("UNC".to_string());
                            segments.push(server.to_string_lossy().into_owned());
                            segments.push(share.to_string_lossy().into_owned());
                        }
                        Prefix::Verbatim(seg) => {
                            // `\\?\<seg>\…` for forms we don't specifically
                            // recognise (e.g. `Volume{GUID}`). Pass through
                            // verbatim so volume-GUID paths still work.
                            segments.push(seg.to_string_lossy().into_owned());
                        }
                        Prefix::DeviceNS(_) => {
                            // `\\.\…` device-namespace paths — outside the
                            // file-manager scope. Drop the prefix and hope
                            // the remaining segments are usable.
                        }
                    }
                }
            }
            // Drop `RootDir`, `CurDir`, `ParentDir`. Absolute paths land at
            // `RootDir` after the prefix (if any); `.` and `..` shouldn't
            // appear in a canonicalised path the caller hands us, and if
            // they do we drop them since segments are meant to be literal.
            _ => {}
        }
    }
    segments
}

#[cfg(all(test, windows))]
mod typed_display_tests {
    use super::*;

    fn p(comps: &[&str]) -> PathBuf {
        PathBuf::from_components(comps.iter().copied())
    }

    #[test]
    fn typed_display_accepts_distinctive_absolute_shapes() {
        for (input, comps) in [
            (r"C:\Users\X", &["?", "C:", "Users", "X"][..]),
            ("c:/x", &["?", "C:", "x"][..]),
            ("C:", &["?", "C:"][..]),
            (r"C:\", &["?", "C:"][..]),
            (r"\\server\share", &["?", "UNC", "server", "share"][..]),
            (
                r"\\server\share\a",
                &["?", "UNC", "server", "share", "a"][..],
            ),
            (
                "//server/share/a",
                &["?", "UNC", "server", "share", "a"][..],
            ),
            (r"\\?\C:\x", &["?", "C:", "x"][..]),
            (
                r"\\?\UNC\server\share\a",
                &["?", "UNC", "server", "share", "a"][..],
            ),
        ] {
            assert_eq!(
                local_path_from_typed_display(input).as_ref(),
                Some(&p(comps)),
                "{input}"
            );
        }
    }

    #[test]
    fn typed_display_rejects_relative_and_exotic_shapes() {
        for input in [
            "foo",
            r"foo\bar",
            "C:foo",   // drive-relative — no per-drive cwd to resolve against
            "/unix/x", // unix absolute stays with the session shell
            r"\\server",
            r"\\.\COM1",
            "//?/C:/x",
            r"C:\a\..\b", // native decode would drop the `..`, not resolve it
        ] {
            assert_eq!(local_path_from_typed_display(input), None, "{input}");
        }
    }
}
