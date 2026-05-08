use newt_common::operation::{OperationId, OperationRequest};
use newt_common::vfs::VfsPath;

use crate::common::Error;
use crate::main_window::{DndData, DndFile, MainWindowContext, PaneHandle};

#[tauri::command]
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
pub fn cancel_dnd(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_update(|gs| {
        *gs.dnd.0.write() = None;
        Ok(())
    })
}

#[tauri::command]
pub async fn execute_dnd(
    ctx: MainWindowContext,
    destination_pane: PaneHandle,
    subdirectory: Option<String>,
    is_move: bool,
) -> Result<OperationId, Error> {
    let (source_path, dest_path, dnd_files) = ctx.with_update(|gs| {
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

        Ok((source_pane.path(), dest_pane.path(), dnd_data.files))
    })?;

    let destination = match subdirectory {
        Some(sub) => dest_path.join(&sub),
        None => dest_path,
    };
    let sources: Vec<VfsPath> = dnd_files
        .iter()
        .map(|f| source_path.join(&f.name))
        .collect();

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

    let sources: Vec<VfsPath> = paths.iter().map(|p| VfsPath::new(host_vfs, p)).collect();

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
