use newt_common::operation::{OperationId, OperationRequest};
use newt_common::vfs::VfsPath;

use crate::common::Error;
use crate::main_window::{DndData, DndFile, MainWindowContext, PaneHandle};

#[tauri::command]
#[specta::specta]
pub fn start_dnd(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    files: Vec<DndFile>,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        *gs.dnd.0.write() = Some(DndData {
            source_pane: pane_handle,
            files,
        });
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn cancel_dnd(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_update(|gs| {
        *gs.dnd.0.write() = None;
        Ok(())
    })
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

        let source_pane = gs
            .panes
            .get(dnd_data.source_pane)
            .ok_or_else(|| Error::Custom("source pane not found".into()))?;
        let dest_pane = gs
            .panes
            .get(destination_pane)
            .ok_or_else(|| Error::Custom("destination pane not found".into()))?;

        let source_path = source_pane.path();
        // Deref via the source pane's view of each entry so dragging from
        // a SearchVfs operates on the real underlying files.
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

        Ok((sources, dest_pane.path()))
    })?;

    let destination = match subdirectory {
        Some(sub) => dest_path.join(&sub),
        None => dest_path,
    };

    let request = if is_move {
        OperationRequest::Move {
            sources,
            destination,
            options: Default::default(),
        }
    } else {
        OperationRequest::Copy {
            sources,
            destination,
            options: Default::default(),
        }
    };

    super::operations::start_operation(ctx, request).await
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
) -> Result<OperationId, Error> {
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
        sources,
        destination,
        options: Default::default(),
    };

    super::operations::start_operation(ctx, request).await
}
