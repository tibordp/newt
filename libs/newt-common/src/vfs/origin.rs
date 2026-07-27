//! Display helpers for origin-derived mounts — VFSes opened from an
//! entry in an upstream VFS (archives, disc images). The origin is
//! rendered through the upstream descriptor so the derived mount keeps
//! the upstream's path style, label, and breadcrumb segmentation.

use serde::{Deserialize, Serialize};

use super::path::{Path, PathBuf};
use super::{Breadcrumb, DisplayPathMatch, Vfs, VfsPath};

/// `mount_meta` of a VFS derived from an entry in an upstream VFS
/// (archives, disc images). The origin is rendered by the *upstream* VFS's
/// descriptor at mount time, so the archive inherits the upstream's path
/// style — a ZIP on a Windows drive shows `C:\…\x.zip`, not a mangled
/// `/C:\…`. Runtime-only (regenerated per mount); no versioning needed.
#[derive(Serialize, Deserialize)]
struct OriginMeta {
    /// Origin via the upstream descriptor's `format_path`.
    display: String,
    /// Origin breadcrumb *labels* from the upstream descriptor — already
    /// styled and segmented in the upstream's convention. Empty only for
    /// legacy/raw bytes, where breadcrumbs fall back to `/`-splitting.
    origin_crumbs: Vec<String>,
    /// The upstream's display separator (`\` on a Windows drive, `/`
    /// elsewhere). Used so the archive interior and the origin↔archive
    /// boundary render in the upstream's style too — *display only*;
    /// `nav_path`s stay canonical `/`.
    sep: char,
}

impl Default for OriginMeta {
    fn default() -> Self {
        Self {
            display: String::new(),
            origin_crumbs: Vec::new(),
            sep: '/',
        }
    }
}

fn decode_meta(mount_meta: &[u8]) -> OriginMeta {
    bincode::deserialize(mount_meta).unwrap_or_else(|_| OriginMeta {
        display: String::from_utf8_lossy(mount_meta).into_owned(),
        ..OriginMeta::default()
    })
}

/// The upstream's display separator, read off an intermediate origin
/// breadcrumb (an upstream non-final crumb label ends with it — `Users\`,
/// `u/`). Defaults to `/`.
fn upstream_sep(origin_crumbs: &[String]) -> char {
    origin_crumbs
        .get(origin_crumbs.len().wrapping_sub(2))
        .and_then(|c| c.chars().last())
        .filter(|c| *c == '/' || *c == '\\')
        .unwrap_or('/')
}

/// Build the `mount_meta` blob + display path for a VFS derived from an
/// entry in an upstream VFS. The origin is rendered through the *upstream*
/// descriptor so the derived mount keeps the upstream's path style (see
/// [`OriginMeta`]). Shared by archive and disc-image mounts.
pub(super) fn build_origin_meta(upstream: &dyn Vfs, origin: &VfsPath) -> (Vec<u8>, String) {
    let upstream_desc = upstream.descriptor();
    let upstream_meta = upstream.mount_meta();
    let display_path = upstream_desc.format_path(&origin.path, &upstream_meta);
    // Capture the upstream's *own* breadcrumb segmentation/styling for
    // the origin so the derived mount renders it in the upstream's
    // convention instead of assuming Unix `/`.
    let origin_crumbs: Vec<String> = upstream_desc
        .breadcrumbs(&origin.path, &upstream_meta)
        .into_iter()
        .map(|b| b.label)
        .collect();
    let sep = upstream_sep(&origin_crumbs);
    let mount_meta = bincode::serialize(&OriginMeta {
        display: display_path.clone(),
        origin_crumbs,
        sep,
    })
    .unwrap_or_default();
    (mount_meta, display_path)
}

pub(super) fn origin_format_path(path: &Path, mount_meta: &[u8]) -> String {
    let meta = decode_meta(mount_meta);
    if path.is_root() {
        meta.display
    } else {
        let sep = meta.sep.to_string();
        let inner: Vec<&str> = path.components().collect();
        format!("{}{sep}{}", meta.display, inner.join(&sep))
    }
}

