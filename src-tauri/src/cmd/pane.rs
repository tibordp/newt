use newt_common::operation::{CopyOptions, OperationRequest};
use newt_common::vfs::{MountRequest, VfsPath};

use crate::common::Error;
use crate::main_window::pane::{FilterMode, Sorting};
use crate::main_window::{MainWindowContext, PaneHandle};

#[tauri::command]
pub fn cancel(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.cancel();
        Ok(())
    })
}

#[tauri::command]
pub async fn navigate(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    path: &str,
    exact: bool,
) -> Result<(), Error> {
    if !exact {
        // First try resolving as a VFS display path (handles s3://, etc.)
        let resolved = if let Some(vfs_path) = ctx.resolve_display_path(path) {
            Some(vfs_path)
        } else {
            // Try shell expansion (handles ~, env vars, etc.)
            let expanded = ctx.shell_service()?.shell_expand(path.to_string()).await?;
            if expanded.is_absolute() {
                Some(VfsPath::root(expanded))
            } else {
                // Relative path — will be resolved against the pane's current path
                None
            }
        };

        ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
            gs.close_modal();
            if let Some(target) = resolved {
                pane.navigate_to(target).await?;
            } else {
                // Resolve relative to the pane's current directory
                pane.navigate(path).await?;
            }
            Ok(())
        })
        .await
    } else {
        let path = path.to_string();
        ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
            gs.close_modal();
            pane.navigate(path).await?;
            Ok(())
        })
        .await
    }
}

#[tauri::command]
pub fn focus(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filename: Option<String>,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        if let Some(filename) = filename {
            state.view_state_mut().focus(filename);
        }
        gs.activate_pane(pane_handle);
        Ok(())
    })
}

#[tauri::command]
pub fn set_sorting(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    sorting: Sorting,
) -> Result<(), Error> {
    let folders_first = ctx.preferences().load().appearance.folders_first;
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().set_sorting(sorting, folders_first);
        Ok(())
    })
}

#[tauri::command]
pub fn toggle_selected(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filename: Option<String>,
    focus_next: bool,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().toggle_selected(filename, focus_next);
        Ok(())
    })
}

#[tauri::command]
pub fn select_range(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filename: String,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().select_range(filename);
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_select_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().select_all();
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_deselect_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().deselect_all();
        Ok(())
    })
}

#[tauri::command]
pub fn end_drag_selection(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    pane.view_state_mut().end_drag_selection();
    Ok(())
}

#[tauri::command]
pub fn set_selection_by_indices(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    start: usize,
    end: usize,
    additive: bool,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut()
            .set_selection_by_indices(start, end, additive);
        Ok(())
    })
}

#[tauri::command]
pub fn set_selection(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    selected: Vec<String>,
    focused: Option<String>,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut()
            .set_selection(selected.into_iter().collect(), focused);
        Ok(())
    })
}

#[tauri::command]
pub fn relative_jump(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    offset: i32,
    with_selection: bool,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().relative_jump(offset, with_selection);
        Ok(())
    })
}

#[tauri::command]
pub fn set_viewport(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    first_visible: usize,
    visible_count: usize,
) -> Result<(), Error> {
    let changed = {
        let pane = ctx.panes().get(pane_handle).unwrap();
        pane.view_state_mut()
            .set_viewport_hint(first_visible, visible_count)
    };
    if changed {
        ctx.publish()?;
    }
    Ok(())
}

#[tauri::command]
pub fn set_filter(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filter: Option<String>,
    mode: Option<FilterMode>,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        if let Some(mode) = mode {
            pane.view_state_mut().set_filter_with_mode(filter, mode);
        } else {
            pane.view_state_mut().set_filter(filter);
        }
        Ok(())
    })
}

#[tauri::command]
pub async fn cmd_as_other_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    ctx.with_update_async(|gs| async move { gs.as_other_pane(pane_handle).await })
        .await
}

pub async fn cmd_open_in_other_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    target: PaneHandle,
) -> Result<(), Error> {
    if pane_handle == target {
        return Ok(());
    }

    let pane = ctx.panes().get(pane_handle).unwrap();
    let pane_path = pane.path();
    let file = match pane.get_focused_file_info() {
        Some(f) => f,
        None => return Ok(()),
    };

    let mut target_path = match file.name.as_str() {
        ".." => pane_path.parent().unwrap_or(pane_path),
        _ => match pane.get_focused_file() {
            Some(s) => s,
            None => return Ok(()),
        },
    };

    if newt_common::vfs::is_archive_name(&file.name) {
        let response = ctx
            .mount_vfs(MountRequest::Archive {
                origin: target_path.clone(),
            })
            .await?;
        target_path = VfsPath::new(response.vfs_id, "/");
    }

    ctx.with_pane_update_async(target, |_gs, pane| async move {
        pane.navigate_to(target_path).await?;
        Ok(())
    })
    .await?;

    Ok(())
}

#[tauri::command]
pub async fn cmd_open_in_left_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    cmd_open_in_other_pane(ctx, pane_handle, PaneHandle::left()).await
}

#[tauri::command]
pub async fn cmd_open_in_right_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    cmd_open_in_other_pane(ctx, pane_handle, PaneHandle::right()).await
}

