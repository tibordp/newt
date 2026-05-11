use newt_common::file_reader::{FileChunk, FileDetails};
use newt_common::operation::{
    CopyOptions, IssueAction, IssueResponse, OperationId, OperationRequest, ResolveIssueRequest,
    StartOperationRequest,
};
use newt_common::vfs::VfsPath;
use tauri::Manager;

use crate::GlobalContext;
use crate::common::Error;
use crate::main_window::{
    ConfirmAction, MainWindowContext, ModalContext, ModalData, ModalDataKind, OperationState,
    OperationStatus, PaneHandle,
};

#[tauri::command]
#[specta::specta]
pub async fn create_directory(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    path: VfsPath,
    name: String,
) -> Result<(), Error> {
    let dir_path = path.join(&name);

    ctx.fs()?.create_directory(dir_path).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None, true).await?;
            pane.view_state_mut().focus(name);
        }

        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn touch_file(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    path: VfsPath,
    name: String,
    open_editor: Option<bool>,
) -> Result<(), Error> {
    let file_path = path.join(&name);

    ctx.fs()?.touch(file_path.clone()).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None, true).await?;
            pane.view_state_mut().focus(name);
        }

        Ok(())
    })
    .await?;

    if open_editor.unwrap_or(false) {
        super::window::open_editor_window(&ctx, &file_path)?;
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_delete_selected(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let paths = pane.get_effective_selection();
    if paths.is_empty() {
        return Ok(());
    }

    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<GlobalContext> = app_handle.state();
    let prefs = global_ctx.preferences().settings();

    if prefs.behavior.confirm_delete {
        let message = if paths.len() > 1 {
            format!("Delete {} selected files?", paths.len())
        } else {
            let name = paths[0]
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            format!("Delete {}?", name)
        };
        ctx.with_update(|gs| {
            *gs.modal.0.write() = Some(ModalData {
                kind: ModalDataKind::Confirm {
                    message,
                    action: ConfirmAction::DeleteSelected { paths },
                },
                context: ModalContext {
                    pane_handle: Some(pane_handle),
                },
            });
            Ok(())
        })
    } else {
        let request = OperationRequest::Delete { paths };
        start_operation(ctx, request).await?;
        Ok(())
    }
}

#[tauri::command]
#[specta::specta]
pub async fn rename(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    base_path: VfsPath,
    old_name: String,
    new_name: String,
) -> Result<(), Error> {
    let old_path = base_path.join(&old_name);
    let new_path = base_path.join(&new_name);

    ctx.fs()?.rename(old_path, new_path).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None, true).await?;
            pane.view_state_mut().focus(new_name);
        }

        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub async fn set_metadata(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    paths: Vec<VfsPath>,
    mode_set: u32,
    mode_clear: u32,
    uid: Option<u32>,
    gid: Option<u32>,
    recursive: bool,
) -> Result<(), Error> {
    let request = OperationRequest::SetMetadata {
        paths,
        mode_set,
        mode_clear,
        uid,
        gid,
        recursive,
    };
    start_operation(ctx.clone(), request).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None, true).await?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn start_operation(
    ctx: MainWindowContext,
    request: OperationRequest,
) -> Result<OperationId, Error> {
    let id = ctx.next_operation_id()?;

    let (kind, description) = match &request {
        OperationRequest::Copy {
            sources,
            destination,
            ..
        } => (
            "copy".to_string(),
            format!(
                "Copying {} item(s) to {}",
                sources.len(),
                ctx.format_vfs_path(destination),
            ),
        ),
        OperationRequest::Move {
            sources,
            destination,
            ..
        } => (
            "move".to_string(),
            format!(
                "Moving {} item(s) to {}",
                sources.len(),
                ctx.format_vfs_path(destination),
            ),
        ),
        OperationRequest::Delete { paths } => (
            "delete".to_string(),
            format!("Deleting {} item(s)", paths.len()),
        ),
        OperationRequest::SetMetadata { paths, .. } => (
            "chmod".to_string(),
            format!("Setting metadata on {} item(s)", paths.len()),
        ),
        OperationRequest::RunCommand { command, .. } => {
            ("command".to_string(), format!("Running: {}", command))
        }
        OperationRequest::DebugSleep { duration_seconds } => (
            "debug_sleep".to_string(),
            format!("Debug sleep ({}s)", duration_seconds),
        ),
    };

    // Insert initial operation state
    {
        let mut ops = ctx.operations().state.write();
        ops.insert(
            id,
            OperationState {
                id,
                kind,
                description,
                total_bytes: None,
                total_items: None,
                bytes_done: 0,
                items_done: 0,
                current_item: String::new(),
                status: OperationStatus::Scanning,
                error: None,
                issue: None,
                backgrounded: false,
                scanning_items: None,
                scanning_bytes: None,
            },
        );
    }
    ctx.publish()?;

    // Send to operations client
    let req = StartOperationRequest { id, request };
    if let Err(e) = ctx.operations_client()?.start_operation(req).await {
        // Operation failed to start — mark as failed so it doesn't get stuck
        let mut ops = ctx.operations().state.write();
        if let Some(op) = ops.get_mut(&id) {
            op.status = OperationStatus::Failed;
            op.error = Some(e.to_string());
        }
        ctx.publish()?;
        return Err(e.into());
    }

    Ok(id)
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_operation(
    ctx: MainWindowContext,
    operation_id: OperationId,
) -> Result<(), Error> {
    ctx.operations_client()?
        .cancel_operation(operation_id)
        .await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn resolve_issue(
    ctx: MainWindowContext,
    operation_id: OperationId,
    issue_id: u64,
    action: IssueAction,
    apply_to_all: bool,
) -> Result<(), Error> {
    let req = ResolveIssueRequest {
        operation_id,
        issue_id,
        response: IssueResponse {
            action,
            apply_to_all,
        },
    };

    ctx.operations_client()?.resolve_issue(req).await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn dismiss_operation(ctx: MainWindowContext, operation_id: OperationId) -> Result<(), Error> {
    {
        let mut ops = ctx.operations().state.write();
        if let Some(op) = ops.get(&operation_id) {
            match op.status {
                OperationStatus::Completed
                | OperationStatus::Failed
                | OperationStatus::Cancelled => {
                    ops.remove(&operation_id);
                }
                _ => {
                    return Err(Error::Custom(
                        "Cannot dismiss an active operation".to_string(),
                    ));
                }
            }
        }
    }
    ctx.publish()?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn background_operation(
    ctx: MainWindowContext,
    operation_id: OperationId,
) -> Result<(), Error> {
    {
        let mut ops = ctx.operations().state.write();
        if let Some(op) = ops.get_mut(&operation_id) {
            op.backgrounded = true;
        }
    }
    ctx.publish()?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn foreground_operation(
    ctx: MainWindowContext,
    operation_id: OperationId,
) -> Result<(), Error> {
    {
        let mut ops = ctx.operations().state.write();
        if let Some(op) = ops.get_mut(&operation_id) {
            op.backgrounded = false;
        }
    }
    ctx.publish()?;
    Ok(())
}

/// Show the next operation in foreground, cycling by op id. If an op is
/// currently foregrounded, send it to the background first and surface the
/// next one (wrapping). If none is foregrounded, surface the oldest
/// backgrounded op. No-op when no operations exist.
#[tauri::command]
#[specta::specta]
pub fn cmd_show_next_operation(
    ctx: MainWindowContext,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    {
        let mut ops = ctx.operations().state.write();
        let mut ids: Vec<OperationId> = ops.keys().copied().collect();
        if ids.is_empty() {
            return Ok(());
        }
        ids.sort();

        let current_foreground = ids
            .iter()
            .copied()
            .find(|id| ops.get(id).is_some_and(|op| !op.backgrounded));

        let next_id = match current_foreground {
            Some(current) => {
                let idx = ids.iter().position(|&id| id == current).unwrap();
                ids[(idx + 1) % ids.len()]
            }
            None => ids[0],
        };

        if Some(next_id) == current_foreground {
            return Ok(()); // single op; nothing to cycle to
        }

        if let Some(current) = current_foreground
            && let Some(op) = ops.get_mut(&current)
        {
            op.backgrounded = true;
        }
        if let Some(op) = ops.get_mut(&next_id) {
            op.backgrounded = false;
        }
    }
    ctx.publish()?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn start_copy_move(
    ctx: MainWindowContext,
    kind: String,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    options: CopyOptions,
) -> Result<OperationId, Error> {
    let request = match kind.as_str() {
        "copy" => OperationRequest::Copy {
            sources,
            destination,
            options,
        },
        "move" => OperationRequest::Move {
            sources,
            destination,
            options,
        },
        _ => return Err(Error::Custom(format!("unknown copy/move kind: {}", kind))),
    };

    ctx.with_update(|gs| {
        gs.close_modal();
        Ok(())
    })?;

    start_operation(ctx, request).await
}

#[tauri::command]
#[specta::specta]
pub async fn confirm_action(ctx: MainWindowContext) -> Result<(), Error> {
    let action = ctx.with_update(|gs| {
        let modal = gs.modal.0.read().clone();
        let modal = modal.ok_or_else(|| Error::Custom("no modal open".into()))?;
        let action = match modal {
            ModalData {
                kind: ModalDataKind::Confirm { action, .. },
                ..
            } => action,
            _ => return Err(Error::Custom("modal is not a confirm dialog".into())),
        };
        gs.close_modal();
        Ok(action)
    })?;

    match action {
        ConfirmAction::DeleteSelected { paths } => {
            let request = OperationRequest::Delete { paths };
            start_operation(ctx, request).await?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// File reader: viewer/editor support (read/write file ranges)
// ---------------------------------------------------------------------------

#[tauri::command]
#[specta::specta]
pub async fn file_details(ctx: MainWindowContext, path: VfsPath) -> Result<FileDetails, Error> {
    let info = ctx.file_reader()?.file_details(path).await?;
    Ok(info)
}

#[tauri::command]
#[specta::specta]
pub async fn read_file_range(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    length: u64,
) -> Result<FileChunk, Error> {
    let chunk = ctx.file_reader()?.read_range(path, offset, length).await?;
    Ok(chunk)
}

#[tauri::command]
#[specta::specta]
pub async fn read_file(
    ctx: MainWindowContext,
    path: VfsPath,
    max_size: u64,
) -> Result<Vec<u8>, Error> {
    let data = ctx.file_reader()?.read_file(path, max_size).await?;
    Ok(data)
}

#[tauri::command]
#[specta::specta]
pub async fn write_file(ctx: MainWindowContext, path: VfsPath, data: Vec<u8>) -> Result<(), Error> {
    ctx.file_reader()?.write_file(path, data).await?;
    Ok(())
}

/// Kick off a synthetic long-running operation. Reachable only from the
/// Debug modal, itself unavailable outside `debug_assertions` builds.
#[tauri::command]
#[specta::specta]
pub async fn cmd_debug_run_test_operation(
    ctx: MainWindowContext,
    duration_seconds: u64,
) -> Result<OperationId, Error> {
    start_operation(ctx, OperationRequest::DebugSleep { duration_seconds }).await
}
