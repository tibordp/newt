//! Volume classification: what kind of storage a filesystem root (or the
//! volume containing a given path) actually is — physical disk, removable
//! media, mapped network drive, subst alias, RAM disk — plus the volume
//! label and filesystem name where obtainable.
//!
//! Two consumers: `local_roots()` classifies each drive it lists
//! (`Vfs::roots`, surfaced in the VFS selector), and `fs_stats` probes the
//! volume containing the listed directory (surfaced in the pane footer). Probing always happens on the side that owns the
//! filesystem; the result crosses RPC as plain data.

use serde::{Deserialize, Serialize};

use newt_common::path::PathBuf;

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
    /// A disk image the OS mounted: a DMG, a loop-mounted ISO or image.
    DiskImage,
    /// A phone or a camera, over MTP or PTP.
    Device,
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

/// A filesystem root paired with its volume classification, as
/// `Vfs::roots` answers; the classification is `None` where probing
/// failed or doesn't apply (unified Unix root, non-local VFSes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            newt_common::path::PathBuf::from_native(std::path::Path::new(&root))
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

/// One decoded line of `/proc/self/mountinfo`. Format (space-separated):
/// `id parent_id major:minor root mount_point options … - fs_type source
/// super_options`, with octal escapes in the path fields.
#[cfg(target_os = "linux")]
pub(crate) struct MountInfoEntry {
    pub(crate) mount_point: String,
    pub(crate) fs_type: String,
    pub(crate) source: String,
}

/// The process's mount table, decoded. Empty when `/proc` is unavailable.
#[cfg(target_os = "linux")]
pub(crate) fn read_mountinfo() -> Vec<MountInfoEntry> {
    let Ok(content) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let sep = fields.iter().position(|&f| f == "-")?;
            if fields.len() < 5 || fields.len() < sep + 3 {
                return None;
            }
            Some(MountInfoEntry {
                mount_point: unescape_mountinfo(fields[4]),
                fs_type: fields[sep + 1].to_string(),
                source: unescape_mountinfo(fields[sep + 2]),
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
pub fn probe_native(path: &std::path::Path) -> Option<VolumeInfo> {
    // Longest mount point that is a prefix of `path` = the containing mount.
    let mut best: Option<MountInfoEntry> = None;
    for entry in read_mountinfo() {
        if !path.starts_with(std::path::Path::new(&entry.mount_point)) {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|b| entry.mount_point.len() >= b.mount_point.len())
        {
            best = Some(entry);
        }
    }
    let best = best?;
    // Under the gvfs FUSE bridge, the mount is the per-location directory.
    if best.fs_type == GVFS_FUSE {
        let mount_point = std::path::Path::new(&best.mount_point);
        let name = path.strip_prefix(mount_point).ok()?.components().next()?;
        return Some(gvfs_volume(
            &name.as_os_str().to_string_lossy(),
            &mount_point.join(name),
        ));
    }
    Some(classify(&best))
}

/// Filesystem type of the gvfs FUSE bridge, mounted at
/// `/run/user/$UID/gvfs` with one directory per GNOME-mounted location.
#[cfg(target_os = "linux")]
pub(crate) const GVFS_FUSE: &str = "fuse.gvfsd-fuse";

/// Whether a mount of this filesystem type reaches over the network.
#[cfg(target_os = "linux")]
pub(crate) fn is_network_fs(fs_type: &str) -> bool {
    const NETWORK_FS: &[&str] = &[
        "cifs",
        "smb3",
        "smbfs",
        "sshfs",
        "fuse.sshfs",
        "fuse.rclone",
        "fuse.kio-fuse",
        "ceph",
        "9p",
        "davfs",
        "fuse.davfs2",
        "glusterfs",
        "afs",
    ];
    fs_type.starts_with("nfs") || NETWORK_FS.contains(&fs_type)
}

/// Classify a mount-table entry.
#[cfg(target_os = "linux")]
pub(crate) fn classify(entry: &MountInfoEntry) -> VolumeInfo {
    let MountInfoEntry {
        mount_point,
        fs_type,
        source,
    } = entry;
    let (kind, target) = if is_network_fs(fs_type) {
        (VolumeKind::Network, Some(source.clone()))
    } else if fs_type == "tmpfs" || fs_type == "ramfs" {
        (VolumeKind::RamDisk, None)
    } else if source.starts_with("/dev/loop") {
        (VolumeKind::DiskImage, None)
    } else if fs_type == "iso9660" || fs_type == "udf" {
        (VolumeKind::Optical, None)
    } else if source.starts_with("/dev/") {
        if linux_removable(source) {
            (VolumeKind::Removable, None)
        } else {
            (VolumeKind::Fixed, None)
        }
    } else {
        (VolumeKind::Unknown, None)
    };

    VolumeInfo {
        kind,
        fs_type: Some(fs_type.clone()),
        label: linux_label(source),
        target,
        mount_point: Some(
            newt_common::path::PathBuf::from_native(std::path::Path::new(mount_point))
                .as_wire_str()
                .to_string(),
        ),
    }
}

/// A location the gvfs FUSE bridge exposes, from its directory name:
/// `smb-share:server=nas,share=media`, `sftp:host=example.com,user=me`,
/// `mtp:host=Google_Pixel_7_…`. Values are percent-escaped.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn gvfs_volume(name: &str, mount_point: &std::path::Path) -> VolumeInfo {
    let (scheme, params) = name.split_once(':').unwrap_or((name, ""));
    let param = |key: &str| {
        params.split(',').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| percent_decode(v))
        })
    };
    let host = param("host").or_else(|| param("server"));
    let (kind, label, target) = match scheme {
        "mtp" | "gphoto2" | "afc" => (VolumeKind::Device, host.map(|h| h.replace('_', " ")), None),
        "smb-share" => {
            let share = param("share");
            let label = match (&share, &host) {
                (Some(share), Some(host)) => Some(format!("{share} on {host}")),
                _ => share.clone().or_else(|| host.clone()),
            };
            let target = host.map(|h| format!("smb://{h}/{}", share.unwrap_or_default()));
            (VolumeKind::Network, label, target)
        }
        "smb-server" | "sftp" | "ftp" | "ftps" | "dav" | "davs" | "afp-volume" | "nfs"
        | "google-drive" | "onedrive" => {
            let target = host.as_ref().map(|h| {
                let user = param("user").map(|u| format!("{u}@")).unwrap_or_default();
                let base = scheme
                    .trim_end_matches("-server")
                    .trim_end_matches("-volume");
                format!("{base}://{user}{h}")
            });
            (VolumeKind::Network, host, target)
        }
        _ => (VolumeKind::Unknown, None, None),
    };
    VolumeInfo {
        kind,
        fs_type: Some("gvfs".to_string()),
        label: Some(label.unwrap_or_else(|| name.to_string())),
        target,
        mount_point: Some(
            newt_common::path::PathBuf::from_native(mount_point)
                .as_wire_str()
                .to_string(),
        ),
    }
}

