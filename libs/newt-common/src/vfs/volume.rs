//! Volume classification: what kind of storage a filesystem root (or the
//! volume containing a given path) actually is — physical disk, removable
//! media, mapped network drive, subst alias, RAM disk — plus the volume
//! label and filesystem name where obtainable.
//!
//! Two consumers: `local_roots()` stamps a `VolumeInfo` per drive into
//! `mount_meta` at mount time (surfaced in the VFS selector), and
//! `fs_stats` probes the volume containing the listed directory (surfaced
//! in the pane footer). Probing always happens on the side that owns the
//! filesystem; the result crosses RPC as plain data.

use serde::{Deserialize, Serialize};

use super::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub enum VolumeKind {
    /// Physical disk / partition (or anything local we can't prove is
    /// something more specific).
    Fixed,
    Removable,
    Optical,
    /// Network filesystem: mapped drive on Windows, nfs/cifs/sshfs/… on Unix.
    Network,
    RamDisk,
    /// Windows `subst` drive — a letter aliasing a local directory.
    Substituted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct VolumeInfo {
    pub kind: VolumeKind,
    /// Filesystem name (NTFS, ext4, apfs, …).
    pub fs_type: Option<String>,
    /// Volume label, when the platform records one.
    pub label: Option<String>,
    /// Where the volume actually points: `\\server\share` for a mapped
    /// network drive, the aliased directory for a subst drive, the mount
    /// source (`server:/export`) for a Unix network mount.
    pub target: Option<String>,
    /// Root of this volume within the owning VFS, in wire path form: the
    /// drive/share root on Windows, the mount point on Unix (`/proc`, not
    /// `/`, when that's what the stats describe).
    pub mount_point: Option<String>,
}

/// A filesystem root paired with its volume classification. `roots()` /
/// `mount_meta` currency; the classification is `None` where probing
/// failed or doesn't apply (unified Unix root, non-local VFSes).
#[derive(Debug, Clone, PartialEq)]
pub struct RootInfo {
    pub path: PathBuf,
    pub volume: Option<VolumeInfo>,
}

impl RootInfo {
    pub fn bare(path: PathBuf) -> Self {
        Self { path, volume: None }
    }

    pub fn root() -> Self {
        Self::bare(PathBuf::root())
    }
}

// ---------------------------------------------------------------------------
// Windows probe
// ---------------------------------------------------------------------------

/// Classify the volume containing `path` (native path). Returns `None`
/// when the path has no classifiable prefix (relative, `Volume{GUID}`).
///
/// The volume root is derived from the path's structured prefix, which
/// handles verbatim `\\?\` forms (what `to_native` produces) uniformly.
/// `GetVolumePathNameW` is deliberately avoided — it rejects subst'd
/// drive roots with `ERROR_INVALID_PARAMETER`. Caveat: a volume mounted
/// on a folder reports the containing drive's volume, not the nested one.
#[cfg(windows)]
pub fn probe_native(path: &std::path::Path) -> Option<VolumeInfo> {
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetVolumeInformationW, QueryDosDeviceW,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };

    let Component::Prefix(prefix) = path.components().next()? else {
        return None;
    };
    // The root (`C:\`, `\\server\share\`) for the volume APIs, and the
    // DOS device (`C:`) for the subst / mapped-drive lookups.
    let (root, device) = match prefix.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
            let letter = char::from(d).to_ascii_uppercase();
            (format!("{letter}:\\"), Some(format!("{letter}:")))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => (
            format!(
                "\\\\{}\\{}\\",
                server.to_string_lossy(),
                share.to_string_lossy()
            ),
            None,
        ),
        _ => return None,
    };
    let root_wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: `root_wide` is NUL-terminated.
    let mut kind = match unsafe { GetDriveTypeW(root_wide.as_ptr()) } {
        DRIVE_REMOVABLE => VolumeKind::Removable,
        DRIVE_FIXED => VolumeKind::Fixed,
        DRIVE_REMOTE => VolumeKind::Network,
        DRIVE_CDROM => VolumeKind::Optical,
        DRIVE_RAMDISK => VolumeKind::RamDisk,
        _ => VolumeKind::Unknown,
    };

    let mut target = None;

    if let Some(device) = device {
        // A `\??\`-prefixed DOS-device target marks a subst alias; a
        // mapped network drive resolves to its UNC path.
        let device_wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buf = [0u16; 1024];
        // SAFETY: `device_wide` is NUL-terminated, `buf` is writable.
        let written =
            unsafe { QueryDosDeviceW(device_wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if written > 0 {
            let first = buf.iter().position(|&c| c == 0).unwrap_or(0);
            let dos_target = String::from_utf16_lossy(&buf[..first]);
            if let Some(aliased) = dos_target.strip_prefix(r"\??\") {
                kind = VolumeKind::Substituted;
                // A subst can itself point at a UNC path (`\??\UNC\…`).
                target = Some(match aliased.strip_prefix(r"UNC\") {
                    Some(unc) => format!(r"\\{unc}"),
                    None => aliased.to_string(),
                });
            }
        }

        if kind == VolumeKind::Network {
            target = wnet_connection(&device_wide);
        }
    } else if kind == VolumeKind::Network {
        // Direct UNC root: the path itself names the share.
        target = Some(root.trim_end_matches('\\').to_string());
    }

    // Label + filesystem name. Skipped for network drives: a disconnected
    // mapped drive can block here for seconds, and drive enumeration runs
    // on the session-launch path — the UNC target is the useful display
    // anyway.
    let mut label = None;
    let mut fs_type = None;
    if kind != VolumeKind::Network {
        let mut name = [0u16; 261];
        let mut fs_name = [0u16; 261];
        // SAFETY: buffers are writable with the stated lengths; the
        // ignored out-params (serial, component length, flags) accept null.
        let ok = unsafe {
            GetVolumeInformationW(
                root_wide.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok != 0 {
            label = utf16_field(&name);
            fs_type = utf16_field(&fs_name);
        }
    }

    Some(VolumeInfo {
        kind,
        fs_type,
        label,
        target,
        mount_point: Some(
            super::local::local_path_from_native(std::path::Path::new(&root))
                .as_wire_str()
                .to_string(),
        ),
    })
}

/// UNC path a mapped drive letter points at, via MPR's local state (no
/// network touch). `device` is the NUL-terminated wide `X:`.
#[cfg(windows)]
fn wnet_connection(device: &[u16]) -> Option<String> {
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;

    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `device` is NUL-terminated, `buf`/`len` describe a writable buffer.
    let ret = unsafe { WNetGetConnectionW(device.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if ret != 0 {
        return None;
    }
    utf16_field(&buf)
}

#[cfg(windows)]
fn utf16_field(buf: &[u16]) -> Option<String> {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    if len == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

// ---------------------------------------------------------------------------
// Linux probe
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub fn probe_native(path: &std::path::Path) -> Option<VolumeInfo> {
    let content = std::fs::read_to_string("/proc/self/mountinfo").ok()?;

    // Longest mount point that is a prefix of `path` = the containing mount.
    let mut best: Option<(String, String, String)> = None; // (mount_point, fs_type, source)
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(sep) = fields.iter().position(|&f| f == "-") else {
            continue;
        };
        if fields.len() < 5 || fields.len() < sep + 3 {
            continue;
        }
        let mount_point = unescape_mountinfo(fields[4]);
        if !path.starts_with(std::path::Path::new(&mount_point)) {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(mp, _, _)| mount_point.len() >= mp.len())
        {
            best = Some((
                mount_point,
                fields[sep + 1].to_string(),
                unescape_mountinfo(fields[sep + 2]),
            ));
        }
    }
    let (mount_point, fs_type, source) = best?;

    const NETWORK_FS: &[&str] = &[
        "cifs",
        "smb3",
        "smbfs",
        "sshfs",
        "fuse.sshfs",
        "ceph",
        "9p",
        "davfs",
        "fuse.davfs2",
        "glusterfs",
        "afs",
    ];

    let (kind, target) = if fs_type.starts_with("nfs") || NETWORK_FS.contains(&fs_type.as_str()) {
        (VolumeKind::Network, Some(source.clone()))
    } else if fs_type == "tmpfs" || fs_type == "ramfs" {
        (VolumeKind::RamDisk, None)
    } else if fs_type == "iso9660" || fs_type == "udf" {
        (VolumeKind::Optical, None)
    } else if source.starts_with("/dev/") {
        if linux_removable(&source) {
            (VolumeKind::Removable, None)
        } else {
            (VolumeKind::Fixed, None)
        }
    } else {
        (VolumeKind::Unknown, None)
    };

    Some(VolumeInfo {
        kind,
        fs_type: Some(fs_type),
        label: linux_label(&source),
        target,
        mount_point: Some(
            super::local::local_path_from_native(std::path::Path::new(&mount_point))
                .as_wire_str()
                .to_string(),
        ),
    })
}

/// Whether the block device backing `source` reports itself removable.
/// `/sys/class/block/<partition>` resolves into the whole-device directory
/// (`…/block/sda/sda1`), so the parent holds the `removable` flag.
#[cfg(target_os = "linux")]
fn linux_removable(source: &str) -> bool {
    let Some(name) = std::fs::canonicalize(source)
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
    else {
        return false;
    };
    let Ok(sys) = std::fs::canonicalize(format!("/sys/class/block/{name}")) else {
        return false;
    };
    let device_dir = match sys.parent() {
        Some(parent) if parent.file_name().is_some_and(|n| n != "block") => parent.to_path_buf(),
        _ => sys,
    };
    std::fs::read_to_string(device_dir.join("removable"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Volume label via the udev `/dev/disk/by-label` symlinks.
#[cfg(target_os = "linux")]
fn linux_label(source: &str) -> Option<String> {
    let canon = std::fs::canonicalize(source).ok()?;
    for entry in std::fs::read_dir("/dev/disk/by-label").ok()?.flatten() {
        if std::fs::canonicalize(entry.path()).is_ok_and(|p| p == canon) {
            return Some(unescape_udev(&entry.file_name().to_string_lossy()));
        }
    }
    None
}

/// Undo udev's `\xNN` escaping in by-label names.
#[cfg(target_os = "linux")]
fn unescape_udev(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'x') {
            chars.next();
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
                continue;
            }
            result.push_str("\\x");
            result.push_str(&hex);
        } else {
            result.push(c);
        }
    }
    result
}

/// Unescape octal escapes in mountinfo fields (e.g. `\040` → space).
#[cfg(target_os = "linux")]
pub(crate) fn unescape_mountinfo(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let oct: String = chars.by_ref().take(3).collect();
            if let Ok(byte) = u8::from_str_radix(&oct, 8) {
                result.push(byte as char);
            } else {
                result.push('\\');
                result.push_str(&oct);
            }
        } else {
            result.push(c);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// macOS probe
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub fn probe_native(path: &std::path::Path) -> Option<VolumeInfo> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated, `st` is a valid out-param.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut st) } != 0 {
        return None;
    }

    let fs_type = cchar_field(&st.f_fstypename);
    let from = cchar_field(&st.f_mntfromname);
    let on = cchar_field(&st.f_mntonname);
    let local = st.f_flags & (libc::MNT_LOCAL as u32) != 0;

    let kind = if !local {
        VolumeKind::Network
    } else {
        match fs_type.as_deref() {
            Some("cd9660") | Some("udf") => VolumeKind::Optical,
            Some("tmpfs") => VolumeKind::RamDisk,
            _ => VolumeKind::Fixed,
        }
    };

    // Finder-visible volume name: the mount-point basename under /Volumes.
    let label = on
        .as_deref()
        .and_then(|on| on.strip_prefix("/Volumes/"))
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
        .map(|rest| rest.to_string());

    Some(VolumeInfo {
        kind,
        fs_type,
        label,
        target: if kind == VolumeKind::Network {
            from
        } else {
            None
        },
        mount_point: on.as_deref().map(|on| {
            super::local::local_path_from_native(std::path::Path::new(on))
                .as_wire_str()
                .to_string()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Machine-independent smoke: wherever the tests run from is a real
    /// volume, so it must classify to *something*.
    #[test]
    fn probe_current_dir() {
        let dir = std::env::current_dir().unwrap();
        let info = probe_native(&dir).expect("current dir should classify");
        println!("current dir volume: {info:?}");
        assert_ne!(info.kind, VolumeKind::Optical);
    }
}

#[cfg(target_os = "macos")]
fn cchar_field(buf: &[libc::c_char]) -> Option<String> {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    if bytes.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}
