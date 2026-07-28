//! Path syntax of a mounted filesystem, carried in `mount_meta`.
//!
//! A VFS path is platform-independent (always `/`-separated, Windows
//! drive/UNC encoded behind the `?` sentinel). But *rendering* one for
//! the user — and deciding what "up" means — depends on whether the
//! filesystem it lives on is Unix- or Windows-shaped. That is a property
//! of the *remote end*, not of the host this process was compiled for: a
//! Windows host browsing a Linux box over SSH must render Unix paths, and
//! a Linux host exposing its FS into a session on a Windows client the
//! reverse.
//!
//! So the producer of a `Local`/`Remote` mount stamps the style into
//! `mount_meta`, and `LocalVfsDescriptor` / `RemoteVfsDescriptor` read it
//! back per call instead of branching on `cfg!(windows)`. Other VFS types
//! (S3, archive, …) keep their own `mount_meta` meaning — only these two
//! descriptors interpret it as a `PathStyle`.

use serde::{Deserialize, Serialize};

use super::path::{Path, PathBuf};
use super::volume::{RootInfo, VolumeInfo};
use super::{Breadcrumb, MetadataTraits};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathStyle {
    Unix,
    Windows,
}

/// No version field: the agent is built from the same sources as the
/// host and bootstrapped per session by content hash, and `mount_meta`
/// is runtime-only (never persisted), so there is no version drift to
/// guard against — only platform-shape differences, which is exactly
/// what `style`/`roots` carry.
///
/// `roots` are the filesystem's root paths (VFS wire strings), captured
/// at mount/launch time on the side that owns the FS — a single `["/"]`
/// for Unix, one per drive/share on Windows. Kept a descriptor-only
/// lookup (no per-call RPC); drive changes refresh the whole meta via
/// `VfsManager::remount`.
#[derive(Serialize, Deserialize)]
struct MountMeta {
    style: PathStyle,
    roots: Vec<RootMeta>,
    /// Human-readable mount target (agent mounts: the container name,
    /// host, or pod). `None` for style-only metas.
    label: Option<String>,
    /// Transport kind shown as the VFS display name (agent mounts:
    /// "Docker", "SSH", …). `None` for style-only metas.
    kind: Option<String>,
}

/// Wire form of one FS root: the path (VFS wire string) plus its volume
/// classification, probed on the owning side at mount time.
#[derive(Serialize, Deserialize)]
struct RootMeta {
    path: String,
    volume: Option<VolumeInfo>,
}

/// Encode `mount_meta` carrying both the path style and the FS roots.
pub fn encode_mount_meta(style: PathStyle, roots: &[RootInfo]) -> Vec<u8> {
    encode_mount_meta_labeled(style, roots, None, None)
}

/// `encode_mount_meta`, plus display strings (see `MountMeta::kind` /
/// `MountMeta::label`).
pub fn encode_mount_meta_labeled(
    style: PathStyle,
    roots: &[RootInfo],
    kind: Option<&str>,
    label: Option<&str>,
) -> Vec<u8> {
    bincode::serialize(&MountMeta {
        style,
        roots: roots
            .iter()
            .map(|r| RootMeta {
                path: r.path.as_wire_str().to_string(),
                volume: r.volume.clone(),
            })
            .collect(),
        label: label.map(|l| l.to_string()),
        kind: kind.map(|k| k.to_string()),
    })
    .unwrap_or_default()
}

/// The display label from `mount_meta`, if one was recorded.
pub fn mount_meta_label(meta: &[u8]) -> Option<String> {
    bincode::deserialize::<MountMeta>(meta)
        .ok()
        .and_then(|m| m.label)
}

/// The transport-kind display name from `mount_meta`, if one was recorded.
pub fn mount_meta_kind(meta: &[u8]) -> Option<String> {
    bincode::deserialize::<MountMeta>(meta)
        .ok()
        .and_then(|m| m.kind)
}

/// The FS roots from `mount_meta`. Empty when none were recorded
/// (legacy / style-only mount) — callers fall back to a single `/`.
pub fn mount_roots(meta: &[u8]) -> Vec<PathBuf> {
    mount_root_infos(meta).into_iter().map(|r| r.path).collect()
}

