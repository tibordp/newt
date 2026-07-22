//! Host-side drive-roots refresh (Windows only).
//!
//! Drive letters come and go (USB media, `net use`, `subst`), and every
//! Windows-styled mount's `mount_meta` describes drives of *this*
//! machine: the local session's ROOT, an elevated session's ROOT (same
//! machine, reached through the agent), and the client-local `Remote`
//! mount exposed into remote/WSL sessions. So the change signal lives
//! here — `WM_DEVICECHANGE` volume broadcasts caught by a hidden window,
//! plus a window-focus-regain sweep as the catch-all for changes that
//! don't broadcast (`subst`) — and each signal runs a logical remount
//! (`VfsManager::remount`) per candidate mount. Byte-comparing old and
//! new meta makes spurious signals free.

use newt_common::vfs::{PathStyle, VfsId};

use crate::main_window::MainWindowContext;

/// Remount every mount of this window's session whose `mount_meta`
/// describes this machine's drives; on change, rebuild the dependent
/// state (mount summary via `remount_vfs`, plus an open VFS selector).
pub async fn refresh_host_drive_mounts(ctx: &MainWindowContext) {
    // ROOT for local and elevated sessions. The elevated agent mostly
    // shares our drives — mapped network drives are per-user, which is
    // exactly why its remount asks the *agent* to enumerate (one
    // round-trip over a local pipe) rather than trusting our view. In
    // remote/WSL sessions ROOT is the agent's Unix FS — skipped — and
    // the client-local `Remote` mounts get owner-supplied meta instead.
    let is_remote = ctx.connection_target().is_remote();
    let remote_ids: Vec<VfsId> = ctx
        .with_session(|s| {
            s.mounted_vfs
                .read()
                .iter()
                .filter(|(_, info)| info.descriptor.type_name() == "remote")
                .map(|(id, _)| *id)
                .collect()
        })
        .unwrap_or_default();

    let mut changed = false;
    if !is_remote {
        match ctx.remount_vfs(VfsId::ROOT, None).await {
            Ok(c) => changed |= c,
            Err(e) => log::warn!("drive refresh: remount of ROOT failed: {e}"),
        }
    }
    if !remote_ids.is_empty() {
        let meta = newt_common::vfs::encode_mount_meta(
            PathStyle::host(),
            &newt_common::vfs::local::local_roots(),
        );
        for vfs_id in remote_ids {
            match ctx.remount_vfs(vfs_id, Some(meta.clone())).await {
                Ok(c) => changed |= c,
                Err(e) => log::warn!("drive refresh: remount of {vfs_id} failed: {e}"),
            }
        }
    }

    if !changed {
        return;
    }
    log::info!("drive refresh: roots changed, mount_meta updated");

    // Publish the refreshed summary, and if the VFS selector is open —
    // it shows stale drives — rebuild it wholesale by re-running its
    // dialog (fresh targets + free-space fill).
    let selector_pane = ctx
        .with_update(|gs| {
            let modal = gs.modal.0.read();
            Ok(match modal.as_ref() {
                Some(crate::main_window::ModalData {
                    kind: crate::main_window::ModalDataKind::SelectVfs { .. },
                    context,
                }) => context.pane_handle,
                _ => None,
            })
        })
        .unwrap_or(None);
    if let Some(pane) = selector_pane
        && let Err(e) = crate::cmd::dialog::dialog(
            ctx.clone(),
            crate::cmd::dialog::DialogKind::SelectVfs,
            Some(pane),
        )
    {
        log::warn!("drive refresh: selector rebuild failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Map / Unmap network drive (F11 / Alt+F11)
// ---------------------------------------------------------------------------

/// Open the system "Map Network Drive" wizard, owned by this session's
/// window. Runs on the main thread — the dialog pumps its own modal
/// message loop there, like any native modal. On success the pane
/// navigates to the freshly mapped drive (`WNetConnectionDialog1W`
/// reports the connected device number; the plain `WNetConnectionDialog`
/// reports nothing). The drive also arrives via the WM_DEVICECHANGE
/// watcher; the explicit refresh just makes it immediate.
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
            refresh_host_drive_mounts(ctx).await;
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

/// Navigate `pane_handle` to `drive` (`X:`) on whichever mount's freshly
/// refreshed meta owns it — ROOT locally, the client-local mount in a
/// remote session. Best-effort: an unknown drive just stays put.
async fn navigate_to_drive(
    ctx: &MainWindowContext,
    pane_handle: crate::main_window::PaneHandle,
    drive: &str,
) {
    let target = ctx
        .with_session(|s| {
            s.mounted_vfs.read().iter().find_map(|(id, info)| {
                newt_common::vfs::mount_root_infos(&info.mount_meta)
                    .into_iter()
                    .find(|r| r.path.components().nth(1) == Some(drive))
                    .map(|r| newt_common::vfs::VfsPath::new(*id, r.path))
            })
        })
        .ok()
        .flatten();
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
/// mapped network drive (per the mount's recorded `VolumeInfo`) and open
/// the confirmation dialog. Errors — not on a drive, not a network
/// drive — surface as toasts.
pub fn open_unmap_confirmation(
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
    let volume = ctx
        .with_session(|s| {
            s.mounted_vfs.read().get(&path.vfs_id).and_then(|info| {
                newt_common::vfs::mount_root_infos(&info.mount_meta)
                    .into_iter()
                    .find(|r| r.path.components().nth(1) == Some(drive.as_str()))
                    .and_then(|r| r.volume)
            })
        })?
        .ok_or_else(|| Error::Custom(format!("No volume information for {drive}")))?;
    if volume.kind != newt_common::vfs::VolumeKind::Network {
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
/// recorded in the open modal, relocate any pane parked on it, refresh
/// the roots.
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
            // `initial_path` alone could hand back that very drive.
            let target = ctx
                .with_session(|s| {
                    s.mounted_vfs.read().get(&path.vfs_id).and_then(|info| {
                        info.descriptor
                            .roots(&info.mount_meta)
                            .into_iter()
                            .find(|r| r.path.components().nth(1) != Some(drive.as_str()))
                            .map(|r| newt_common::vfs::VfsPath::new(path.vfs_id, r.path))
                    })
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| ctx.vfs_initial_path(path.vfs_id));
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

    refresh_host_drive_mounts(ctx).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// WM_DEVICECHANGE listener
// ---------------------------------------------------------------------------

static DRIVE_EVENTS: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<()>> =
    std::sync::OnceLock::new();

/// Spawn the process-wide volume-broadcast listener: a hidden top-level
/// window on its own thread (message-only windows don't receive
/// broadcasts), draining bursts of events into one refresh sweep across
/// every main window's session.
pub fn spawn_drive_watcher(app: tauri::AppHandle) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    if DRIVE_EVENTS.set(tx).is_err() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        while rx.recv().await.is_some() {
            // Coalesce: one plugged device fires several volume events
            // (partitions); a single sweep covers them all.
            while rx.try_recv().is_ok() {}
            let global_ctx: tauri::State<crate::GlobalContext> = app.state();
            let ctxs: Vec<MainWindowContext> = global_ctx
                .real_main_window_labels()
                .into_iter()
                .filter_map(|label| global_ctx.main_window_by_label(&label))
                .collect();
            for ctx in ctxs {
                refresh_host_drive_mounts(&ctx).await;
            }
        }
    });

    std::thread::Builder::new()
        .name("drive-watch".into())
        .spawn(run_message_window)
        .expect("failed to spawn drive-watch thread");
}

fn run_message_window() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVTYP_VOLUME,
        DEV_BROADCAST_HDR, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        TranslateMessage, WM_DEVICECHANGE, WNDCLASSW,
    };

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_DEVICECHANGE
            && (wparam as u32 == DBT_DEVICEARRIVAL || wparam as u32 == DBT_DEVICEREMOVECOMPLETE)
            && lparam != 0
        {
            // SAFETY: for these two events lParam points at a
            // DEV_BROADCAST_HDR-prefixed struct supplied by the system.
            let device_type = unsafe { (*(lparam as *const DEV_BROADCAST_HDR)).dbch_devicetype };
            if device_type == DBT_DEVTYP_VOLUME
                && let Some(tx) = DRIVE_EVENTS.get()
            {
                let _ = tx.send(());
            }
        }
        // SAFETY: plain passthrough for everything else.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    let class_name: Vec<u16> = "newt-drive-watch\0".encode_utf16().collect();
    // SAFETY: standard window-class + message-loop boilerplate; the
    // class name outlives registration and creation.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            log::warn!("drive watch: RegisterClassW failed");
            return;
        }
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            log::warn!("drive watch: CreateWindowExW failed");
            return;
        }
        let mut msg = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
