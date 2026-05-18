//! WSL distribution enumeration.
//!
//! `wslapi.dll` exposes only per-distro functions (`WslLaunch`,
//! `WslGetDistributionConfiguration`, …) — it has no API to list installed
//! distributions. So, like Windows Terminal / VS Code, we read them from
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`: each GUID subkey
//! describes one distro (`DistributionName`, `State`), and the parent key's
//! `DefaultDistribution` value names the default distro's subkey.
//!
//! Windows-only — declared `#[cfg(windows)]` from the `discovery` module.

use serde::Serialize;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_SZ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    RegQueryValueExW,
};

const LXSS_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Lxss";

/// One installed WSL distribution. Serialized into the picker modal payload.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct WslDistro {
    pub name: String,
    pub is_default: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// RAII wrapper closing the registry handle on drop.
struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn open(parent: HKEY, sub: &str) -> Option<RegKey> {
    let sub_w = wide(sub);
    let mut hk: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(parent, sub_w.as_ptr(), 0, KEY_READ, &mut hk) };
    (rc == ERROR_SUCCESS).then_some(RegKey(hk))
}

fn read_sz(key: &RegKey, name: &str) -> Option<String> {
    let name_w = wide(name);
    let mut ty: u32 = 0;
    let mut len: u32 = 0;
    // First call sizes the value (REG_SZ → byte count incl. NUL).
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            name_w.as_ptr(),
            std::ptr::null(),
            &mut ty,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if rc != ERROR_SUCCESS || ty != REG_SZ || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let mut got = len;
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            name_w.as_ptr(),
            std::ptr::null(),
            &mut ty,
            buf.as_mut_ptr(),
            &mut got,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let u16buf: Vec<u16> = buf[..got as usize]
        .chunks_exact(2)
        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
        .collect();
    Some(from_wide(&u16buf))
}

fn read_dword(key: &RegKey, name: &str) -> Option<u32> {
    let name_w = wide(name);
    let mut ty: u32 = 0;
    let mut data: u32 = 0;
    let mut len: u32 = 4;
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            name_w.as_ptr(),
            std::ptr::null(),
            &mut ty,
            &mut data as *mut u32 as *mut u8,
            &mut len,
        )
    };
    (rc == ERROR_SUCCESS).then_some(data)
}

/// Installed, usable WSL distributions, default first then alphabetical.
/// Returns empty if WSL isn't installed (the Lxss key is absent).
pub fn list_distros() -> Vec<WslDistro> {
    let Some(root) = open(HKEY_CURRENT_USER, LXSS_PATH) else {
        return Vec::new();
    };
    let default_guid = read_sz(&root, "DefaultDistribution");

    let mut out: Vec<WslDistro> = Vec::new();
    let mut idx: u32 = 0;
    loop {
        let mut name_buf = [0u16; 256];
        let mut name_len = name_buf.len() as u32;
        let rc = unsafe {
            RegEnumKeyExW(
                root.0,
                idx,
                name_buf.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != ERROR_SUCCESS {
            break;
        }
        idx += 1;

        let guid = from_wide(&name_buf[..name_len as usize]);
        let Some(sub) = open(root.0, &guid) else {
            continue;
        };
        // State == 1 → installed and usable (2 = installing, 3 = uninstalling).
        if read_dword(&sub, "State") != Some(1) {
            continue;
        }
        let Some(name) = read_sz(&sub, "DistributionName") else {
            continue;
        };
        let is_default = default_guid.as_deref() == Some(guid.as_str());
        out.push(WslDistro { name, is_default });
    }

    out.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// The default distro name, falling back to the first installed one.
pub fn default_distro() -> Option<String> {
    let mut list = list_distros();
    if list.is_empty() {
        return None;
    }
    if let Some(d) = list.iter().find(|d| d.is_default) {
        return Some(d.name.clone());
    }
    Some(list.remove(0).name)
}