/// The FS roots with their volume classification. Empty when none were
/// recorded (style-only mount) — callers fall back to a single `/`.
pub fn mount_root_infos(meta: &[u8]) -> Vec<RootInfo> {
    if meta.is_empty() {
        return Vec::new();
    }
    bincode::deserialize::<MountMeta>(meta)
        .map(|m| {
            m.roots
                .into_iter()
                .map(|r| RootInfo {
                    path: PathBuf::from_wire_str(&r.path),
                    volume: r.volume,
                })
                .collect()
        })
        .unwrap_or_default()
}

impl PathStyle {
    /// The style of the OS this process was compiled for. Use **only**
    /// where the filesystem genuinely is this process's own (a real
    /// local session, or the client-local FS exposed back into a remote
    /// session) — never to describe the far end of a connection.
    pub fn host() -> PathStyle {
        if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Unix
        }
    }

    /// Characters that separate segments in *typed* path input for this
    /// style, and that mark an input as root-relative when leading.
    ///
    /// `\` is a separator only on Windows. On a Unix filesystem it is an
    /// ordinary filename character — a directory may legitimately be
    /// called `\`, and navigating to it must enter it rather than jump to
    /// the root. So the set is a property of the *session's* filesystem,
    /// not of the host the UI happens to run on.
    pub fn separators(self) -> &'static [char] {
        match self {
            PathStyle::Unix => &['/'],
            PathStyle::Windows => &['/', '\\'],
        }
    }

    /// Encode for `mount_meta` with no recorded roots (style-only — e.g.
    /// a remote Unix root, where the single `/` is implied).
    pub fn encode(self) -> Vec<u8> {
        encode_mount_meta(self, &[])
    }

    /// Decode from `mount_meta`. Empty / legacy / unparseable ⇒ `Unix`:
    /// every remote in scope is Unix, and a Unix render of a Unix path is
    /// correct, so this is the safe default.
    pub fn from_mount_meta(meta: &[u8]) -> PathStyle {
        if meta.is_empty() {
            return PathStyle::Unix;
        }
        bincode::deserialize::<MountMeta>(meta)
            .map(|m| m.style)
            .unwrap_or(PathStyle::Unix)
    }
}

// ---------------------------------------------------------------------------
// Style-driven rendering: display paths, breadcrumbs, parents. Shared by
// every descriptor whose paths are host-shaped (`Local`/`Remote`/`Agent`)
// or plain Unix-shaped (SFTP, S3, archives, …).
// ---------------------------------------------------------------------------

/// `/`-rooted display string for a segment list. Used by the descriptors
/// of every Unix-path-speaking VFS (SFTP, S3, archives, …) in their
/// `format_path` impls.
pub fn unix_display_path(path: &Path) -> String {
    path.as_wire_str().to_string()
}

/// Breadcrumb list for a Unix-style path. Each breadcrumb's `nav_path`
/// is the corresponding prefix as a `/`-rooted string.
pub fn unix_breadcrumbs(path: &Path) -> Vec<Breadcrumb> {
    let comps: Vec<&str> = path.components().collect();
    let mut crumbs = Vec::with_capacity(comps.len() + 1);
    crumbs.push(Breadcrumb {
        label: "/".to_string(),
        nav_path: "/".to_string(),
    });
    let mut accumulated = String::new();
    for (i, seg) in comps.iter().enumerate() {
        accumulated.push('/');
        accumulated.push_str(seg);
        let is_last = i == comps.len() - 1;
        crumbs.push(Breadcrumb {
            label: if is_last {
                (*seg).to_string()
            } else {
                format!("{}/", seg)
            },
            nav_path: accumulated.clone(),
        });
    }
    crumbs
}

