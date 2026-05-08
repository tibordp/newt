use newt_common::vfs::{MountRequest, VfsId, VfsPath, lookup_descriptor};

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
        MountRequest::Remote => "remote",
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
    let vfs_path = VfsPath::new(response.vfs_id, "/");

    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_to(vfs_path).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
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
pub async fn mount_sftp(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    host: String,
) -> Result<(), Error> {
    mount_and_navigate(ctx, pane_handle, MountRequest::Sftp { host }).await
}

#[tauri::command]
pub async fn mount_k8s(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    context: String,
) -> Result<(), Error> {
    mount_and_navigate(ctx, pane_handle, MountRequest::Kubernetes { context }).await
}

#[tauri::command]
pub async fn switch_vfs(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    vfs_id: Option<VfsId>,
    type_name: String,
) -> Result<(), Error> {
    let vfs_path = if let Some(id) = vfs_id {
        VfsPath::new(id, "/")
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
        VfsPath::new(response.vfs_id, "/")
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
            pane.navigate_to(VfsPath::root("/")).await?;
        }
    }
    ctx.unmount_vfs(vfs_id).await
}

#[tauri::command]
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