/// Undo `%XX` escaping; anything malformed is kept as it is.
#[cfg(any(target_os = "linux", test))]
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = s
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
fn unescape_mountinfo(s: &str) -> String {
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
    Some(classify(&st))
}

/// The volumes mounted under `/Volumes` that Finder would show, by mount
/// point. From the kernel's mount table (`getmntinfo`, `MNT_NOWAIT`), so a
/// stalled network share is listed without being contacted; `/Volumes`
/// itself also holds a link to the boot volume, which isn't a mount.
#[cfg(target_os = "macos")]
pub(crate) fn mounted_volumes() -> Vec<(std::path::PathBuf, VolumeInfo)> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: on success `mounts` points at `count` entries in a buffer the
    // C library owns and reuses on the next call; they are read before
    // returning.
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 || mounts.is_null() {
        return Vec::new();
    }
    let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };
    mounts
        .iter()
        .filter(|st| st.f_flags & (libc::MNT_DONTBROWSE as u32) == 0)
        .filter_map(|st| {
            let on = cchar_field(&st.f_mntonname)?;
            let name = on.strip_prefix("/Volumes/")?;
            (!name.is_empty() && !name.contains('/'))
                .then(|| (std::path::PathBuf::from(&on), classify(st)))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn classify(st: &libc::statfs) -> VolumeInfo {
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
            _ => from
                .as_deref()
                .and_then(|from| from.strip_prefix("/dev/"))
                .and_then(disk_arbitration::kind)
                .unwrap_or(VolumeKind::Fixed),
        }
    };

    // Finder-visible volume name: the mount-point basename under /Volumes.
    let label = on
        .as_deref()
        .and_then(|on| on.strip_prefix("/Volumes/"))
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
        .map(|rest| rest.to_string());

    VolumeInfo {
        kind,
        fs_type,
        label,
        target: if kind == VolumeKind::Network {
            from.as_deref().map(without_password)
        } else {
            None
        },
        mount_point: on.as_deref().map(|on| {
            newt_common::path::PathBuf::from_native(std::path::Path::new(on))
                .as_wire_str()
                .to_string()
        }),
    }
}

/// What DiskArbitration knows about a local disk that `statfs` doesn't:
/// whether it's a disk image, or external. Local IPC to `diskarbitrationd`;
/// no disk is touched.
#[cfg(target_os = "macos")]
mod disk_arbitration {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation_sys::base::{CFAllocatorRef, CFRelease, CFTypeRef, kCFAllocatorDefault};
    use core_foundation_sys::dictionary::CFDictionaryRef;

    use super::VolumeKind;

