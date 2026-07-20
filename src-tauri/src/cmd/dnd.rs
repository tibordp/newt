use newt_common::operation::{OperationId, OperationRequest};
use newt_common::vfs::VfsPath;
use newt_common::vfs::local::to_native;

use crate::common::Error;
use crate::main_window::{DndData, DndFile, MainWindowContext, MainWindowState, PaneHandle};

#[tauri::command]
#[specta::specta]
pub fn start_dnd(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    files: Vec<DndFile>,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        *gs.dnd.0.write() = Some(DndData::new(pane_handle, files));
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn cancel_dnd(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let mut slot = gs.dnd.0.write();
        // Don't kill a live native drag session — its own drop callback
        // (or a self-drop) clears it.
        if slot.as_ref().is_none_or(|d| !d.outbound) {
            *slot = None;
        }
        Ok(())
    })
}

/// Deref each dragged file via the source pane's view of the entry so
/// dragging from a SearchVfs operates on the real underlying files; falls
/// back to `source_path.join(name)`. Returns the sources and the source
/// pane's path.
fn resolve_dnd_sources(
    gs: &MainWindowState,
    dnd_data: &DndData,
) -> Result<(Vec<VfsPath>, VfsPath), Error> {
    let source_pane = gs
        .panes
        .get(dnd_data.source_pane)
        .ok_or_else(|| Error::Custom("source pane not found".into()))?;
    let source_path = source_pane.path();
    let view_state = source_pane.view_state();
    let sources: Vec<VfsPath> = dnd_data
        .files
        .iter()
        .map(|f| {
            view_state
                .files()
                .iter()
                .find(|file| file.key() == f.name)
                .and_then(|file| file.source.clone())
                .unwrap_or_else(|| source_path.join(&f.name))
        })
        .collect();
    drop(view_state);
    Ok((sources, source_path))
}

#[tauri::command]
#[specta::specta]
pub async fn execute_dnd(
    ctx: MainWindowContext,
    destination_pane: PaneHandle,
    subdirectory: Option<String>,
    is_move: bool,
) -> Result<OperationId, Error> {
    let (sources, dest_path) = ctx.with_update(|gs| {
        let dnd_data = gs
            .dnd
            .0
            .write()
            .take()
            .ok_or_else(|| Error::Custom("no active DnD session".into()))?;

        let (sources, _) = resolve_dnd_sources(gs, &dnd_data)?;

        let dest_pane = gs
            .panes
            .get(destination_pane)
            .ok_or_else(|| Error::Custom("destination pane not found".into()))?;
        Ok((sources, dest_pane.path()))
    })?;

    let destination = match subdirectory {
        Some(sub) => dest_path.join(&sub),
        None => dest_path,
    };

    let request = if is_move {
        OperationRequest::Move {
            rename_to: None,
            sources,
            destination,
            options: Default::default(),
        }
    } else {
        OperationRequest::Copy {
            rename_to: None,
            sources,
            destination,
            options: Default::default(),
        }
    };

    super::operations::start_operation(ctx, request).await
}

/// Escalate the active internal drag to a native OS drag session so files
/// can be dropped into other applications (and other Newt windows).
/// Returns false when there is nothing to escalate — no active drag, or a
/// dragged file doesn't resolve to a host-local path (S3/SFTP/remote
/// sessions keep internal-only DnD until temp materialization exists).
///
/// `image` is the PNG drag preview rendered by the frontend; an empty
/// vector falls back to the app icon (the macOS backend aborts on invalid
/// image bytes).
#[tauri::command]
#[specta::specta]
pub fn dnd_drag_out(ctx: MainWindowContext, image: Vec<u8>) -> Result<bool, Error> {
    let vfs_info = ctx.vfs_info()?;

    // Validate and flip to outbound under one lock so a concurrent
    // execute_dnd/cancel_dnd can't take the data in between.
    let Some((native, generation)) = ctx.with_update(|gs| {
        let mut slot = gs.dnd.0.write();
        let Some(dnd) = slot.as_mut() else {
            return Ok(None);
        };
        if dnd.outbound {
            return Ok(None);
        }
        let (sources, _) = resolve_dnd_sources(gs, dnd)?;
        if !sources.iter().all(|s| vfs_info.is_host_local(s.vfs_id)) {
            return Ok(None);
        }
        // Windows paths longer than MAX_PATH stay verbatim through
        // dunce::canonicalize and are known to crash drag-rs (#76);
        // accepted as an upstream limitation.
        let native: Vec<std::path::PathBuf> = sources.iter().map(|s| to_native(&s.path)).collect();
        dnd.outbound = true;
        Ok(Some((native, dnd.generation)))
    })?
    else {
        return Ok(false);
    };

    let image = if image.is_empty() {
        include_bytes!("../../icons/32x32.png").to_vec()
    } else {
        image
    };

    let window = ctx.window();
    let ctx_cb = ctx.clone();
    let ctx_err = ctx.clone();
    window
        .clone()
        .run_on_main_thread(move || {
            let callback = move |result: drag::DragResult, _pos: drag::CursorPosition| {
                let ctx = ctx_cb.clone();
                match result {
                    drag::DragResult::Cancel => clear_outbound_dnd(&ctx, generation),
                    drag::DragResult::Dropped => {
                        // The OS drop event (self-drop path) and this callback
                        // arrive in platform-dependent order and there is no
                        // signal tying them together — a drop into another app
                        // never produces one. Grace period + generation guard
                        // make the clear idempotent either way.
                        tauri::async_runtime::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            clear_outbound_dnd(&ctx, generation);
                        });
                    }
                }
            };
            let item = drag::DragItem::Files(native);
            let image = drag::Image::Raw(image);
            let options = drag::Options {
                mode: drag::DragMode::Copy,
                ..Default::default()
            };

            #[cfg(not(target_os = "linux"))]
            let started = drag::start_drag(&window, item, image, callback, options)
                .map_err(|e| e.to_string());
            #[cfg(target_os = "linux")]
            let started = window
                .gtk_window()
                .map_err(|e| e.to_string())
                .and_then(|gtk| {
                    drag::start_drag(&gtk, item, image, callback, options)
                        .map_err(|e| e.to_string())
                });

            if let Err(e) = started {
                log::error!("failed to start native drag: {e}");
                clear_outbound_dnd(&ctx_err, generation);
            }
        })
        .map_err(|e| Error::Custom(format!("run_on_main_thread: {e}")))?;

    Ok(true)
}

