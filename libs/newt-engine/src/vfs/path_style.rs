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

use newt_common::Error;

use super::{Breadcrumb, MetadataTraits};
use newt_common::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathStyle {
    Unix,
    Windows,
}

/// How a filesystem names a file's alternate data, if it keeps any: the
/// path `stream_path` builds for a stream. Like `PathStyle`, a property of
/// the side that owns the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NamedStreams {
    #[default]
    None,
    /// macOS: a file's resource fork at `file/..namedfork/rsrc`.
    ResourceFork,
    /// NTFS alternate data streams, `file:name:$DATA`.
    Ntfs,
}

impl NamedStreams {
    /// The convention of the OS this process was compiled for. Like
    /// `PathStyle::host`, only for a filesystem that is this process's own.
    pub fn host() -> NamedStreams {
        if cfg!(target_os = "macos") {
            NamedStreams::ResourceFork
        } else if cfg!(windows) {
            NamedStreams::Ntfs
        } else {
            NamedStreams::None
        }
    }
}

/// Where the stream `name` of the file at `path` lives, under `streams`.
/// `NotSupported` for a name the convention has no place for.
pub fn stream_path(path: &Path, name: &str, streams: NamedStreams) -> Result<PathBuf, Error> {
    match streams {
        NamedStreams::ResourceFork if name == "com.apple.ResourceFork" => {
            Ok(path.join("..namedfork").join("rsrc"))
        }
        NamedStreams::Ntfs
            if name.starts_with(':')
                && name.ends_with(":$DATA")
                && !name.contains(['/', '\\'])
                && name != "::$DATA" =>
        {
            Ok(PathBuf::from_wire_str(&format!(
                "{}{name}",
                path.as_wire_str()
            )))
        }
        _ => Err(Error::not_supported()),
    }
}

/// No version field: the agent is built from the same sources as the
/// host and bootstrapped per session by content hash, and `mount_meta`
/// is runtime-only (never persisted), so there is no version drift to
/// guard against — only platform-shape differences, which is exactly
/// what it carries. It holds what is fixed for the mount's lifetime;
/// anything that changes under it (a Windows filesystem's drives) is
/// asked of the filesystem when needed (`Vfs::roots`).
#[derive(Serialize, Deserialize)]
struct MountMeta {
    style: PathStyle,
    streams: NamedStreams,
    /// Human-readable mount target (agent mounts: the container name,
    /// host, or pod). `None` for style-only metas.
    label: Option<String>,
    /// Transport kind shown as the VFS display name (agent mounts:
    /// "Docker", "SSH", …). `None` for style-only metas.
    kind: Option<String>,
}

/// Encode `mount_meta` carrying the path style and the stream convention,
/// as the owner of the filesystem describes it.
pub fn encode_mount_meta(style: PathStyle, streams: NamedStreams) -> Vec<u8> {
    bincode::serialize(&MountMeta {
        style,
        streams,
        label: None,
        kind: None,
    })
    .expect("mount_meta encodes")
}

/// `meta`, as its owner reported it, with the display strings the mounting
/// side gives it (see `MountMeta::kind` / `MountMeta::label`).
pub fn relabel_mount_meta(meta: &[u8], kind: &str, label: &str) -> Vec<u8> {
    let mut meta: MountMeta = bincode::deserialize(meta).expect("mount_meta from the owner");
    meta.kind = Some(kind.to_string());
    meta.label = Some(label.to_string());
    bincode::serialize(&meta).expect("mount_meta encodes")
}

/// The stream convention from `mount_meta`; `None` for a style-only meta.
pub fn mount_named_streams(meta: &[u8]) -> NamedStreams {
    bincode::deserialize::<MountMeta>(meta)
        .map(|m| m.streams)
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

    /// Encode for `mount_meta`, style only.
    pub fn encode(self) -> Vec<u8> {
        encode_mount_meta(self, NamedStreams::None)
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
        note: None,
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
            note: None,
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

fn comps_wire(comps: &[&str]) -> String {
    format!("/{}", comps.join("/"))
}

/// Breadcrumbs for a host-shaped path. Each breadcrumb's `nav_path` is the
/// wire form of the path up to that segment (`/?/C:/Users`).
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
            nav_path: comps_wire(&comps),
            note: None,
        });
        return crumbs;
    }
    // The root crumb's display (`C:\`, `\\server\share`) is the
    // conventional form. When deeper segments follow, the *label* (the
    // concatenation unit) must end in a separator so the next segment
    // doesn't fuse onto it — `C:\` already does, `\\server\share` does
    // not.
    let root_disp = comps_display(&comps[..root_depth], style);
    let root_label = if comps.len() > root_depth && !root_disp.ends_with('\\') {
        format!("{root_disp}\\")
    } else {
        root_disp
    };
    crumbs.push(Breadcrumb {
        label: root_label,
        nav_path: comps_wire(&comps[..root_depth]),
        note: None,
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
            nav_path: comps_wire(&comps[..i + 1]),
            note: None,
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

/// Whether a `Local`/`Remote` mount presents one unified `/` root.
///
/// This is a property of the *path style*, **not** the number of roots: a
/// Windows filesystem with only a `C:` drive is still split-root — its
/// root is `C:\`, never `/` — so counting roots would offer a single-drive
/// box as one "Local" entry at the unlistable `\\?\` sentinel.
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
    fn stream_paths_follow_the_filesystems_convention() {
        let file = PathBuf::from_wire_str("/dir/file");
        let fork = "com.apple.ResourceFork";
        let ads = ":Zone.Identifier:$DATA";

        assert_eq!(
            stream_path(&file, fork, NamedStreams::ResourceFork).unwrap(),
            PathBuf::from_wire_str("/dir/file/..namedfork/rsrc")
        );
        assert_eq!(
            stream_path(&file, ads, NamedStreams::Ntfs).unwrap(),
            PathBuf::from_wire_str("/dir/file:Zone.Identifier:$DATA")
        );
        for (name, streams) in [
            (fork, NamedStreams::Ntfs),
            (ads, NamedStreams::ResourceFork),
            (fork, NamedStreams::None),
            ("::$DATA", NamedStreams::Ntfs),
            (":a/b:$DATA", NamedStreams::Ntfs),
        ] {
            let err = stream_path(&file, name, streams).unwrap_err();
            assert_eq!(
                err.kind,
                newt_common::ErrorKind::NotSupported,
                "{name} {streams:?}"
            );
        }
    }

    #[test]
    fn a_relabelled_meta_keeps_what_its_owner_said() {
        let owner = encode_mount_meta(PathStyle::Windows, NamedStreams::Ntfs);
        let meta = relabel_mount_meta(&owner, "SSH", "box");
        assert_eq!(PathStyle::from_mount_meta(&meta), PathStyle::Windows);
        assert_eq!(mount_named_streams(&meta), NamedStreams::Ntfs);
        assert_eq!(mount_meta_kind(&meta).as_deref(), Some("SSH"));
        assert_eq!(mount_meta_label(&meta).as_deref(), Some("box"));
        assert_eq!(
            mount_named_streams(&PathStyle::Unix.encode()),
            NamedStreams::None
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
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use newt_common::path::PathBuf as VfsPathBuf;

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
    use crate::vfs::{LOCAL_VFS_DESCRIPTOR, PathStyle, VfsDescriptor};
    use newt_common::path::PathBuf;

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