/// Render host-shaped path components into the conventional display form.
///
/// * Unix: `["Users", "tibor"]` → `/Users/tibor`.
/// * Windows: strips the `"?"` sentinel — `["?", "C:", "Users", "Tibor"]`
///   → `C:\Users\Tibor`; `["?", "UNC", "server", "share", "foo"]` →
///   `\\server\share\foo`.
fn comps_display(comps: &[&str], style: PathStyle) -> String {
    match style {
        PathStyle::Unix => {
            if comps.is_empty() {
                String::from("/")
            } else {
                format!("/{}", comps.join("/"))
            }
        }
        PathStyle::Windows => {
            // `[]` and `["?"]` both correspond to the "above any
            // drive/share" position (`\\?\`). Navigation rules normally
            // prevent landing here — see `navigable_parent` — but render
            // something defensively rather than panic.
            if comps.is_empty() || (comps.len() == 1 && comps[0] == "?") {
                return String::from(r"\\?\");
            }
            match comps[0] {
                "?" => {
                    if comps.len() >= 2 && comps[1] == "UNC" {
                        let mut s = String::from(r"\\");
                        s.push_str(&comps[2..].join(r"\"));
                        s
                    } else {
                        let mut s = comps[1].to_string();
                        if comps.len() > 2 {
                            s.push('\\');
                            s.push_str(&comps[2..].join(r"\"));
                        } else {
                            // Bare drive root: `C:\`, not `C:`.
                            s.push('\\');
                        }
                        s
                    }
                }
                // Defensive fallback for non-sentinel components.
                _ => comps.join(r"\"),
            }
        }
    }
}

/// User-facing rendering of a host-shaped path. See [`comps_display`].
pub fn local_display_path(path: &Path, style: PathStyle) -> String {
    let comps: Vec<&str> = path.components().collect();
    comps_display(&comps, style)
}

/// Breadcrumbs for a host-shaped path. Each breadcrumb's `nav_path` is the
/// display form of the path up to that segment, suitable for the
/// path-input dialog.
pub fn local_breadcrumbs(path: &Path, style: PathStyle) -> Vec<Breadcrumb> {
    if style == PathStyle::Unix {
        return unix_breadcrumbs(path);
    }
    let comps: Vec<&str> = path.components().collect();
    if comps.first().copied() != Some("?") {
        // Defensive — render unstructured components Unix-style.
        return unix_breadcrumbs(path);
    }
    let mut crumbs = Vec::new();
    // Root depth: 2 (`?/C:`) for drives, 4 (`?/UNC/server/share`)
    // for UNC. The root crumb covers through that point.
    let root_depth = if comps.get(1).copied() == Some("UNC") {
        4
    } else {
        2
    };
    if comps.len() < root_depth {
        crumbs.push(Breadcrumb {
            label: comps_display(&comps, style),
            nav_path: comps_display(&comps, style),
        });
        return crumbs;
    }
    // The root crumb's display (`C:\`, `\\server\share`) is the
    // conventional form. When deeper segments follow, the *label* (the
    // concatenation unit) must end in a separator so the next segment
    // doesn't fuse onto it — `C:\` already does, `\\server\share` does
    // not. `nav_path` stays the conventional form regardless.
    let root_disp = comps_display(&comps[..root_depth], style);
    let root_label = if comps.len() > root_depth && !root_disp.ends_with('\\') {
        format!("{root_disp}\\")
    } else {
        root_disp.clone()
    };
    crumbs.push(Breadcrumb {
        label: root_label,
        nav_path: root_disp,
    });
    for i in root_depth..comps.len() {
        let is_last = i + 1 == comps.len();
        let label = if is_last {
            comps[i].to_string()
        } else {
            format!("{}\\", comps[i])
        };
        crumbs.push(Breadcrumb {
            label,
            nav_path: comps_display(&comps[..i + 1], style),
        });
    }
    crumbs
}

/// Logical parent of a host-shaped path, honouring Windows drive/share
/// roots. Shared by `LocalVfsDescriptor` and `RemoteVfsDescriptor` (the
/// path shape is identical; only the `mount_meta`-derived style differs).
pub fn navigable_parent(path: &Path, style: PathStyle) -> Option<PathBuf> {
    match style {
        PathStyle::Unix => path.parent().map(Path::to_owned),
        PathStyle::Windows => {
            // `/?`, `/?/C:`, and `/?/UNC/server/share` are all "roots".
            // Anything above them isn't a navigable location in our
            // current model (no "This PC" view, no "shares on server"
            // view), so refuse to go up past them.
            let comps: Vec<&str> = path.components().collect();
            // A sentinel-less path isn't a real Windows path; treat it
            // like Unix rather than misapplying drive-root rules.
            if comps.first().copied() != Some("?") {
                return path.parent().map(Path::to_owned);
            }
            let root_depth = match comps.get(1).copied() {
                Some("UNC") => 4,
                Some(_) => 2,
                None => return None,
            };
            if comps.len() <= root_depth {
                None
            } else {
                Some(PathBuf::from_components(
                    comps[..comps.len() - 1].iter().copied(),
                ))
            }
        }
    }
}

/// `MetadataTraits` for a `Local`/`Remote`/`Agent` mount, decided by the
/// path style recorded in `mount_meta`.
pub fn metadata_traits_from_meta(mount_meta: &[u8]) -> MetadataTraits {
    match PathStyle::from_mount_meta(mount_meta) {
        PathStyle::Unix => MetadataTraits {
            unix_owner: true,
            windows_attributes: false,
        },
        PathStyle::Windows => MetadataTraits {
            unix_owner: false,
            windows_attributes: true,
        },
    }
}

/// FS roots from `mount_meta`, defaulting to a single `/` when none were
/// recorded (a style-only mount, e.g. a remote Unix root). Shared by
/// `LocalVfsDescriptor` and `RemoteVfsDescriptor`.
pub fn roots_from_meta(mount_meta: &[u8]) -> Vec<RootInfo> {
    let roots = mount_root_infos(mount_meta);
    if roots.is_empty() {
        vec![RootInfo::root()]
    } else {
        roots
    }
}

/// Whether a `Local`/`Remote` mount presents one unified `/` root.
///
/// This is a property of the *path style*, **not** the number of roots: a
/// Windows filesystem with only a `C:` drive is still split-root — its
/// root is `C:\`, never `/`. Keying off `roots().len() == 1` (the trait
/// default) silently misclassifies a single-drive Windows box as unified,
/// so the VFS selector offers one "Local" entry pointing at the
/// unlistable `\\?\` sentinel instead of the drive. Decide by style.
pub fn unified_root_from_meta(mount_meta: &[u8]) -> bool {
    PathStyle::from_mount_meta(mount_meta) == PathStyle::Unix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_windows_treats_backslash_as_a_separator() {
        assert_eq!(PathStyle::Unix.separators(), &['/']);
        assert_eq!(PathStyle::Windows.separators(), &['/', '\\']);
        // A Unix directory really can be called `\`.
        assert_eq!(
            r"a\b"
                .split(PathStyle::Unix.separators())
                .collect::<Vec<_>>(),
            vec![r"a\b"]
        );
        assert_eq!(
            r"a\b"
                .split(PathStyle::Windows.separators())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn round_trips() {
        for s in [PathStyle::Unix, PathStyle::Windows] {
            assert_eq!(PathStyle::from_mount_meta(&s.encode()), s);
        }
    }

    #[test]
    fn empty_and_garbage_default_to_unix() {
        assert_eq!(PathStyle::from_mount_meta(&[]), PathStyle::Unix);
        assert_eq!(
            PathStyle::from_mount_meta(&[0xff, 0x00, 0x42]),
            PathStyle::Unix
        );
    }

    #[test]
    fn roots_round_trip() {
        use super::super::volume::{VolumeInfo, VolumeKind};

        let roots = [
            RootInfo {
                path: PathBuf::from_wire_str("/?/C:"),
                volume: Some(VolumeInfo {
                    kind: VolumeKind::Fixed,
                    fs_type: Some("NTFS".into()),
                    label: Some("Data".into()),
                    target: None,
                    mount_point: Some("/?/C:".into()),
                }),
            },
            RootInfo::bare(PathBuf::from_wire_str("/?/D:")),
        ];
        let meta = encode_mount_meta(PathStyle::Windows, &roots);
        assert_eq!(PathStyle::from_mount_meta(&meta), PathStyle::Windows);
        assert_eq!(
            mount_roots(&meta),
            roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>()
        );
        assert_eq!(mount_root_infos(&meta), roots);
        // Style-only / empty / garbage → no roots recorded.
        assert!(mount_roots(&PathStyle::Unix.encode()).is_empty());
        assert!(mount_roots(&[]).is_empty());
        assert!(mount_roots(&[0xff, 0x00]).is_empty());
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::vfs::path::PathBuf as VfsPathBuf;

    fn p(comps: &[&str]) -> VfsPathBuf {
        VfsPathBuf::from_components(comps.iter().copied())
    }

    #[test]
    fn roots_render_conventionally() {
        // Drives keep their trailing separator (`C:\`); a UNC share root
        // does not (`\\server\share`) — matches Explorer and every other
        // final-location display.
        assert_eq!(comps_display(&["?", "C:"], PathStyle::Windows), r"C:\");
        assert_eq!(
            comps_display(&["?", "UNC", "localhost", "Users"], PathStyle::Windows),
            r"\\localhost\Users"
        );
        assert_eq!(
            comps_display(
                &["?", "UNC", "localhost", "Users", "Public"],
                PathStyle::Windows
            ),
            r"\\localhost\Users\Public"
        );
    }

    #[test]
    fn share_root_breadcrumb_has_no_trailing_slash() {
        let crumbs = local_breadcrumbs(&p(&["?", "UNC", "localhost", "Users"]), PathStyle::Windows);
        assert_eq!(crumbs.len(), 1);
        assert_eq!(crumbs[0].label, r"\\localhost\Users");
    }

    #[test]
    fn breadcrumbs_concatenate_without_fusing() {
        for (comps, expected) in [
            (
                &["?", "UNC", "localhost", "Users", "Public"][..],
                r"\\localhost\Users\Public",
            ),
            (&["?", "C:", "Users", "Public"][..], r"C:\Users\Public"),
        ] {
            let crumbs = local_breadcrumbs(&p(comps), PathStyle::Windows);
            let joined: String = crumbs.iter().map(|c| c.label.as_str()).collect();
            assert_eq!(joined, expected);
        }
    }
}

/// What an absolute path fragment resolves against. On Windows a leading
/// `\` is drive-relative, so it has to land on the current drive's root
/// rather than the filesystem's abstract (and unlistable) `\\?\` root.
#[cfg(test)]
mod root_of_tests {
    use crate::vfs::path::PathBuf;
    use crate::vfs::{LOCAL_VFS_DESCRIPTOR, PathStyle, VfsDescriptor};

    fn root_of(path: &str, style: PathStyle) -> String {
        let meta = style.encode();
        LOCAL_VFS_DESCRIPTOR
            .root_of(&PathBuf::from_wire_str(path), &meta)
            .as_wire_str()
            .to_string()
    }

    #[test]
    fn a_unified_root_filesystem_resolves_to_slash() {
        assert_eq!(root_of("/home/tibor/src", PathStyle::Unix), "/");
        assert_eq!(root_of("/home", PathStyle::Unix), "/");
        assert_eq!(root_of("/", PathStyle::Unix), "/");
    }

    #[test]
    fn a_windows_path_resolves_to_its_drive_root() {
        assert_eq!(
            root_of("/?/C:/Users/Tibor/src", PathStyle::Windows),
            "/?/C:"
        );
        assert_eq!(root_of("/?/D:/games", PathStyle::Windows), "/?/D:");
        // Already at the drive root.
        assert_eq!(root_of("/?/C:", PathStyle::Windows), "/?/C:");
    }

    #[test]
    fn a_unc_path_resolves_to_its_share_root() {
        assert_eq!(
            root_of("/?/UNC/server/share/deep/dir", PathStyle::Windows),
            "/?/UNC/server/share"
        );
        assert_eq!(
            root_of("/?/UNC/server/share", PathStyle::Windows),
            "/?/UNC/server/share"
        );
    }

    /// The `\\?\` position itself is a root: there is no "This PC" view to
    /// go up to, and `navigable_parent` refuses to leave it.
    #[test]
    fn the_windows_sentinel_root_is_its_own_root() {
        assert_eq!(root_of("/?", PathStyle::Windows), "/?");
        assert_eq!(root_of("/", PathStyle::Windows), "/");
    }

    /// Whatever `..` reaches by repetition, `root_of` reaches in one step —
    /// the two are derived from the same `navigable_parent`, and this pins
    /// that they cannot drift.
    #[test]
    fn agrees_with_walking_up_by_hand() {
        for (path, style) in [
            ("/?/C:/Users/Tibor/src/newt", PathStyle::Windows),
            ("/?/UNC/server/share/a/b", PathStyle::Windows),
            ("/home/tibor/src/newt", PathStyle::Unix),
        ] {
            let meta = style.encode();
            let mut walked = PathBuf::from_wire_str(path);
            while let Some(parent) = LOCAL_VFS_DESCRIPTOR.navigable_parent(&walked, &meta) {
                walked = parent;
            }
            assert_eq!(walked.as_wire_str(), root_of(path, style), "{path}");
        }
    }
}
