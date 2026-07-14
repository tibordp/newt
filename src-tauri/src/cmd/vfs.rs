use newt_common::file_reader::SearchPattern;
use newt_common::vfs::{MountRequest, VfsId, VfsPath, lookup_descriptor, search::SearchParams};

use crate::common::Error;
use crate::main_window::{MainWindowContext, PaneHandle};

/// Mount a VFS, then close the originating modal and navigate `pane_handle`
/// to the new mount root.
async fn mount_and_navigate(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    request: MountRequest,
) -> Result<(), Error> {
    // Log the variant discriminant only — the S3 variant carries credentials,
    // and Debug-formatting the whole request would leak them into logs.
    let kind = match &request {
        MountRequest::S3 { .. } => "s3",
        MountRequest::Sftp { .. } => "sftp",
        MountRequest::Kubernetes { .. } => "k8s",
        MountRequest::Archive { .. } => "archive",
        MountRequest::Search { .. } => "search",
        MountRequest::Remote => "remote",
        MountRequest::Agent { .. } => "agent",
    };
    log::info!("cmd: mount {} pane={:?}", kind, pane_handle);
    let response = ctx.mount_vfs(request).await.inspect_err(|e| {
        log::error!("cmd: mount {} failed: {}", kind, e);
    })?;
    log::info!(
        "cmd: mount {} succeeded, vfs_id={:?}",
        kind,
        response.vfs_id
    );
    let vfs_path = ctx.vfs_initial_path(response.vfs_id);

    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_to(vfs_path).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn mount_s3(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    region: Option<String>,
    bucket: Option<String>,
    credentials: newt_common::vfs::S3Credentials,
) -> Result<(), Error> {
    mount_and_navigate(
        ctx,
        pane_handle,
        MountRequest::S3 {
            region,
            bucket,
            credentials,
        },
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn mount_sftp(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    host: String,
) -> Result<(), Error> {
    mount_and_navigate(ctx, pane_handle, MountRequest::Sftp { host }).await
}

#[tauri::command]
#[specta::specta]
pub async fn mount_k8s(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    context: String,
) -> Result<(), Error> {
    mount_and_navigate(ctx, pane_handle, MountRequest::Kubernetes { context }).await
}

/// Submit handler for the search dialog. Builds a `SearchVfs` rooted at
/// `root` with the supplied parameters and navigates the pane to its
/// mount root. `name_pattern` is a glob (`*.rs`, `Cargo.*`, …);
/// `content_*` together optionally specify a content match (one of
/// literal substring or regex). Empty strings are treated as "not set".
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub async fn mount_search(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    root: VfsPath,
    name_pattern: Option<String>,
    content_pattern: Option<String>,
    content_is_regex: bool,
    case_sensitive: bool,
    follow_symlinks: bool,
) -> Result<(), Error> {
    let name_pattern = name_pattern.filter(|s| !s.is_empty());
    let content_pattern = content_pattern.filter(|s| !s.is_empty()).map(|s| {
        if content_is_regex {
            // Regex case-insensitivity is encoded inline rather than via a
            // separate flag; wrap in `(?i)` when the dialog says so.
            if case_sensitive {
                SearchPattern::Regex(s)
            } else {
                SearchPattern::Regex(format!("(?i){}", s))
            }
        } else if case_sensitive {
            SearchPattern::Literal(s.into_bytes())
        } else {
            // Case-insensitive literal — turn it into a regex with `(?i)`
            // and escape regex metacharacters.
            SearchPattern::Regex(format!("(?i){}", regex::escape(&s)))
        }
    });

    let params = SearchParams {
        name_pattern,
        content_pattern,
        case_sensitive,
        follow_symlinks,
        ..SearchParams::default()
    };

    mount_and_navigate(ctx, pane_handle, MountRequest::Search { root, params }).await
}

#[tauri::command]
#[specta::specta]
pub async fn switch_vfs(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    vfs_id: Option<VfsId>,
    type_name: String,
    root: Option<newt_common::vfs::path::PathBuf>,
) -> Result<(), Error> {
    let vfs_path = if let Some(id) = vfs_id {
        // A split-root entry carries the exact drive to land on;
        // otherwise use the VFS's default landing path.
        match root {
            Some(r) => VfsPath::new(id, r),
            None => ctx.vfs_initial_path(id),
        }
    } else {
        let descriptor = lookup_descriptor(&type_name)
            .ok_or_else(|| Error::Custom(format!("unknown VFS type: {}", type_name)))?;
        let request = descriptor.auto_mount_request().ok_or_else(|| {
            Error::Custom(format!(
                "VFS type {} does not support auto-mount",
                type_name
            ))
        })?;
        let response = ctx.mount_vfs(request).await?;
        ctx.vfs_initial_path(response.vfs_id)
    };

    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_to(vfs_path).await?;
        Ok(())
    })
    .await
}

/// Redirect any pane on `vfs_id` back to the local root, then unmount.
async fn redirect_and_unmount(ctx: &MainWindowContext, vfs_id: VfsId) -> Result<(), Error> {
    for pane in ctx.panes().all() {
        if pane.path().vfs_id == vfs_id {
            pane.navigate_to(ctx.vfs_initial_path(VfsId::ROOT)).await?;
        }
    }
    ctx.unmount_vfs(vfs_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_unmount_vfs(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx
        .panes()
        .get(pane_handle)
        .ok_or_else(|| Error::Custom("pane not found".into()))?;
    let vfs_id = pane.path().vfs_id;
    if vfs_id == VfsId::ROOT {
        return Ok(());
    }

    redirect_and_unmount(&ctx, vfs_id).await?;
    let _ = ctx.publish();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn unmount_vfs(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    vfs_id: VfsId,
) -> Result<(), Error> {
    redirect_and_unmount(&ctx, vfs_id).await?;

    // Close the modal (VFS selector dropdown) and refresh
    ctx.with_pane_update(pane_handle, |gs, _pane| {
        gs.close_modal();
        Ok(())
    })?;

    Ok(())
}
