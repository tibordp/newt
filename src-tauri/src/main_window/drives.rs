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
