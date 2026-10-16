//! The Win32 clipboard: `CF_HDROP` and `Preferred DropEffect` for files,
//! `CF_UNICODETEXT` for text, and a message-only window that hears
//! `WM_CLIPBOARDUPDATE`.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tauri::AppHandle;
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG,
    RegisterClassW, WM_CLIPBOARDUPDATE, WNDCLASSW,
};

use super::{CopiedFiles, Stamp, error};
use crate::common::Error;

const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;

/// What the listener window calls on `WM_CLIPBOARDUPDATE`.
static CHANGED: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();

pub struct Clipboard;

impl Clipboard {
    pub fn new(_app: &AppHandle, changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        let _ = CHANGED.set(changed);
        if let Err(e) = std::thread::Builder::new()
            .name("clipboard-listener".into())
            .spawn(listen)
        {
            log::warn!("clipboard: no change listener: {e}");
        }
        Clipboard
    }

    pub async fn text(&self) -> Result<Option<String>, Error> {
        with_clipboard(|| unsafe { read_text() }).await
    }

    pub async fn set_text(&self, text: String) -> Result<Stamp, Error> {
        with_clipboard(move || unsafe {
            if EmptyClipboard() == 0 {
                return Err(error("couldn't empty it"));
            }
            put(CF_UNICODETEXT as u32, &utf16_bytes(text.encode_utf16()))
        })
        .await?;
        Ok(current_stamp())
    }

    pub async fn set_files(
        &self,
        paths: Vec<PathBuf>,
        text: String,
        cut: bool,
    ) -> Result<Stamp, Error> {
        with_clipboard(move || unsafe {
            if EmptyClipboard() == 0 {
                return Err(error("couldn't empty it"));
            }
            put(CF_HDROP as u32, &drop_files(&paths))?;
            let effect = if cut {
                DROPEFFECT_MOVE
            } else {
                DROPEFFECT_COPY
            };
            put(drop_effect_format(), &effect.to_le_bytes())?;
            put(CF_UNICODETEXT as u32, &utf16_bytes(text.encode_utf16()))
        })
        .await?;
        Ok(current_stamp())
    }

    pub async fn files(&self) -> Result<Option<CopiedFiles>, Error> {
        with_clipboard(|| unsafe { Ok(read_files()) }).await
    }

    pub async fn clear(&self) -> Result<(), Error> {
        with_clipboard(|| unsafe {
            if EmptyClipboard() == 0 {
                return Err(error("couldn't empty it"));
            }
            Ok(())
        })
        .await
    }

    pub async fn stamp(&self) -> Stamp {
        current_stamp()
    }
}

fn current_stamp() -> Stamp {
    Stamp(u64::from(unsafe { GetClipboardSequenceNumber() }))
}

/// Run `f` with the clipboard open, off the async runtime.
async fn with_clipboard<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tauri::async_runtime::spawn_blocking(move || {
        let _open = Open::new()?;
        f()
    })
    .await
    .map_err(error)?
}

/// The clipboard, open until dropped.
struct Open;

impl Open {
    fn new() -> Result<Self, Error> {
        // Only one window can have the clipboard open, and any app may hold
        // it for a moment (clipboard managers open it on every change), so
        // OpenClipboard fails now and then; every Win32 client retries.
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Open);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(error("another application has it open"))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn drop_effect_format() -> u32 {
    let name = wide("Preferred DropEffect");
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// Put `bytes` on the (open, emptied) clipboard as `format`.
unsafe fn put(format: u32, bytes: &[u8]) -> Result<(), Error> {
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if mem.is_null() {
            return Err(error("out of memory"));
        }
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            GlobalFree(mem);
            return Err(error("out of memory"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        GlobalUnlock(mem);
        // On success the system owns the memory.
        if SetClipboardData(format, mem as HANDLE).is_null() {
            GlobalFree(mem);
            return Err(error("it refused the data"));
        }
        Ok(())
    }
}

unsafe fn read_text() -> Result<Option<String>, Error> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) == 0 {
            return Ok(None);
        }
        let mem = GetClipboardData(CF_UNICODETEXT as u32);
        if mem.is_null() {
            return Ok(None);
        }
        let ptr = GlobalLock(mem) as *const u16;
        if ptr.is_null() {
            return Ok(None);
        }
        let units = std::slice::from_raw_parts(ptr, GlobalSize(mem) / 2);
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        let text = String::from_utf16_lossy(&units[..end]);
        GlobalUnlock(mem);
        Ok(Some(text))
    }
}

unsafe fn read_files() -> Option<CopiedFiles> {
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP as u32) == 0 {
            return None;
        }
        let drop = GetClipboardData(CF_HDROP as u32);
        if drop.is_null() {
            return None;
        }
        let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
        let mut paths = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(drop, i, buf.as_mut_ptr(), len + 1);
            paths.push(PathBuf::from(OsString::from_wide(&buf[..got as usize])));
        }
        Some(CopiedFiles {
            paths,
            cut: read_drop_effect() & DROPEFFECT_MOVE != 0,
        })
    }
}

unsafe fn read_drop_effect() -> u32 {
    unsafe {
        let format = drop_effect_format();
        if IsClipboardFormatAvailable(format) == 0 {
            return DROPEFFECT_COPY;
        }
        let mem = GetClipboardData(format);
        if mem.is_null() || GlobalSize(mem) < 4 {
            return DROPEFFECT_COPY;
        }
        let ptr = GlobalLock(mem) as *const u8;
        if ptr.is_null() {
            return DROPEFFECT_COPY;
        }
        let effect = u32::from_le_bytes(std::ptr::read_unaligned(ptr as *const [u8; 4]));
        GlobalUnlock(mem);
        effect
    }
}

/// A `CF_HDROP` block: `DROPFILES`, then each path NUL-terminated in
/// UTF-16, then one more NUL.
fn drop_files(paths: &[PathBuf]) -> Vec<u8> {
    let header = DROPFILES {
        pFiles: std::mem::size_of::<DROPFILES>() as u32,
        pt: POINT { x: 0, y: 0 },
        fNC: 0,
        fWide: 1,
    };
    let mut bytes = unsafe {
        std::slice::from_raw_parts(
            (&header as *const DROPFILES).cast::<u8>(),
            std::mem::size_of::<DROPFILES>(),
        )
    }
    .to_vec();
    let mut units: Vec<u16> = Vec::new();
    for path in paths {
        units.extend(path.as_os_str().encode_wide());
        units.push(0);
    }
    units.push(0);
    bytes.extend(utf16_bytes(units.into_iter()));
    bytes
}

/// UTF-16 units as little-endian bytes, NUL-terminated unless they already
/// end in one.
fn utf16_bytes(units: impl Iterator<Item = u16>) -> Vec<u8> {
    let mut units: Vec<u16> = units.collect();
    if units.last() != Some(&0) {
        units.push(0);
    }
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The listener: a message-only window subscribed to clipboard changes,
/// pumping its messages for the life of the process.
fn listen() {
    unsafe {
        let class = wide("NewtClipboardListener");
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() || AddClipboardFormatListener(hwnd) == 0 {
            log::warn!("clipboard: couldn't listen for changes");
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        if let Some(changed) = CHANGED.get() {
            changed();
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
