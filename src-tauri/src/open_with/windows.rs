use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IDataObject};
use windows::Win32::UI::Shell::{
    ASSOC_FILTER_RECOMMENDED, ASSOCF_NONE, ASSOCSTR_EXECUTABLE, AssocQueryStringW, BHID_DataObject,
    IAssocHandler, IShellItem, OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OAIF_REGISTER_EXT,
    OPEN_AS_INFO_FLAGS, OPENASINFO, SHAssocEnumHandlers, SHCreateItemFromParsingName,
    SHOpenWithDialog,
};
use windows::core::{HRESULT, HSTRING, PCWSTR, PWSTR};

use super::OpenWithApp;
use crate::main_window::shell_menu::ComInit;

/// A string the shell allocated, freed after copying.
fn take(s: PWSTR) -> String {
    // SAFETY: the shell hands back a NUL-terminated string from
    // `CoTaskMemAlloc`, ours to free.
    let text = unsafe { s.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(s.0 as *const _)) };
    text
}

fn extension(name: &str) -> Option<HSTRING> {
    let extension = Path::new(name).extension()?;
    Some(HSTRING::from(format!(".{}", extension.to_string_lossy())))
}

/// The handlers recommended for `name`'s extension.
fn handlers(name: &str) -> windows::core::Result<Vec<IAssocHandler>> {
    let Some(extension) = extension(name) else {
        return Ok(Vec::new());
    };
    let handlers = unsafe { SHAssocEnumHandlers(&extension, ASSOC_FILTER_RECOMMENDED) }?;
    let mut out = Vec::new();
    loop {
        let mut batch = [None];
        let mut fetched = 0;
        unsafe { handlers.Next(&mut batch, Some(&mut fetched)) }?;
        if fetched == 0 {
            break;
        }
        out.extend(batch[0].take());
    }
    Ok(out)
}

/// The executable that opens `name`'s extension by default.
fn default_executable(name: &str) -> Option<String> {
    let extension = extension(name)?;
    let mut len = 0u32;
    // Sizes the buffer; a null buffer returns S_FALSE with the length.
    let _ = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_EXECUTABLE,
            &extension,
            PCWSTR::null(),
            None,
            &mut len,
        )
    };
    if len == 0 {
        return None;
    }
    let mut buffer = vec![0u16; len as usize];
    unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_EXECUTABLE,
            &extension,
            PCWSTR::null(),
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
    }
    .ok()
    .ok()?;
    // `len` counts the terminating NUL.
    Some(String::from_utf16_lossy(
        &buffer[..(len as usize).saturating_sub(1)],
    ))
}

pub fn apps_for(name: &str) -> Vec<OpenWithApp> {
    let _com = ComInit::new();
    let default = default_executable(name).map(|path| path.to_lowercase());
    let handlers = handlers(name).unwrap_or_else(|e| {
        log::warn!("open with: listing handlers for {name}: {e}");
        Vec::new()
    });
    handlers
        .into_iter()
        .filter_map(|handler| {
            // For a packaged app the name is its AppUserModelID rather than
            // an executable, so it is never taken for the default.
            let id = take(unsafe { handler.GetName() }.ok()?);
            let name = take(unsafe { handler.GetUIName() }.ok()?);
            Some(OpenWithApp {
                is_default: default.as_deref() == Some(id.to_lowercase().as_str()),
                id,
                name,
            })
        })
        .collect()
}

pub fn open_with(path: &Path, app: &str) -> Result<(), String> {
    let _com = ComInit::new();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let handler = handlers(&name)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|handler| {
            unsafe { handler.GetName() }
                .ok()
                .map(take)
                .is_some_and(|id| id == app)
        })
        .ok_or_else(|| format!("{app} is no longer offered for {name}"))?;
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(path), None::<&IBindCtx>) }
            .map_err(|e| e.to_string())?;
    let data: IDataObject = unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) }
        .map_err(|e| e.to_string())?;
    unsafe { handler.Invoke(&data) }.map_err(|e| e.to_string())
}

/// `SHOpenWithDialog`, modal to the window: like the shell menu, it runs
/// its own message loop on the main thread.
pub fn choose(window: &tauri::WebviewWindow, path: PathBuf) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
    window
        .run_on_main_thread(move || {
            let _com = ComInit::new();
            let file = HSTRING::from(path.as_path());
            let info = OPENASINFO {
                pcszFile: PCWSTR(file.as_ptr()),
                pcszClass: PCWSTR::null(),
                oaifInFlags: OPEN_AS_INFO_FLAGS(
                    OAIF_ALLOW_REGISTRATION.0 | OAIF_REGISTER_EXT.0 | OAIF_EXEC.0,
                ),
            };
            // SAFETY: `hwnd` is this window's handle; `file` outlives the
            // (synchronous, modal) call.
            let result =
                unsafe { SHOpenWithDialog(Some(HWND(hwnd as *mut core::ffi::c_void)), &info) };
            if let Err(e) = result
                && e.code() != HRESULT::from_win32(ERROR_CANCELLED.0)
            {
                log::warn!("open with: {e}");
            }
        })
        .map_err(|e| e.to_string())
}