fn clear_outbound_dnd(ctx: &MainWindowContext, generation: u64) {
    let _ = ctx.with_update(|gs| {
        let mut slot = gs.dnd.0.write();
        if slot
            .as_ref()
            .is_some_and(|d| d.outbound && d.generation == generation)
        {
            *slot = None;
        }
        Ok(())
    });
}

/// Handle files dropped from outside the app (OS file manager).
/// The dropped paths are host-local, so we need the host-local VFS to
/// construct VfsPaths from them.
#[tauri::command]
#[specta::specta]
pub async fn external_drop(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    subdirectory: Option<String>,
    paths: Vec<String>,
) -> Result<Option<OperationId>, Error> {
    // While an outbound native drag from this window is in flight, a drop
    // landing back here is our own drag — route it through internal DnD
    // semantics (deref'd VfsPath sources, copy, same-dir no-op) instead of
    // the OS paths. Taking the data consumes the session, so the drag
    // callback's delayed clear finds nothing to do.
    let self_drop = ctx.with_update(|gs| {
        let mut slot = gs.dnd.0.write();
        if slot.as_ref().is_some_and(|d| d.outbound) {
            let dnd = slot.take().unwrap();
            drop(slot);
            let (sources, source_path) = resolve_dnd_sources(gs, &dnd)?;
            Ok(Some((dnd, sources, source_path)))
        } else {
            Ok(None)
        }
    })?;

    if let Some((dnd, sources, source_path)) = self_drop {
        let dest_path = ctx
            .panes()
            .get(pane_handle)
            .ok_or_else(|| Error::Custom("pane not found".into()))?
            .path();
        // Dropping onto a row that is one of the dragged folders degrades to
        // a pane-background drop, mirroring the internal same-pane rule.
        let subdirectory = subdirectory.filter(|s| {
            !(pane_handle == dnd.source_pane && dnd.files.iter().any(|f| &f.name == s))
        });
        let destination = match subdirectory {
            Some(sub) => dest_path.join(&sub),
            None => dest_path,
        };
        if destination == source_path {
            return Ok(None);
        }
        let request = OperationRequest::Copy {
            rename_to: None,
            sources,
            destination,
            options: Default::default(),
        };
        return super::operations::start_operation(ctx, request)
            .await
            .map(Some);
    }

    let vfs_info = ctx.vfs_info()?;
    let host_vfs = vfs_info.host_local_vfs_id().ok_or_else(|| {
        Error::Custom(
            "No local filesystem mounted \u{2014} cannot accept external drops".to_string(),
        )
    })?;

    let sources: Vec<VfsPath> = paths
        .iter()
        .map(|p| {
            VfsPath::new(
                host_vfs,
                newt_common::vfs::local::local_path_from_native(std::path::Path::new(p)),
            )
        })
        .collect();

    let dest_path = ctx
        .panes()
        .get(pane_handle)
        .ok_or_else(|| Error::Custom("pane not found".into()))?
        .path();
    let destination = match subdirectory {
        Some(sub) => dest_path.join(&sub),
        None => dest_path,
    };

    let request = OperationRequest::Copy {
        rename_to: None,
        sources,
        destination,
        options: Default::default(),
    };

    super::operations::start_operation(ctx, request)
        .await
        .map(Some)
}