    #[link(name = "DiskArbitration", kind = "framework")]
    unsafe extern "C" {
        fn DASessionCreate(allocator: CFAllocatorRef) -> CFTypeRef;
        fn DADiskCreateFromBSDName(
            allocator: CFAllocatorRef,
            session: CFTypeRef,
            name: *const std::ffi::c_char,
        ) -> CFTypeRef;
        fn DADiskCopyDescription(disk: CFTypeRef) -> CFDictionaryRef;
        static kDADiskDescriptionDeviceProtocolKey: CFStringRef;
        static kDADiskDescriptionDeviceModelKey: CFStringRef;
        static kDADiskDescriptionDeviceInternalKey: CFStringRef;
    }

    /// `bsd_name` is the device without `/dev/` (`disk4s1`).
    pub(super) fn kind(bsd_name: &str) -> Option<VolumeKind> {
        let name = std::ffi::CString::new(bsd_name).ok()?;
        // SAFETY: each Create/Copy result is released once, below, or
        // handed to a wrapper that releases it; the key statics are
        // constants the framework owns.
        unsafe {
            let session = DASessionCreate(kCFAllocatorDefault);
            if session.is_null() {
                return None;
            }
            let disk = DADiskCreateFromBSDName(kCFAllocatorDefault, session, name.as_ptr());
            let description = if disk.is_null() {
                std::ptr::null()
            } else {
                DADiskCopyDescription(disk)
            };
            if !disk.is_null() {
                CFRelease(disk);
            }
            CFRelease(session);
            if description.is_null() {
                return None;
            }
            let description: CFDictionary<CFString, CFType> =
                CFDictionary::wrap_under_create_rule(description);
            let string = |key: CFStringRef| {
                description
                    .find(CFString::wrap_under_get_rule(key))
                    .and_then(|v| v.downcast::<CFString>())
                    .map(|v| v.to_string().trim().to_string())
            };
            let internal = description
                .find(CFString::wrap_under_get_rule(
                    kDADiskDescriptionDeviceInternalKey,
                ))
                .and_then(|v| v.downcast::<CFBoolean>())
                .map(bool::from);
            // An image reports its model as "Disk Image" (its protocol is
            // "Virtual Interface"); older releases put it in the protocol.
            let image = [
                kDADiskDescriptionDeviceModelKey,
                kDADiskDescriptionDeviceProtocolKey,
            ]
            .into_iter()
            .any(|key| string(key).as_deref() == Some("Disk Image"));
            Some(if image {
                VolumeKind::DiskImage
            } else if internal == Some(false) {
                VolumeKind::Removable
            } else {
                VolumeKind::Fixed
            })
        }
    }
}

/// A mount source (`//user:secret@server/share`) with any password in
/// its user part left out.
#[cfg(any(target_os = "macos", test))]
fn without_password(source: &str) -> String {
    let Some((user_part, rest)) = source.split_once('@') else {
        return source.to_string();
    };
    match user_part.split_once(':') {
        Some((user, _)) => format!("{user}@{rest}"),
        None => source.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mount_source_shows_no_password() {
        assert_eq!(
            without_password("//GUEST:@ARIADNE._smb._tcp.local/share"),
            "//GUEST@ARIADNE._smb._tcp.local/share"
        );
        assert_eq!(without_password("//me:secret@nas/share"), "//me@nas/share");
        assert_eq!(without_password("//nas/share"), "//nas/share");
    }

    #[test]
    fn gvfs_locations_are_labelled_by_scheme() {
        let at = std::path::Path::new("/run/user/1000/gvfs/x");
        let smb = gvfs_volume("smb-share:server=192.168.0.32,share=my%20share", at);
        assert_eq!(smb.kind, VolumeKind::Network);
        assert_eq!(smb.label.as_deref(), Some("my share on 192.168.0.32"));
        assert_eq!(smb.target.as_deref(), Some("smb://192.168.0.32/my share"));

        let sftp = gvfs_volume("sftp:host=nas.home.arpa,user=demo", at);
        assert_eq!(sftp.kind, VolumeKind::Network);
        assert_eq!(sftp.label.as_deref(), Some("nas.home.arpa"));
        assert_eq!(sftp.target.as_deref(), Some("sftp://demo@nas.home.arpa"));

        let phone = gvfs_volume("mtp:host=Google_Pixel_7", at);
        assert_eq!(phone.kind, VolumeKind::Device);
        assert_eq!(phone.label.as_deref(), Some("Google Pixel 7"));

        let unknown = gvfs_volume("archive:host=file%3A%2F%2F%2Ftmp%2Fa.zip", at);
        assert_eq!(unknown.kind, VolumeKind::Unknown);
        assert_eq!(
            unknown.label.as_deref(),
            Some("archive:host=file%3A%2F%2F%2Ftmp%2Fa.zip")
        );
    }

    /// Whatever is mounted on the machine running the tests: each volume
    /// sits right under `/Volumes`, and the boot volume (only a link
    /// there) isn't one of them.
    #[cfg(target_os = "macos")]
    #[test]
    fn mounted_volumes_are_under_volumes() {
        for (path, info) in mounted_volumes() {
            println!("{}: {info:?}", path.display());
            assert_eq!(path.parent(), Some(std::path::Path::new("/Volumes")));
            assert!(std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_symlink()));
        }
    }

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
