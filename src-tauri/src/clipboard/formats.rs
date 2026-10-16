//! The freedesktop file-clipboard formats, as text: `file://` URIs, URI
//! lists, and the GNOME/MATE copied-files payload.

use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode, percent_encode};

/// Bytes a path keeps as-is in a `file://` URI: RFC 3986's unreserved set,
/// and `/`.
const PATH: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'/');

/// `path` (absolute) as a `file://` URI.
pub fn file_uri(path: &Path) -> String {
    format!(
        "file://{}",
        percent_encode(path.as_os_str().as_bytes(), PATH)
    )
}

/// The local path a `file://` URI names; `None` for another scheme or a
/// host other than this one.
pub fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = match rest.strip_prefix("localhost") {
        Some(path) => path,
        None => rest,
    };
    if !path.starts_with('/') {
        return None;
    }
    let bytes: Vec<u8> = percent_decode(path.as_bytes()).collect();
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// A `text/uri-list` body: one URI per line, CRLF-terminated as RFC 2483
/// asks.
pub fn uri_list(uris: &[String]) -> String {
    uris.iter().map(|uri| format!("{uri}\r\n")).collect()
}

/// The URIs of a `text/uri-list` body, comments left out.
pub fn parse_uri_list(body: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(body)
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The body of `x-special/gnome-copied-files` (and MATE's twin): `copy` or
/// `cut`, then a URI per line.
pub fn copied_files(cut: bool, uris: &[String]) -> String {
    let mut body = String::from(if cut { "cut" } else { "copy" });
    for uri in uris {
        body.push('\n');
        body.push_str(uri);
    }
    body
}

/// The URIs of a copied-files body, and whether it is a cut; `None` unless
/// it starts with `copy` or `cut`, as Nautilus and Caja require.
pub fn parse_copied_files(body: &[u8]) -> Option<(Vec<String>, bool)> {
    let text = String::from_utf8_lossy(body);
    let mut lines = text.lines().map(|line| line.trim_end_matches('\r'));
    let cut = match lines.next()? {
        "cut" => true,
        "copy" => false,
        _ => return None,
    };
    let uris = lines
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Some((uris, cut))
}

/// The local paths among `uris`, in order.
pub fn local_paths(uris: &[String]) -> Vec<PathBuf> {
    uris.iter()
        .filter_map(|uri| path_from_file_uri(uri))
        .collect()
}

/// `paths` as `file://` URIs.
pub fn file_uris(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| file_uri(path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_round_trip_odd_names() {
        for name in ["/tmp/plain.txt", "/tmp/a b/c#d%e?f", "/tmp/naïve ü", "/"] {
            let path = PathBuf::from(name);
            let uri = file_uri(&path);
            assert!(uri.starts_with("file:///"), "{uri}");
            assert!(!uri[7..].contains(' ') && !uri.contains('#'), "{uri}");
            assert_eq!(path_from_file_uri(&uri), Some(path));
        }
        let raw = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe"));
        assert_eq!(path_from_file_uri(&file_uri(&raw)), Some(raw));
    }

    #[test]
    fn foreign_uris_are_not_paths() {
        assert_eq!(path_from_file_uri("sftp://host/etc/passwd"), None);
        assert_eq!(path_from_file_uri("file://other-host/etc"), None);
        assert_eq!(
            path_from_file_uri("file://localhost/etc/hosts"),
            Some(PathBuf::from("/etc/hosts"))
        );
    }

    #[test]
    fn copied_files_payloads_round_trip_and_reject_strangers() {
        let uris = vec!["file:///a".to_string(), "file:///b%20c".to_string()];
        for cut in [false, true] {
            let body = copied_files(cut, &uris);
            assert_eq!(
                parse_copied_files(body.as_bytes()),
                Some((uris.clone(), cut))
            );
        }
        assert_eq!(parse_copied_files(b"file:///a\nfile:///b"), None);
        assert_eq!(parse_copied_files(b""), None);
    }

    #[test]
    fn uri_lists_skip_comments_and_crlf() {
        let body = uri_list(&["file:///a".into(), "file:///b".into()]);
        assert_eq!(body, "file:///a\r\nfile:///b\r\n");
        assert_eq!(
            parse_uri_list(b"# from somewhere\r\nfile:///a\r\n\r\nfile:///b"),
            vec!["file:///a".to_string(), "file:///b".to_string()]
        );
    }
}
