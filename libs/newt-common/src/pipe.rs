//! Named pipes for talking to our own processes. A pipe created without a
//! security descriptor gets the default DACL, which lets Everyone open it
//! for reading; these grant only this user, SYSTEM and Administrators (an
//! over-the-shoulder elevation runs the agent as another administrator).
//! Names carry a random part so another account can't take one first.

use std::ffi::c_void;
use std::io;
use std::sync::OnceLock;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `\\.\pipe\<prefix>-<random>`.
pub fn unique_name(prefix: &str) -> String {
    format!(r"\\.\pipe\{}-{}", prefix, uuid::Uuid::new_v4())
}

/// `options.create(name)`, with the private DACL.
pub fn create(options: &ServerOptions, name: &str) -> io::Result<NamedPipeServer> {
    let sddl = wide(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{})",
        user_sid()?
    ));
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated and outlives the call; `sd` receives
    // a LocalAlloc'd descriptor, freed below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    // SAFETY: `attrs` and the descriptor it points to are valid for the
    // duration of the call, which copies them into the pipe.
    let server = unsafe {
        options.create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut c_void)
    };
    unsafe { LocalFree(sd) };
    server
}

/// This process's user SID in string form.
fn user_sid() -> io::Result<&'static str> {
    static SID: OnceLock<String> = OnceLock::new();
    if let Some(sid) = SID.get() {
        return Ok(sid);
    }
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing;
    // `token` is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = token_user_sid(token);
    unsafe { CloseHandle(token) };
    let sid = sid?;
    Ok(SID.get_or_init(|| sid))
}

fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut len = 0u32;
    // Sizing call: fails with ERROR_INSUFFICIENT_BUFFER, setting `len`.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    // u64s keep the buffer aligned for TOKEN_USER.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` bytes.
    if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID
    // points into the same buffer.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut s: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `s` is a LocalAlloc'd NUL-terminated wide string.
    let out = unsafe {
        let len = (0..).take_while(|&i| *s.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(s, len))
    };
    unsafe { LocalFree(s.cast()) };
    Ok(out)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
