//! Classic shell context menu (`IContextMenu`) for host-local files
//! (Windows only).
//!
//! Our own context menu stays the default; this pops the native menu on
//! request (a trailing menu item, or Shift+right-click directly). The
//! whole dance runs synchronously on the main thread — `TrackPopupMenuEx`
//! pumps its own modal message loop there, like any native menu — with
//! the Tauri window temporarily subclassed so `WM_INITMENUPOPUP` &
//! friends reach `IContextMenu2/3` (without that, "Open with" and
//! "Send to" submenus come up empty).

use newt_common::vfs::VfsPath;

use crate::common::Error;
use crate::main_window::{MainWindowContext, PaneHandle};

pub async fn show_shell_context_menu(
    ctx: &MainWindowContext,
    pane_handle: PaneHandle,
    x: f64,
    y: f64,
    on_background: bool,
) -> Result<(), Error> {
    let vfs_info = ctx.vfs_info()?;
    let targets: Vec<VfsPath> = {
        let pane = ctx
            .panes()
            .get(pane_handle)
            .ok_or_else(|| Error::Custom("no such pane".into()))?;
        if on_background {
            vec![pane.path()]
        } else {
            pane.get_effective_selection()
        }
    };
    if targets.is_empty() {
        return Err(Error::Custom("Nothing selected".into()));
    }
    if !targets.iter().all(|t| vfs_info.is_host_local(t.vfs_id)) {
        return Err(Error::Custom(
            "The Windows shell menu is only available for local files".into(),
        ));
    }
    // `launch_cwd` strips the verbatim prefix `to_native` produces —
    // `SHParseDisplayName` rejects `\\?\` paths.
    let native: Vec<std::path::PathBuf> = targets
        .iter()
        .map(|t| newt_common::vfs::local::launch_cwd(&t.path))
        .collect();

    let window = ctx.window();
    let hwnd = window.hwnd()?.0 as isize;
    let scale = window.scale_factor().unwrap_or(1.0);
    let (px, py) = ((x * scale).round() as i32, (y * scale).round() as i32);

    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            // SAFETY: hwnd is this window's handle; the paths outlive the
            // (synchronous, modal) call.
            let result = unsafe { show_menu_blocking(hwnd, &native, px, py) };
            let _ = tx.send(result);
        })
        .map_err(|e| Error::Custom(format!("run_on_main_thread: {e}")))?;
    rx.await
        .map_err(|_| Error::Custom("shell menu vanished".into()))?
        .map_err(Error::Custom)
}

// ---------------------------------------------------------------------------
// Native plumbing (main thread only)
// ---------------------------------------------------------------------------

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{IContextMenu2, IContextMenu3};
use windows::Win32::UI::WindowsAndMessaging::HMENU;

const ID_FIRST: u32 = 1;
const ID_LAST: u32 = 0x7FFF;
const SUBCLASS_ID: usize = 0x7477656e; // "newt"

struct ComInit(bool);
impl ComInit {
    /// The main thread is already STA under wry (OLE for drag-drop), so
    /// this is a ref-count bump; balanced on drop iff it succeeded.
    fn new() -> Self {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}
impl Drop for ComInit {
    fn drop(&mut self) {
        if self.0 {
            unsafe { windows::Win32::System::Com::CoUninitialize() }
        }
    }
}

struct Pidl(*mut ITEMIDLIST);
impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { windows::Win32::UI::Shell::ILFree(Some(self.0)) }
    }
}

struct Menu(HMENU);
impl Drop for Menu {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyMenu(self.0) };
    }
}

/// While the menu tracks, `WM_INITMENUPOPUP`/`WM_DRAWITEM`/
/// `WM_MEASUREITEM`/`WM_MENUCHAR` land on the owning window and must be
/// forwarded to the context-menu object for dynamic submenus to
/// populate. Installed as a window subclass for the duration.
struct MenuHost {
    icm2: Option<IContextMenu2>,
    icm3: Option<IContextMenu3>,
}

struct Subclass {
    hwnd: HWND,
    // Box gives the host a stable address for the subclass proc refdata.
    _host: Box<MenuHost>,
}

impl Subclass {
    fn install(hwnd: HWND, menu: &windows::Win32::UI::Shell::IContextMenu) -> Option<Self> {
        use windows::core::Interface;
        let host = Box::new(MenuHost {
            icm2: menu.cast().ok(),
            icm3: menu.cast().ok(),
        });
        if host.icm2.is_none() && host.icm3.is_none() {
            return None;
        }
        let ok = unsafe {
            windows::Win32::UI::Shell::SetWindowSubclass(
                hwnd,
                Some(subclass_proc),
                SUBCLASS_ID,
                &*host as *const MenuHost as usize,
            )
        };
        ok.as_bool().then_some(Self { hwnd, _host: host })
    }
}

