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

use super::path::PathBuf;
use super::volume::{RootInfo, VolumeInfo};

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
/// once at mount/launch time on the side that owns the FS — a single
/// `["/"]` for Unix, one per drive/share on Windows. A drive added after
/// launch needs a restart to appear; the deliberate tradeoff for keeping
/// this a descriptor-only lookup with no per-call RPC.
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

#[cfg(test)]
mod tests {
    use super::*;

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
