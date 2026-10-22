//! Map and disconnect network drives (F11 / Alt+F11), Windows only. The
//! drives a filesystem has are asked of it when needed (`Vfs::roots`), so
//! nothing here keeps them current.

use newt_common::path::VfsPath;
use newt_engine::vfs::RootInfo;

use crate::main_window::MainWindowContext;

/// The root of `roots` that is drive `drive` (`X:`), if any.
fn drive_root(roots: Vec<RootInfo>, drive: &str) -> Option<RootInfo> {
    roots
        .into_iter()
        .find(|r| r.path.components().nth(1) == Some(drive))
}

// ---------------------------------------------------------------------------
// Map / Unmap network drive (F11 / Alt+F11)
// ---------------------------------------------------------------------------

/// Open the system "Map Network Drive" wizard, owned by this session's
/// window. Runs on the main thread — the dialog pumps its own modal
/// message loop there, like any native modal. On success the pane
/// navigates to the freshly mapped drive (`WNetConnectionDialog1W`
/// reports the connected device number; the plain `WNetConnectionDialog`
/// reports nothing).
pub async fn map_network_drive(
    ctx: &MainWindowContext,
    pane_handle: crate::main_window::PaneHandle,
) -> Result<(), crate::common::Error> {
    use windows_sys::Win32::NetworkManagement::WNet::{
        CONNECTDLGSTRUCTW, NETRESOURCEW, RESOURCETYPE_DISK, WNetConnectionDialog1W,
    };

    let window = ctx.window();
    let hwnd = window.hwnd()?.0 as isize;
    let (tx, rx) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            // SAFETY: a zeroed NETRESOURCEW with only dwType set is the
            // documented "browse" input; both structs only need to live
            // across the (synchronous, modal) call.
            let (ret, dev_num) = unsafe {
                let mut res: NETRESOURCEW = std::mem::zeroed();
                res.dwType = RESOURCETYPE_DISK;
                let mut dlg: CONNECTDLGSTRUCTW = std::mem::zeroed();
                dlg.cbStructure = std::mem::size_of::<CONNECTDLGSTRUCTW>() as u32;
                dlg.hwndOwner = hwnd as _;
                dlg.lpConnRes = &mut res;
                let ret = WNetConnectionDialog1W(&mut dlg);
                (ret, dlg.dwDevNum)
            };
            let _ = tx.send((ret, dev_num));
        })
        .map_err(|e| crate::common::Error::Custom(format!("run_on_main_thread: {e}")))?;
    let (ret, dev_num) = rx
        .await
        .map_err(|_| crate::common::Error::Custom("map dialog vanished".into()))?;
    match ret {
        0 => {
            // dwDevNum: 1 = A:, 2 = B:, …; -1 for a deviceless connection.
            if (1..=26).contains(&dev_num) {
                let drive = format!("{}:", (b'A' + dev_num as u8 - 1) as char);
                navigate_to_drive(ctx, pane_handle, &drive).await;
            }
        }
        // Documented "user cancelled" sentinel.
        u32::MAX => {}
        code => {
            return Err(crate::common::Error::Custom(format!(
                "Map Network Drive failed (WNet error {code})"
            )));
        }
    }
    Ok(())
}

/// Navigate `pane_handle` to `drive` (`X:`) on whichever split-root mount
/// has it — ROOT locally, the client-local mount in a remote session.
/// Best-effort: an unknown drive just stays put.
async fn navigate_to_drive(
    ctx: &MainWindowContext,
    pane_handle: crate::main_window::PaneHandle,
    drive: &str,
) {
    let target = ctx
        .split_roots()
        .await
        .into_iter()
        .find_map(|(id, roots)| drive_root(roots, drive).map(|r| VfsPath::new(id, r.path)));
    if let Some(target) = target {
        let _ = ctx
            .with_pane_update_async(pane_handle, |_, pane| async move {
                pane.navigate_to(target).await?;
                Ok(())
            })
            .await;
    }
}

