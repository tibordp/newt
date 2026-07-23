use newt_common::vfs::{MountRequest, VfsId, VfsPath, lookup_descriptor, search::SearchParams};
use tauri::Manager;

use crate::common::Error;
use crate::main_window::{MainWindowContext, PaneHandle};

/// Mount a VFS, then close the originating modal and navigate `pane_handle`
/// to the new mount root. With `replace` the navigation takes over the
/// pane's current history entry instead of pushing a new one (search
/// refinement — the superseded search shouldn't linger in history).
async fn mount_and_navigate(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    request: MountRequest,
    replace: bool,
) -> Result<(), Error> {
    // The mount dialogs surface the mount log while connecting — show only
    // the attempt in flight.
    ctx.clear_mount_log();
    // Log the variant discriminant only — the S3 variant carries credentials,
    // and Debug-formatting the whole request would leak them into logs.
    let kind = match &request {
        MountRequest::S3 { .. } => "s3",
        MountRequest::Sftp { .. } => "sftp",
        MountRequest::Kubernetes { .. } => "k8s",
        MountRequest::Archive { .. } => "archive",
        MountRequest::Disc { .. } => "disc",
        MountRequest::Search { .. } => "search",
        MountRequest::Remote { .. } => "remote",
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
        if replace {
            pane.navigate_to_replace(vfs_path).await?;
        } else {
            pane.navigate_to(vfs_path).await?;
        }
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
    let app_handle = ctx.window().app_handle().clone();
    // Secret-free target for the recents MRU; the mode is recovered from
    // which credential fields the dialog filled in.
    let credential_mode = if credentials.access_key_id.is_some() {
        "iam_user"
    } else if credentials.role_arn.is_some() {
        "assume_role"
    } else if credentials.profile.is_some() {
        "profile"
    } else {
        "default"
    };
    let recent_kind = crate::connections::ConnectionKind::S3 {
        region: region.clone(),
        bucket: bucket.clone(),
        endpoint_url: credentials.endpoint_url.clone(),
        credential_mode: credential_mode.to_string(),
        profile: credentials.profile.clone(),
        role_arn: credentials.role_arn.clone(),
        external_id: credentials.external_id.clone(),
    };
    mount_and_navigate(
        ctx,
        pane_handle,
        MountRequest::S3 {
            region,
            bucket,
            credentials,
        },
        false,
    )
    .await?;
    crate::connections::record_recent(&app_handle, recent_kind, crate::connections::OpenIn::Pane);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn mount_sftp(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    host: String,
) -> Result<(), Error> {
    let app_handle = ctx.window().app_handle().clone();
    mount_and_navigate(
        ctx,
        pane_handle,
        MountRequest::Sftp { host: host.clone() },
        false,
    )
    .await?;
    // SFTP always mounts into the pane; record only on a successful mount.
    crate::connections::record_recent(
        &app_handle,
        crate::connections::ConnectionKind::Sftp { host },
        crate::connections::OpenIn::Pane,
    );
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn mount_k8s(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    context: String,
) -> Result<(), Error> {
    mount_and_navigate(
        ctx,
        pane_handle,
        MountRequest::Kubernetes { context },
        false,
    )
    .await
}

/// Submit handler for the search dialog. Builds a `SearchVfs` rooted at
/// `root` with the supplied parameters and navigates the pane to its
/// mount root. `name_pattern` is a glob (`*.rs`, `Cargo.*`, …);
/// `content_*` together optionally specify a content match (one of
/// literal substring or regex). Empty strings are treated as "not set".
/// Pattern compilation and validation happen mount-side (`search::mount`),
/// so this just forwards the raw dialog values.
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
    let params = SearchParams {
        name_pattern: name_pattern.filter(|s| !s.is_empty()),
        content_pattern: content_pattern.filter(|s| !s.is_empty()),
        content_is_regex,
        case_sensitive,
        follow_symlinks,
        ..SearchParams::default()
    };

    // If the pane is already inside a search, this mount is a refinement
    // of it — the new results take over the current history entry rather
    // than stacking on top (and the superseded mount gets auto-unmounted
    // once nothing references it).
    let replace = ctx
        .vfs_info()
        .ok()
        .zip(ctx.panes().get(pane_handle))
        .and_then(|(vi, pane)| {
            let (desc, meta) = vi.descriptor(pane.path().vfs_id)?;
            desc.search_params(&meta)
        })
        .is_some();

    mount_and_navigate(
        ctx,
        pane_handle,
        MountRequest::Search { root, params },
        replace,
    )
    .await
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

// ---------------------------------------------------------------------------
// Map / Unmap network drive (Windows). Doc comments on the #[cfg(windows)]
// and stub definitions must stay identical: tauri-specta emits them as
// JSDoc, so a mismatch makes `bindings.ts` depend on the build host.
// ---------------------------------------------------------------------------

/// Open the system "Map Network Drive" wizard (F11).
#[cfg(windows)]
#[tauri::command]
#[specta::specta]
pub async fn cmd_map_network_drive(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    crate::main_window::drives::map_network_drive(&ctx, pane_handle).await
}

/// Confirm-and-disconnect the network drive the pane is on (Alt+F11).
#[cfg(windows)]
#[tauri::command]
#[specta::specta]
pub async fn cmd_unmap_network_drive(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    crate::main_window::drives::open_unmap_confirmation(&ctx, pane_handle)
}

/// The unmap confirmation dialog's "Disconnect" button.
#[cfg(windows)]
#[tauri::command]
#[specta::specta]
pub async fn confirm_unmap_drive(ctx: MainWindowContext) -> Result<(), Error> {
    crate::main_window::drives::confirm_unmap_drive(&ctx).await
}

/// Open the system "Map Network Drive" wizard (F11).
#[cfg(not(windows))]
#[tauri::command]
#[specta::specta]
pub async fn cmd_map_network_drive(_pane_handle: PaneHandle) -> Result<(), Error> {
    Err(Error::Custom(
        "Network drive mapping is only available on Windows".into(),
    ))
}

/// Confirm-and-disconnect the network drive the pane is on (Alt+F11).
#[cfg(not(windows))]
#[tauri::command]
#[specta::specta]
pub async fn cmd_unmap_network_drive(_pane_handle: PaneHandle) -> Result<(), Error> {
    Err(Error::Custom(
        "Network drive mapping is only available on Windows".into(),
    ))
}

/// The unmap confirmation dialog's "Disconnect" button.
#[cfg(not(windows))]
#[tauri::command]
#[specta::specta]
pub async fn confirm_unmap_drive() -> Result<(), Error> {
    Err(Error::Custom(
        "Network drive mapping is only available on Windows".into(),
    ))
}
