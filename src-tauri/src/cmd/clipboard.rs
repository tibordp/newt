//! The file clipboard: Copy, Cut and Paste of a pane's selection. Within a
//! session, paste goes through the session's record, so files on any VFS
//! paste; other apps and other sessions see only what the OS clipboard can
//! hold — this machine's files, and the paths of everything.

use newt_common::operation::OperationRequest;
use newt_common::vfs::{VfsId, VfsPath};

use crate::clipboard::Stamp;
use crate::common::Error;
use crate::main_window::{ClipboardRecord, MainWindowContext, PaneHandle};
use crate::runtime_state::CopyMoveDefaults;

#[tauri::command]
#[specta::specta]
pub async fn cmd_copy_to_clipboard(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    put_selection(ctx, pane_handle, false).await
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_cut_to_clipboard(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    put_selection(ctx, pane_handle, true).await
}

/// The selection's display paths as text, and nothing else.
#[tauri::command]
#[specta::specta]
pub async fn cmd_copy_path(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let sources = ctx
        .panes()
        .require(pane_handle)?
        .get_effective_selection_dereferenced();
    ctx.clipboard()
        .set_text(display_paths(&ctx, &sources))
        .await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_paste_from_clipboard(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    paste(ctx, pane_handle, false).await
}

/// Paste as a move, whatever was copied (Finder's ⌥⌘V).
#[tauri::command]
#[specta::specta]
pub async fn cmd_move_from_clipboard(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    paste(ctx, pane_handle, true).await
}

/// Put the selection on the clipboard — as files where they're on this
/// machine, as paths otherwise — and in the session's record.
async fn put_selection(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    cut: bool,
) -> Result<(), Error> {
    let sources = ctx
        .panes()
        .require(pane_handle)?
        .get_effective_selection_dereferenced();
    if sources.is_empty() {
        return Ok(());
    }
    let text = display_paths(&ctx, &sources);
    let vfs_info = ctx.vfs_info()?;
    let clipboard = ctx.clipboard();
    // A selection is on one VFS: this machine's throughout, or not at all.
    let stamp = if sources.iter().all(|s| vfs_info.is_host_local(s.vfs_id)) {
        let paths = sources.iter().map(|s| s.path.to_native()).collect();
        clipboard.set_files(paths, text, cut).await?
    } else {
        clipboard.set_text(text).await?
    };
    *ctx.clipboard_record().lock() = Some(ClipboardRecord {
        sources,
        cut,
        stamp,
    });
    ctx.sync_cut_marks()
}

fn display_paths(ctx: &MainWindowContext, sources: &[VfsPath]) -> String {
    #[cfg(windows)]
    const LINE_ENDING: &str = "\r\n";
    #[cfg(not(windows))]
    const LINE_ENDING: &str = "\n";
    sources
        .iter()
        .map(|source| ctx.format_vfs_path(source))
        .collect::<Vec<_>>()
        .join(LINE_ENDING)
}

/// Paste into the pane's folder: the session's own record if the
/// clipboard still holds it, else files another app (or session) put
/// there, else a path to go to.
async fn paste(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    as_move: bool,
) -> Result<(), Error> {
    let clipboard = ctx.clipboard();
    let stamp = clipboard.stamp().await;
    let recorded = ctx
        .clipboard_record()
        .lock()
        .as_ref()
        .filter(|record| record.stamp == stamp)
        .map(|record| (record.sources.clone(), record.cut));
    if let Some((sources, cut)) = recorded {
        return paste_files(
            &ctx,
            pane_handle,
            sources,
            as_move || cut,
            cut.then_some(stamp),
        )
        .await;
    }
    if let Some(files) = clipboard.files().await? {
        let Some(host) = ctx.vfs_info()?.host_local_vfs_id() else {
            return Err(Error::Custom(
                "This session has no access to the files of the computer running Newt \
                 (\"Allow access to this host\" when connecting)"
                    .into(),
            ));
        };
        let sources = files
            .paths
            .iter()
            .map(|path| VfsPath::new(host, newt_common::vfs::path::PathBuf::from_native(path)))
            .collect();
        let cut = files.cut.then_some(stamp);
        return paste_files(&ctx, pane_handle, sources, as_move || files.cut, cut).await;
    }
    go_to_pasted_path(ctx, pane_handle).await
}

/// Copy, or move, `sources` into the pane's folder, starting at once as a
/// drop does. `cut` is the clipboard's stamp when it holds a cut, which
/// pastes once: it is consumed when the move finishes, or at once when
/// there is nothing to move.
async fn paste_files(
    ctx: &MainWindowContext,
    pane_handle: PaneHandle,
    sources: Vec<VfsPath>,
    is_move: bool,
    cut: Option<Stamp>,
) -> Result<(), Error> {
    let destination = ctx.panes().require(pane_handle)?.path();
    let sources: Vec<VfsPath> = if is_move {
        // Moving a file into the folder it is in has nothing to do (a
        // copy says it can't, there being no second name to give it).
        sources
            .into_iter()
            .filter(|source| source.parent().as_ref() != Some(&destination))
            .collect()
    } else {
        sources
    };
    if sources.is_empty() {
        if let Some(stamp) = cut {
            consume_cut(ctx.clone(), stamp).await;
        }
        return Ok(());
    }
    let request = if is_move {
        OperationRequest::Move {
            rename_to: None,
            sources,
            destination,
            options: CopyMoveDefaults::stock(),
        }
    } else {
        OperationRequest::Copy {
            rename_to: None,
            sources,
            destination,
            options: CopyMoveDefaults::stock(),
        }
    };
    let after = ctx.clone();
    let on_completed = cut.map(|stamp| -> Box<dyn FnOnce() + Send> {
        Box::new(move || {
            tauri::async_runtime::spawn(consume_cut(after, stamp));
        })
    });
    super::operations::start_operation_with_callback(ctx.clone(), request, on_completed).await?;
    Ok(())
}

/// A pasted cut comes off the clipboard and out of the session's record —
/// unless something else has been put there since (`stamp`).
async fn consume_cut(ctx: MainWindowContext, stamp: Stamp) {
    let clipboard = ctx.clipboard();
    if clipboard.stamp().await == stamp
        && let Err(e) = clipboard.clear().await
    {
        log::warn!("clearing the clipboard after a cut: {e}");
    }
    ctx.revalidate_clipboard().await;
}

/// Text on the clipboard naming a place goes there, as typing it in Go to
/// would; anything else does nothing.
async fn go_to_pasted_path(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let text = ctx.clipboard().text().await?.unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }

    // Same resolution chain as the navigate command with exact: false
    let resolved = if let Some(vfs_path) = ctx.resolve_display_path(text) {
        Some(vfs_path)
    } else {
        ctx.shell_service()?
            .shell_expand(text.to_string())
            .await?
            .map(|p| VfsPath::new(VfsId::ROOT, p))
    };

    let text = text.to_string();
    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        if let Some(target) = resolved {
            pane.navigate_to(target).await?;
        } else {
            pane.navigate(&text).await?;
        }
        Ok(())
    })
    .await
}
