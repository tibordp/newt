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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathStyle {
    Unix,
    Windows,
}

/// Versioned wrapper so a mixed-version agent/host can't silently
/// misread the bytes (a future field/variant bumps `version`).
#[derive(Serialize, Deserialize)]
struct MountMetaV1 {
    version: u8,
    style: PathStyle,
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

    /// Encode for `mount_meta`.
    pub fn encode(self) -> Vec<u8> {
        bincode::serialize(&MountMetaV1 {
            version: 1,
            style: self,
        })
        .unwrap_or_default()
    }

    /// Decode from `mount_meta`. Empty / legacy / unparseable ⇒ `Unix`:
    /// every remote in scope is Unix, and a Unix render of a Unix path is
    /// correct, so this is the safe default.
    pub fn from_mount_meta(meta: &[u8]) -> PathStyle {
        if meta.is_empty() {
            return PathStyle::Unix;
        }
        bincode::deserialize::<MountMetaV1>(meta)
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
}
