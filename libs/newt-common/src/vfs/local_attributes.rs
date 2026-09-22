use super::attributes::{Attribute, AttributeKind, NamedStream};
use super::path::{Path, PathBuf};
use crate::Error;

pub(super) fn read(
    path: &std::path::Path,
    kind: AttributeKind,
) -> Result<Option<Attribute>, Error> {
    match kind {
        AttributeKind::Metadata | AttributeKind::Timestamps | AttributeKind::Permissions => {
            Ok(None)
        }
        AttributeKind::Owner { by_name: false } | AttributeKind::Group { by_name: false } => {
            #[cfg(windows)]
            {
                windows::read_owner(path, matches!(kind, AttributeKind::Group { .. })).map(Some)
            }
            #[cfg(not(windows))]
            {
                Ok(None)
            }
        }
        AttributeKind::Owner { by_name: true } | AttributeKind::Group { by_name: true } => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let meta = std::fs::symlink_metadata(path)?;
                let property = if matches!(kind, AttributeKind::Owner { .. }) {
                    Attribute::OwnerName(
                        nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(meta.uid()))?
                            .ok_or_else(|| Error::custom("source user has no account name"))?
                            .name,
                    )
                } else {
                    Attribute::GroupName(
                        nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(meta.gid()))?
                            .ok_or_else(|| Error::custom("source group has no account name"))?
                            .name,
                    )
                };
                Ok(Some(property))
            }
            #[cfg(not(unix))]
            {
                Err(Error::not_supported())
            }
        }

        AttributeKind::HardLinks => Ok(super::local::file_identity(path)?.map(|(volume, id)| {
            Attribute::Identity(
                [volume.to_le_bytes().as_slice(), id.to_le_bytes().as_slice()].concat(),
            )
        })),
        AttributeKind::Sparse => {
            let meta = std::fs::symlink_metadata(path)?;
            #[cfg(unix)]
            let sparse = {
                use std::os::unix::fs::MetadataExt;
                meta.is_file() && meta.blocks() * 512 < meta.len()
            };
            #[cfg(windows)]
            let sparse = {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes()
                    & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SPARSE_FILE
                    != 0
            };
            Ok(Some(Attribute::Sparse(sparse)))
        }

        AttributeKind::ObjectMetadata | AttributeKind::ObjectTags | AttributeKind::ObjectAccess => {
            Ok(None)
        }
        AttributeKind::Streams => streams(path).map(|s| Some(Attribute::Streams(s))),
        AttributeKind::AccessControl => acl_read(path).map(Some),
        AttributeKind::ExtendedAttributes => xattrs_read(path),
    }
}

pub(super) fn write(path: &std::path::Path, property: &Attribute) -> Result<(), Error> {
    match property {
        Attribute::NativeOwner {
            format,
            data,
            group,
        } => {
            #[cfg(windows)]
            {
                windows::write_owner(path, format, data, *group)
            }
            #[cfg(not(windows))]
            {
                let _ = (format, data, group);
                Err(Error::not_supported())
            }
        }
        Attribute::OwnerName(name) | Attribute::GroupName(name) => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                let (uid, gid) = if matches!(property, Attribute::OwnerName(_)) {
                    (
                        nix::unistd::User::from_name(name)?
                            .ok_or_else(|| {
                                Error::custom(format!("destination user {name} does not exist"))
                            })?
                            .uid
                            .as_raw(),
                        !0,
                    )
                } else {
                    (
                        !0,
                        nix::unistd::Group::from_name(name)?
                            .ok_or_else(|| {
                                Error::custom(format!("destination group {name} does not exist"))
                            })?
                            .gid
                            .as_raw(),
                    )
                };
                let native = std::ffi::CString::new(path.as_os_str().as_bytes())
                    .map_err(|e| Error::custom(e.to_string()))?;
                if unsafe { libc::lchown(native.as_ptr(), uid, gid) } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = name;
                Err(Error::not_supported())
            }
        }

        Attribute::ExtendedAttributes(attrs) => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                for attr in attrs {
                    xattr::set(path, std::ffi::OsStr::from_bytes(&attr.name), &attr.value)?;
                }
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = (path, attrs);
                Err(Error::not_supported())
            }
        }
        Attribute::AccessControl { format, data } => acl_write(path, format, data),
        _ => Err(Error::not_supported()),
    }
}

fn xattrs_read(path: &std::path::Path) -> Result<Option<Attribute>, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut attrs = Vec::new();
        for name in xattr::list(path)? {
            let bytes = name.as_os_str().as_bytes();
            // ACLs and resource forks have independent preservation controls.
            if bytes.starts_with(b"system.posix_acl_") || bytes == b"com.apple.ResourceFork" {
                continue;
            }
            let value = xattr::get(path, &name)?
                .ok_or_else(|| Error::custom("extended attribute disappeared during copy"))?;
            attrs.push(super::attributes::Xattr {
                name: bytes.to_vec(),
                value,
            });
        }
        Ok(Some(Attribute::ExtendedAttributes(attrs)))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(Error::not_supported())
    }
}