impl Drop for Subclass {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::Shell::RemoveWindowSubclass(
                self.hwnd,
                Some(subclass_proc),
                SUBCLASS_ID,
            );
        }
    }
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
    };
    if matches!(
        msg,
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
    ) {
        // SAFETY: refdata is the boxed MenuHost, alive until the Subclass
        // guard (which outlives menu tracking) drops.
        let host = unsafe { &*(refdata as *const MenuHost) };
        if let Some(icm3) = &host.icm3 {
            let mut lres = LRESULT(0);
            if unsafe { icm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut lres)) }.is_ok() {
                return lres;
            }
        } else if let Some(icm2) = &host.icm2
            && unsafe { icm2.HandleMenuMsg(msg, wparam, lparam) }.is_ok()
        {
            return LRESULT(0);
        }
    }
    unsafe { windows::Win32::UI::Shell::DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Parse the paths, query the shell for the merged context menu, track
/// it at (`x`,`y`) client coordinates, invoke the chosen verb. All items
/// are children of one directory (a pane selection), so the first item's
/// parent `IShellFolder` serves for the whole batch.
unsafe fn show_menu_blocking(
    hwnd_raw: isize,
    paths: &[std::path::PathBuf],
    x: i32,
    y: i32,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::{
        CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, IContextMenu,
        ILFindLastID, IShellFolder, SEE_MASK_UNICODE, SHBindToParent, SHParseDisplayName,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreatePopupMenu, GetMenuItemCount, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        TrackPopupMenuEx,
    };
    use windows::core::{PCSTR, PCWSTR};

    let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
    let mut pt = POINT { x, y };
    let _ = unsafe { windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut pt) };

    let _com = ComInit::new();

    let mut pidls: Vec<Pidl> = Vec::with_capacity(paths.len());
    for p in paths {
        let wide: Vec<u16> = p
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None) }
            .map_err(|e| format!("The shell rejected {}: {e}", p.display()))?;
        pidls.push(Pidl(pidl));
    }

    let folder: IShellFolder =
        unsafe { SHBindToParent(pidls[0].0, None) }.map_err(|e| format!("SHBindToParent: {e}"))?;
    let children: Vec<*const ITEMIDLIST> = pidls
        .iter()
        .map(|p| unsafe { ILFindLastID(p.0) } as *const ITEMIDLIST)
        .collect();

    let menu: IContextMenu = unsafe { folder.GetUIObjectOf(hwnd, &children, None) }
        .map_err(|e| format!("GetUIObjectOf: {e}"))?;

    let hmenu = Menu(unsafe { CreatePopupMenu() }.map_err(|e| format!("CreatePopupMenu: {e}"))?);
    unsafe { menu.QueryContextMenu(hmenu.0, 0, ID_FIRST, ID_LAST, CMF_NORMAL) }
        .ok()
        .map_err(|e| format!("QueryContextMenu: {e}"))?;
    if unsafe { GetMenuItemCount(Some(hmenu.0)) } <= 0 {
        return Ok(());
    }

    let cmd = {
        let _subclass = Subclass::install(hwnd, &menu);
        unsafe {
            TrackPopupMenuEx(
                hmenu.0,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                pt.x,
                pt.y,
                hwnd,
                None,
            )
        }
        .0
    };
    if cmd <= 0 {
        return Ok(()); // dismissed
    }

    let verb = (cmd as u32 - ID_FIRST) as usize;
    let dir_wide: Option<Vec<u16>> =
        paths[0]
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| {
                p.as_os_str()
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect()
            });
    // SAFETY: zeroed is valid for this all-plain-data struct; overwritten
    // fields carry MAKEINTRESOURCE verbs per the IContextMenu contract.
    let mut info: CMINVOKECOMMANDINFOEX = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32;
    // CMIC_MASK_UNICODE is SEE_MASK_UNICODE by definition; the windows
    // crate only exposes the latter name.
    info.fMask = SEE_MASK_UNICODE | CMIC_MASK_PTINVOKE;
    info.hwnd = hwnd;
    info.lpVerb = PCSTR(verb as *const u8);
    info.lpVerbW = PCWSTR(verb as *const u16);
    info.lpDirectoryW = dir_wide
        .as_ref()
        .map(|w| PCWSTR(w.as_ptr()))
        .unwrap_or(PCWSTR::null());
    info.nShow = SW_SHOWNORMAL.0;
    info.ptInvoke = pt;
    match unsafe { menu.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO) } {
        Ok(()) => Ok(()),
        // User backed out of the verb's own UI (e.g. a delete
        // confirmation) — not an error worth a toast.
        Err(e) if e.code() == windows::Win32::Foundation::ERROR_CANCELLED.to_hresult() => Ok(()),
        Err(e) => Err(format!("InvokeCommand: {e}")),
    }
}