/// Alt+F11 entry point: validate that the pane's current drive is a
/// mapped network drive (per its `VolumeInfo`) and open the confirmation
/// dialog. Errors — not on a drive, not a network drive — surface as
/// toasts.
pub async fn open_unmap_confirmation(
    ctx: &MainWindowContext,
    pane_handle: crate::main_window::PaneHandle,
) -> Result<(), crate::common::Error> {
    use crate::common::Error;

    let pane = ctx
        .panes()
        .get(pane_handle)
        .ok_or_else(|| Error::Custom("no such pane".into()))?;
    let path = pane.path();
    let drive = {
        let mut comps = path.path.components();
        (comps.next() == Some("?"))
            .then(|| comps.next())
            .flatten()
            .filter(|c| c.len() == 2 && c.ends_with(':'))
            .map(str::to_string)
            .ok_or_else(|| Error::Custom("The pane is not on a Windows drive".into()))?
    };
    let roots = ctx.fs()?.roots(path.vfs_id).await?;
    let volume = drive_root(roots, &drive)
        .and_then(|r| r.volume)
        .ok_or_else(|| Error::Custom(format!("No volume information for {drive}")))?;
    if volume.kind != newt_engine::vfs::VolumeKind::Network {
        return Err(Error::Custom(format!(
            "{drive} is not a mapped network drive"
        )));
    }

    ctx.with_update(|gs| {
        *gs.modal.0.write() = Some(crate::main_window::ModalData {
            kind: crate::main_window::ModalDataKind::ConfirmUnmapDrive {
                drive,
                target: volume.target,
            },
            context: crate::main_window::ModalContext {
                pane_handle: Some(pane_handle),
            },
        });
        Ok(())
    })
}

/// The confirmation dialog's "Disconnect" button: disconnect the drive
/// recorded in the open modal, relocating any pane parked on it first.
pub async fn confirm_unmap_drive(ctx: &MainWindowContext) -> Result<(), crate::common::Error> {
    use crate::common::Error;
    use windows_sys::Win32::NetworkManagement::WNet::{
        CONNECT_UPDATE_PROFILE, WNetCancelConnection2W,
    };

    let drive = ctx.with_update(|gs| {
        let drive = match &*gs.modal.0.read() {
            Some(crate::main_window::ModalData {
                kind: crate::main_window::ModalDataKind::ConfirmUnmapDrive { drive, .. },
                ..
            }) => drive.clone(),
            _ => return Err(Error::Custom("modal is not an unmap confirmation".into())),
        };
        gs.close_modal();
        Ok(drive)
    })?;

    // Relocate panes off the drive *before* disconnecting: a pane parked
    // there holds its directory-watcher handle (ReadDirectoryChangesW)
    // open, which the disconnect counts as an open file (WNet 2401).
    for handle in [
        crate::main_window::PaneHandle::left(),
        crate::main_window::PaneHandle::right(),
    ] {
        let Some(pane) = ctx.panes().get(handle) else {
            continue;
        };
        let path = pane.path();
        let mut comps = path.path.components();
        if comps.next() == Some("?") && comps.next() == Some(drive.as_str()) {
            // First root that isn't the drive being disconnected —
            // `vfs_initial_path` alone could hand back that very drive.
            let other = match ctx.fs() {
                Ok(fs) => fs.roots(path.vfs_id).await.ok().and_then(|roots| {
                    roots
                        .into_iter()
                        .find(|r| r.path.components().nth(1) != Some(drive.as_str()))
                }),
                Err(_) => None,
            };
            let target = match other {
                Some(root) => VfsPath::new(path.vfs_id, root.path),
                None => ctx.vfs_initial_path(path.vfs_id).await,
            };
            let _ = ctx
                .with_pane_update_async(handle, |_, pane| async move {
                    pane.navigate_to(target).await?;
                    Ok(())
                })
                .await;
        }
    }

    // No force: open handles surface as a WNet error instead of yanking
    // the drive out from under running operations. One bounded retry on
    // ERROR_OPEN_FILES (2401): the relocated pane's watcher handle is
    // closed by notify's watcher thread with no completion signal we
    // could await instead.
    const ERROR_OPEN_FILES: u32 = 2401;
    let mut ret = 0;
    for attempt in 0..2 {
        let drive_wide: Vec<u16> = drive.encode_utf16().chain(std::iter::once(0)).collect();
        ret = tokio::task::spawn_blocking(move || {
            // SAFETY: `drive_wide` is a NUL-terminated `X:` string.
            unsafe { WNetCancelConnection2W(drive_wide.as_ptr(), CONNECT_UPDATE_PROFILE, 0) }
        })
        .await
        .map_err(|e| Error::Custom(e.to_string()))?;
        if ret != ERROR_OPEN_FILES || attempt == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    match ret {
        0 => {}
        ERROR_OPEN_FILES => {
            return Err(Error::Custom(format!(
                "Cannot disconnect {drive}: files on it are still open (viewers, editors, terminals?)"
            )));
        }
        code => {
            return Err(Error::Custom(format!(
                "Failed to disconnect {drive} (WNet error {code})"
            )));
        }
    }

    Ok(())
}