pub(super) fn stream_path(path: &Path, name: &str) -> Result<PathBuf, Error> {
    #[cfg(target_os = "macos")]
    if name == "com.apple.ResourceFork" {
        return Ok(path.join("..namedfork").join("rsrc"));
    }
    #[cfg(windows)]
    if name.starts_with(':')
        && name.ends_with(":$DATA")
        && !name.contains(['/', '\\'])
        && name != "::$DATA"
    {
        return Ok(PathBuf::from_wire_str(&format!(
            "{}{name}",
            path.as_wire_str()
        )));
    }
    let _ = (path, name);
    Err(Error::not_supported())
}

fn streams(path: &std::path::Path) -> Result<Vec<NamedStream>, Error> {
    #[cfg(target_os = "macos")]
    {
        if std::fs::symlink_metadata(path)?.is_symlink() {
            return Ok(Vec::new());
        }
        let fork = path.join("..namedfork/rsrc");
        match std::fs::metadata(&fork) {
            Ok(meta) if meta.len() > 0 => Ok(vec![NamedStream {
                name: "com.apple.ResourceFork".into(),
                path: PathBuf::from_native(&fork),
                size: meta.len(),
            }]),
            Ok(_) => Ok(Vec::new()),
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENOTDIR) =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(windows)]
    {
        windows::streams(path)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        Ok(Vec::new())
    }
}

#[cfg(target_os = "linux")]
fn acl_read(path: &std::path::Path) -> Result<Attribute, Error> {
    let mut attrs = Vec::new();
    for name in ["system.posix_acl_access", "system.posix_acl_default"] {
        if let Some(value) = xattr::get(path, name)? {
            attrs.push(super::attributes::Xattr {
                name: name.as_bytes().to_vec(),
                value,
            });
        }
    }
    Ok(Attribute::AccessControl {
        format: "linux.posix-acl".into(),
        data: bincode::serialize(&attrs).expect("ACL encoding"),
    })
}