#[tauri::command]
pub async fn enter(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let file = match pane.get_focused_file_info() {
        Some(f) => f,
        None => return Ok(()),
    };

    if file.name == ".." || file.is_dir {
        return navigate(ctx, pane_handle, &file.name, true).await;
    }

    if newt_common::vfs::is_archive_name(&file.name) {
        return cmd_open_archive(ctx, pane_handle).await;
    }

    // Default: open with system handler
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return Ok(()),
    };

    // Open through shell if on local VFS
    if ctx.vfs_info()?.is_host_local(full_path.vfs_id) {
        opener::open(&full_path.path)?;
    } else {
        download_and_open(&ctx, full_path, &file.name).await?;
    }

    Ok(())
}

/// Download a file from a non-host-local VFS to a temp directory on the host,
/// then open it with the system's default handler when the copy completes.
async fn download_and_open(
    ctx: &MainWindowContext,
    source: VfsPath,
    filename: &str,
) -> Result<(), Error> {
    let vfs_info = ctx.vfs_info()?;
    let host_vfs = vfs_info.host_local_vfs_id().ok_or_else(|| {
        Error::Custom("No local filesystem mounted — cannot open files externally".to_string())
    })?;

    let temp_dir = tempfile::tempdir_in(std::env::temp_dir())?.keep();
    let dest_path = temp_dir.join(filename);
    let dest_vfs_path = VfsPath::new(host_vfs, temp_dir.to_string_lossy().to_string());

    let op_id = super::operations::start_operation(
        ctx.clone(),
        OperationRequest::Copy {
            sources: vec![source],
            destination: dest_vfs_path,
            options: CopyOptions::default(),
        },
    )
    .await?;

    ctx.operations().register_completion_callback(
        op_id,
        Box::new(move || {
            let _ = opener::open(&dest_path);
        }),
    );

    Ok(())
}

#[tauri::command]
pub async fn cmd_open(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    enter(ctx, pane_handle).await
}

#[tauri::command]
pub async fn cmd_open_archive(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let origin = match pane.get_focused_file() {
        Some(s) => s,
        None => return Ok(()),
    };

    let response = ctx
        .mount_vfs(MountRequest::Archive {
            origin: origin.clone(),
        })
        .await?;
    let vfs_path = VfsPath::new(response.vfs_id, "/");

    ctx.with_pane_update_async(pane_handle, |_gs, pane| async move {
        pane.navigate_to(vfs_path).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cmd_follow_symlink(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let target = match pane.get_focused_symlink_target() {
        Some(t) => t,
        None => return Ok(()),
    };

    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        let resolved = if target.is_absolute() {
            target
        } else {
            pane.path().path.join(&target)
        };
        let parent = resolved.parent().unwrap_or(&resolved).to_path_buf();
        let filename = resolved
            .file_name()
            .map(|n: &std::ffi::OsStr| n.to_string_lossy().to_string());
        pane.navigate(&parent).await?;
        if let Some(name) = filename {
            pane.view_state_mut().focus(name);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cmd_open_folder(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let full_path = pane.path();

    if ctx.vfs_info()?.is_host_local(full_path.vfs_id) {
        opener::open(&full_path.path)?;
    }

    Ok(())
}

#[tauri::command]
pub async fn cmd_navigate_back(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    ctx.with_pane_update_async(
        pane_handle,
        |_, pane| async move { pane.navigate_back().await },
    )
    .await
}

#[tauri::command]
pub async fn cmd_navigate_forward(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        pane.navigate_forward().await
    })
    .await
}

#[tauri::command]
pub async fn navigate_history(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    target_index: usize,
) -> Result<(), Error> {
    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_history(target_index).await
    })
    .await
}

#[tauri::command]
pub fn cmd_toggle_hidden(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        c.toggle_hidden();
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_copy_to_clipboard(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();

    #[cfg(windows)]
    const LINE_ENDING: &'static str = "\r\n";
    #[cfg(not(windows))]
    const LINE_ENDING: &str = "\n";

    let mut text = String::new();
    for (idx, line) in pane.get_effective_selection().into_iter().enumerate() {
        if idx != 0 {
            text.push_str(LINE_ENDING);
        }
        text.push_str(&ctx.format_vfs_path(&line));
    }

    ctx.clipboard().set_text(text)?;

    Ok(())
}

#[tauri::command]
pub async fn cmd_paste_from_clipboard(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let mut clipboard = arboard::Clipboard::new()?;
    let text = clipboard.get_text()?;
    let text = text.trim();

    // Same resolution chain as the navigate command with exact: false
    let resolved = if let Some(vfs_path) = ctx.resolve_display_path(text) {
        Some(vfs_path)
    } else {
        let expanded = ctx.shell_service()?.shell_expand(text.to_string()).await?;
        if expanded.is_absolute() {
            Some(VfsPath::root(expanded))
        } else {
            None
        }
    };

    let text = text.to_string();
    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        if let Some(target) = resolved {
            pane.navigate_to(target).await?;
        } else {
            pane.navigate(text).await?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cmd_refresh(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    pane.refresh(None, true).await?;
    ctx.publish()?;
    Ok(())
}