pub(super) fn origin_breadcrumbs(path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
    let OriginMeta {
        display,
        origin_crumbs,
        sep,
    } = decode_meta(mount_meta);

    // Inner archive path segments (no leading slash, no empties).
    let segments: Vec<&str> = path.components().collect();

    // Origin crumb labels in the *upstream's* style (`C:\`, `Users\`, …
    // or `/`, `home/`, …). Legacy/raw bytes have none → Unix-split the
    // display string and prepend a `/` root, the historical behaviour.
    let origin_labels: Vec<String> = if origin_crumbs.is_empty() {
        std::iter::once("/".to_string())
            .chain(
                display
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            )
            .collect()
    } else {
        origin_crumbs
    };

    let n = origin_labels.len();
    let mut crumbs = Vec::with_capacity(n + segments.len());
    // Origin crumbs navigate via `..` to escape back into the parent VFS;
    // the archive-file crumb (last origin) is the archive root.
    for (i, label) in origin_labels.iter().enumerate() {
        let depth_from_root = n - 1 - i;
        let nav_path = if depth_from_root == 0 {
            "/".to_string()
        } else {
            let mut p = String::from("/");
            for _ in 0..depth_from_root {
                p.push_str("../");
            }
            p.pop();
            p
        };
        // Non-last origin labels already carry the upstream separator;
        // the archive-file label is bare, so append the archive's `/`
        // when inner segments follow it.
        let is_archive_file = i == n - 1;
        let crumb_label = if is_archive_file && !segments.is_empty() {
            format!("{label}{sep}")
        } else {
            label.clone()
        };
        crumbs.push(Breadcrumb {
            label: crumb_label,
            nav_path,
        });
    }

    // Inner archive path segments.
    let mut accumulated = String::new();
    for (i, seg) in segments.iter().enumerate() {
        accumulated.push('/');
        accumulated.push_str(seg);
        crumbs.push(Breadcrumb {
            label: if i == segments.len() - 1 {
                seg.to_string()
            } else {
                format!("{}{sep}", seg)
            },
            nav_path: accumulated.clone(),
        });
    }

    crumbs
}

pub(super) fn origin_try_parse_display_path(
    input: &str,
    mount_meta: &[u8],
) -> Option<DisplayPathMatch> {
    let display = decode_meta(mount_meta).display;
    if input == display {
        return Some(DisplayPathMatch::exact(PathBuf::root()));
    }
    let rest = input.strip_prefix(&display)?;
    let rest = rest.strip_prefix('/')?;
    Some(DisplayPathMatch::exact(PathBuf::from_wire_str(rest)))
}

pub(super) fn origin_mount_label(mount_meta: &[u8]) -> Option<String> {
    let display = decode_meta(mount_meta).display;
    (!display.is_empty()).then_some(display)
}

#[cfg(test)]
mod display_tests {
    use super::*;

    fn meta(display: &str, crumbs: &[&str], sep: char) -> Vec<u8> {
        bincode::serialize(&OriginMeta {
            display: display.to_string(),
            origin_crumbs: crumbs.iter().map(|s| s.to_string()).collect(),
            sep,
        })
        .unwrap()
    }

    #[test]
    fn windows_origin_renders_backslash_throughout() {
        let m = meta(
            r"C:\Users\Tibor\Downloads\hello.zip",
            &[r"C:\", r"Users\", r"Tibor\", r"Downloads\", "hello.zip"],
            '\\',
        );
        let p = PathBuf::from_wire_str("/sub/file");
        assert_eq!(
            origin_format_path(&p, &m),
            r"C:\Users\Tibor\Downloads\hello.zip\sub\file"
        );
        let joined: String = origin_breadcrumbs(&p, &m)
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(joined, r"C:\Users\Tibor\Downloads\hello.zip\sub\file");
        // Archive root: the origin verbatim, no trailing separator.
        assert_eq!(
            origin_format_path(&PathBuf::root(), &m),
            r"C:\Users\Tibor\Downloads\hello.zip"
        );
    }

    #[test]
    fn unix_origin_unchanged() {
        let m = meta("/home/u/x.zip", &["/", "home/", "u/", "x.zip"], '/');
        let p = PathBuf::from_wire_str("/a/b");
        assert_eq!(origin_format_path(&p, &m), "/home/u/x.zip/a/b");
        let joined: String = origin_breadcrumbs(&p, &m)
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(joined, "/home/u/x.zip/a/b");
    }
}