#[cfg(target_os = "linux")]
fn acl_write(path: &std::path::Path, format: &str, data: &[u8]) -> Result<(), Error> {
    if format != "linux.posix-acl" {
        return Err(Error::not_supported());
    }
    let attrs: Vec<super::attributes::Xattr> = bincode::deserialize(data).expect("ACL encoding");
    for name in ["system.posix_acl_access", "system.posix_acl_default"] {
        if let Some(attr) = attrs.iter().find(|attr| attr.name == name.as_bytes()) {
            xattr::set(path, name, &attr.value)?;
        } else if xattr::get(path, name)?.is_some() {
            xattr::remove(path, name)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn acl_get_link_np(path: *const c_char, kind: c_int) -> *mut c_void;
        fn acl_set_link_np(path: *const c_char, kind: c_int, acl: *mut c_void) -> c_int;
        fn acl_size(acl: *mut c_void) -> isize;
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_copy_ext(buf: *mut c_void, acl: *mut c_void, size: isize) -> isize;
        fn acl_copy_int(buf: *const c_void) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> c_int;
    }
    struct Acl(*mut c_void);
    impl Drop for Acl {
        fn drop(&mut self) {
            unsafe {
                if !self.0.is_null() {
                    acl_free(self.0);
                }
            }
        }
    }
    pub(super) fn read(path: &std::path::Path) -> Result<Attribute, Error> {
        let path =
            CString::new(path.as_os_str().as_bytes()).map_err(|e| Error::custom(e.to_string()))?;
        unsafe {
            let mut acl = Acl(acl_get_link_np(path.as_ptr(), 0x100));
            if acl.0.is_null()
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT)
            {
                // An existing file with no extended ACL is an empty ACL.
                std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(path.as_bytes()))?;
                acl.0 = acl_init(0);
            }
            if acl.0.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let size = acl_size(acl.0);
            if size < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut data = vec![0; size as usize];
            if acl_copy_ext(data.as_mut_ptr().cast(), acl.0, size) < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Attribute::AccessControl {
                format: "macos.extended-acl".into(),
                data,
            })
        }
    }
    pub(super) fn write(path: &std::path::Path, format: &str, data: &[u8]) -> Result<(), Error> {
        if format != "macos.extended-acl" {
            return Err(Error::not_supported());
        }
        let path =
            CString::new(path.as_os_str().as_bytes()).map_err(|e| Error::custom(e.to_string()))?;
        unsafe {
            let acl = Acl(acl_copy_int(data.as_ptr().cast()));
            if acl.0.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            if acl_set_link_np(path.as_ptr(), 0x100, acl.0) != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
use macos::{read as acl_read, write as acl_write};
#[cfg(windows)]
use windows::{read as acl_read, write as acl_write};
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn acl_read(_path: &std::path::Path) -> Result<Attribute, Error> {
    Err(Error::not_supported())
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn acl_write(_path: &std::path::Path, _format: &str, _data: &[u8]) -> Result<(), Error> {
    Err(Error::not_supported())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{Foundation::*, Security::*, Storage::FileSystem::*};
    fn wide(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    pub(super) fn read_owner(path: &std::path::Path, group: bool) -> Result<Attribute, Error> {
        if std::fs::symlink_metadata(path)?.is_symlink() {
            return Err(Error::not_supported());
        }
        let path = wide(path);
        let info = if group {
            GROUP_SECURITY_INFORMATION
        } else {
            OWNER_SECURITY_INFORMATION
        };
        let mut needed = 0;
        unsafe {
            GetFileSecurityW(path.as_ptr(), info, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut data = vec![0; needed as usize];
            if GetFileSecurityW(
                path.as_ptr(),
                info,
                data.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Attribute::NativeOwner {
                format: "windows.security-descriptor".into(),
                data,
                group,
            })
        }
    }
    pub(super) fn write_owner(
        path: &std::path::Path,
        format: &str,
        data: &[u8],
        group: bool,
    ) -> Result<(), Error> {
        if format != "windows.security-descriptor" || std::fs::symlink_metadata(path)?.is_symlink()
        {
            return Err(Error::not_supported());
        }
        let path = wide(path);
        let info = if group {
            GROUP_SECURITY_INFORMATION
        } else {
            OWNER_SECURITY_INFORMATION
        };
        if unsafe { SetFileSecurityW(path.as_ptr(), info, data.as_ptr() as _) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    pub(super) fn read(path: &std::path::Path) -> Result<Attribute, Error> {
        if std::fs::symlink_metadata(path)?.is_symlink() {
            return Err(Error::not_supported());
        }
        let path = wide(path);
        let mut needed = 0;
        unsafe {
            GetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                0,
                &mut needed,
            );
            if needed == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut data = vec![0u8; needed as usize];
            if GetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION,
                data.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Attribute::AccessControl {
                format: "windows.dacl".into(),
                data,
            })
        }
    }
    pub(super) fn write(path: &std::path::Path, format: &str, data: &[u8]) -> Result<(), Error> {
        if format != "windows.dacl" || std::fs::symlink_metadata(path)?.is_symlink() {
            return Err(Error::not_supported());
        }
        let path = wide(path);
        unsafe {
            let mut control = 0;
            let mut revision = 0;
            if GetSecurityDescriptorControl(data.as_ptr() as _, &mut control, &mut revision) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let protection = if control & SE_DACL_PROTECTED != 0 {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
            if SetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION | protection,
                data.as_ptr() as _,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    }
    pub(super) fn streams(path: &std::path::Path) -> Result<Vec<NamedStream>, Error> {
        if std::fs::symlink_metadata(path)?.is_symlink() {
            return Ok(Vec::new());
        }
        let wide_path = wide(path);
        unsafe {
            let mut data: WIN32_FIND_STREAM_DATA = std::mem::zeroed();
            let handle = FindFirstStreamW(
                wide_path.as_ptr(),
                FindStreamInfoStandard,
                (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
                0,
            );
            if handle == INVALID_HANDLE_VALUE {
                let error = GetLastError();
                if error == ERROR_HANDLE_EOF || error == ERROR_INVALID_PARAMETER {
                    return Ok(Vec::new());
                }
                return Err(std::io::Error::from_raw_os_error(error as i32).into());
            }
            let mut streams = Vec::new();
            loop {
                let end = data
                    .cStreamName
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(data.cStreamName.len());
                let name = String::from_utf16_lossy(&data.cStreamName[..end]);
                if name != "::$DATA" {
                    streams.push(NamedStream {
                        path: super::stream_path(&PathBuf::from_native(path), &name)?,
                        name,
                        size: data.StreamSize as u64,
                    });
                }
                if FindNextStreamW(handle, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast()) == 0 {
                    let error = GetLastError();
                    FindClose(handle);
                    if error != ERROR_HANDLE_EOF {
                        return Err(std::io::Error::from_raw_os_error(error as i32).into());
                    }
                    break;
                }
            }
            Ok(streams)
        }
    }
}

#[cfg(unix)]
pub(crate) fn punch_holes(file: &std::fs::File, holes: &[(u64, u64)]) -> Result<(), Error> {
    use std::os::fd::AsRawFd;
    for &(offset, length) in holes {
        let start = offset.div_ceil(4096) * 4096;
        let end = (offset + length) / 4096 * 4096;
        if end <= start {
            continue;
        }
        #[cfg(target_os = "macos")]
        let result = unsafe {
            let hole = libc::fpunchhole_t {
                fp_flags: 0,
                reserved: 0,
                fp_offset: start as i64,
                fp_length: (end - start) as i64,
            };
            libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &hole)
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::fallocate(
                file.as_raw_fd(),
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                start as i64,
                (end - start) as i64,
            )
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let result = 0;
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
