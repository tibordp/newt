use newt_common::path::{VfsId, VfsPath};
use newt_engine::operation::{CopyOptions, OperationRequest};
use newt_engine::vfs::{MountRequest, PathStyle};

use tauri::Manager;

use crate::associations::Action;
use crate::common::Error;
use crate::main_window::pane::{FilterMode, PARENT_KEY, Sorting};
use crate::main_window::{MainWindowContext, PaneHandle};
use crate::preferences::schema::BrowseFormat;

#[tauri::command]
#[specta::specta]
pub fn cancel(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.cancel();
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub async fn navigate(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    path: &str,
    exact: bool,
) -> Result<(), Error> {
    // Decode an *absolute* input into a fully-qualified VfsPath here, at
    // the boundary. Anything that stays `None` is a relative fragment
    // (`..`, `subdir`) resolved against the pane's current directory.
    // This is the single place native OS paths are turned into VfsPaths;
    // the VFS-domain code below the `Pane::navigate` call never sees a
    // drive letter / separator skew.
    let resolved = if let Some(vfs_path) = ctx.resolve_display_path(path) {
        // A VFS display path (s3://, archive, …, or a Windows-syntax
        // path claimed by a Windows-styled client-local mount).
        Some(vfs_path)
    } else if exact {
        // Verbatim: no shell expansion/fuzzing. Shift+<drive> hands us a
        // native display path (`C:\`); `..` and other relative fragments
        // fall through to relative resolution.
        // Host-native decode is only sound when the session root speaks
        // the host's path syntax (local and elevated sessions). On a
        // Windows host in a Unix remote session a stray `C:\` — Shift+
        // <drive> with no Windows-styled mount — must fall through
        // harmlessly, not decode against the agent's `/`.
        let root_is_host_style = ctx
            .vfs_info()
            .ok()
            .and_then(|vi| vi.descriptor(VfsId::ROOT))
            .is_some_and(|(_, meta)| PathStyle::from_mount_meta(&meta) == PathStyle::host());
        let native = std::path::Path::new(path);
        if root_is_host_style && native.is_absolute() {
            Some(VfsPath::new(
                VfsId::ROOT,
                newt_common::path::PathBuf::from_native(native),
            ))
        } else {
            None
        }
    } else {
        // Non-exact: allow shell expansion (~, env vars, …). The decode
        // to a VFS path happens inside `shell_expand`, on the side the
        // shell runs (the agent in a remote session); `None` means the
        // expansion wasn't absolute → resolve relative to the pane.
        ctx.shell_service()?
            .shell_expand(path.to_string())
            .await?
            .map(|p| VfsPath::new(VfsId::ROOT, p))
    };

    let path = path.to_string();
    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        if let Some(target) = resolved {
            pane.navigate_to(target).await?;
        } else {
            pane.navigate(&path).await?;
        }
        Ok(())
    })
    .await
}

/// Navigate to an already-resolved `VfsPath`, `vfs_id` included.
///
/// For callers holding a path rather than a string: the string-taking
/// `navigate` parses *display* paths and relative fragments, not VFS wire
/// paths (`/?/C:/Users/x`).
#[tauri::command]
#[specta::specta]
pub async fn navigate_to_path(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    path: VfsPath,
) -> Result<(), Error> {
    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_to(path).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub fn focus(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filename: Option<String>,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let state = gs.panes.require(pane_handle)?;
        if let Some(filename) = filename {
            state.view_state_mut().focus(filename);
        }
        gs.activate_pane(pane_handle);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
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
#[specta::specta]
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
#[specta::specta]
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
#[specta::specta]
pub fn cmd_select_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().select_all();
        Ok(())
    })
}

/// Toggle the focused entry's selection and move to the next one.
#[tauri::command]
#[specta::specta]
pub fn cmd_toggle_selection(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    toggle_selected(ctx, pane_handle, None, true)
}

#[tauri::command]
#[specta::specta]
pub fn cmd_invert_selection(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().invert_selection();
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn cmd_select_same_extension(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().select_same_extension();
        Ok(())
    })
}

/// Dialog submission: apply the pattern and close. An uncompilable
/// pattern leaves the dialog open (the frontend already shows it as
/// invalid), so nothing is done here.
#[tauri::command]
#[specta::specta]
pub fn select_by_pattern(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    pattern: String,
    subtract: bool,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |gs, pane| {
        if pane
            .view_state_mut()
            .select_matching(&pattern, subtract)
            .is_some()
        {
            *gs.select_pattern.lock() = pattern;
            gs.close_modal();
        }
        Ok(())
    })
}

/// Live match count for the dialog; `None` when the pattern doesn't compile.
#[tauri::command]
#[specta::specta]
pub fn count_pattern_matches(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    pattern: String,
) -> Result<Option<u32>, Error> {
    let pane = ctx.panes().require(pane_handle)?;
    let count = pane.view_state().count_matching(&pattern);
    Ok(count.map(|c| c as u32))
}

#[tauri::command]
#[specta::specta]
pub fn cmd_deselect_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().deselect_all();
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn end_drag_selection(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    pane.view_state_mut().end_drag_selection();
    Ok(())
}

#[tauri::command]
#[specta::specta]
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
#[specta::specta]
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
#[specta::specta]
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

/// Shift+Alt+wheel: move the cursor by `offset`, selecting from where the
/// session started. `end_drag_selection` ends the session.
#[tauri::command]
#[specta::specta]
pub fn wheel_select(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    offset: i32,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().wheel_select(offset);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn set_viewport(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    first_visible: usize,
    visible_count: usize,
) -> Result<(), Error> {
    let changed = {
        let pane = ctx.panes().require(pane_handle)?;
        pane.view_state_mut()
            .set_viewport_hint(first_visible, visible_count)
    };
    if changed {
        ctx.publish()?;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
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

/// Switch the pane to explicit-filter mode, keeping whatever is already
/// typed. That covers both ways in: from the file list there is no filter
/// yet so the box opens empty, and from quick-search the typed text
/// carries over into the filter. The frontend focuses the input off the
/// state change — see the effect keyed on `filter` in `Pane.tsx`.
#[tauri::command]
#[specta::specta]
pub fn cmd_start_filter(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        let mut view_state = pane.view_state_mut();
        let existing = view_state.filter.clone().unwrap_or_default();
        view_state.set_filter_with_mode(Some(existing), FilterMode::Filter);
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_as_other_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    ctx.with_update_async(|gs| async move { gs.as_other_pane(pane_handle).await })
        .await
}

/// Exchange the two panes rather than making them navigate to each other:
/// each side arrives with its selection, sorting, filter and navigation
/// history intact. The active handle moves with them, so the cursor stays
/// on the directory it was in and that directory changes sides.
#[tauri::command]
#[specta::specta]
pub fn cmd_swap_panes(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|gs| {
        gs.panes.swap();
        // Deliberately not `activate_pane` — that also claims focus for the
        // panes, which would yank it out of the terminal when the command
        // is run from the palette.
        let mut opts = gs.display_options.0.write();
        opts.active_pane = opts.active_pane.other();
        Ok(())
    })
}

pub async fn cmd_open_in_other_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    target: PaneHandle,
) -> Result<(), Error> {
    if pane_handle == target {
        return Ok(());
    }

    let pane = ctx.panes().require(pane_handle)?;
    let pane_path = pane.path();
    let file = match pane.get_focused_file_info() {
        Some(f) => f,
        None => return Ok(()),
    };

    let mut target_path = match file.name.as_str() {
        ".." => pane_path.parent().unwrap_or(pane_path),
        _ => match pane.get_focused_source() {
            Some(s) => s,
            None => return Ok(()),
        },
    };

    if !file.is_dir
        && let Action::Browse(format) = ctx.preferences().associations().action(
            &file.name,
            false,
            ctx.vfs_info()?.is_host_local(target_path.vfs_id),
            false,
        )
    {
        let request = browse_request(&ctx, &file.name, target_path, format).await?;
        target_path = VfsPath::root(ctx.mount_vfs(request).await?.vfs_id);
    }

    ctx.with_pane_update_async(target, |_gs, pane| async move {
        pane.navigate_to(target_path).await?;
        Ok(())
    })
    .await?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_open_in_left_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    cmd_open_in_other_pane(ctx, pane_handle, PaneHandle::left()).await
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_open_in_right_pane(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    cmd_open_in_other_pane(ctx, pane_handle, PaneHandle::right()).await
}

#[tauri::command]
#[specta::specta]
pub async fn enter(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    let Some(file) = pane.get_focused_file_info() else {
        return Ok(());
    };
    if file.name == ".." {
        drop(pane);
        return navigate(ctx, pane_handle, &file.name, true).await;
    }
    let Some(source) = pane.get_focused_source() else {
        return Ok(());
    };
    let package = pane.view_state().is_package(file.key());
    drop(pane);

    let host_local = ctx.vfs_info()?.is_host_local(source.vfs_id);
    let action =
        ctx.preferences()
            .associations()
            .action(&file.name, file.is_dir, host_local, package);
    match action {
        Action::Navigate => enter_directory(ctx, pane_handle).await,
        Action::Open => open_default(&ctx, source, &file.name, file.is_dir).await,
        Action::Browse(format) => browse(ctx, pane_handle, &file.name, source, format).await,
        Action::View => super::window::cmd_view(ctx, pane_handle).await,
        Action::Edit => super::window::cmd_edit(ctx, pane_handle).await,
        Action::Command(title) => run_command_titled(ctx, pane_handle, &title).await,
    }
}

/// Go into the focused directory.
async fn enter_directory(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    let Some(file) = pane.get_focused_file_info() else {
        return Ok(());
    };
    // On a Windows-shaped FS, arm a fallback for directory symlinks/
    // junctions: enter logically first (the pane keeps the link path,
    // as on Unix), and only when that listing fails land on the
    // resolved link *target* instead. Healthy links (`mklink /D`)
    // enter in place; the ACL-denied app-compat junctions
    // (`C:\Users\<user>\Cookies` — `Everyone:(DENY)(RD)`, unlistable
    // by design) resolve. Deliberately shallow: navigating to such a
    // path directly (Ctrl+L, a breadcrumb) still errors.
    let symlink_fallback = file.is_symlink() && {
        let source_vfs = pane
            .get_focused_source()
            .map(|p| p.vfs_id)
            .unwrap_or_else(|| pane.path().vfs_id);
        ctx.vfs_info()?
            .descriptor(source_vfs)
            .is_some_and(|(d, meta)| !d.has_unified_root(&meta))
    };

    // Directory entries from a synthetic VFS (e.g. a flat search hit
    // that happens to be a directory) should land on the *real*
    // directory in the underlying source VFS, not the in-search path.
    let Some(target) = pane.get_focused_source() else {
        return Ok(());
    };
    drop(pane);
    let link = target.clone();
    let logical = ctx
        .with_pane_update_async(pane_handle, |gs, pane| async move {
            gs.close_modal();
            pane.navigate_to(target).await?;
            Ok(())
        })
        .await;
    match logical {
        Err(e) if symlink_fallback && !matches!(e, Error::Cancelled) => {
            let Ok(resolved) = ctx.fs()?.resolve_link(link).await else {
                return Err(e);
            };
            ctx.with_pane_update_async(pane_handle, |_, pane| async move {
                pane.navigate_to(resolved).await?;
                Ok(())
            })
            .await
        }
        result => result,
    }
}

/// Mount `source` as a filesystem and go into it.
async fn browse(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    name: &str,
    source: VfsPath,
    format: Option<BrowseFormat>,
) -> Result<(), Error> {
    let request = browse_request(&ctx, name, source, format).await?;
    let root = VfsPath::root(ctx.mount_vfs(request).await?.vfs_id);
    ctx.with_pane_update_async(pane_handle, |_gs, pane| async move {
        pane.navigate_to(root).await?;
        Ok(())
    })
    .await
}

/// The mount request for browsing `origin` as `format`, or as whatever its
/// first bytes say when no association names a format.
async fn browse_request(
    ctx: &MainWindowContext,
    name: &str,
    origin: VfsPath,
    format: Option<BrowseFormat>,
) -> Result<MountRequest, Error> {
    if let Some(format) = format {
        return Ok(format.mount_request(origin));
    }
    let header = ctx
        .fs()?
        .read_range(origin.clone(), 0, newt_engine::vfs::SNIFF_LEN)
        .await?
        .data;
    newt_engine::vfs::sniff_mount_request(&header, origin)
        .ok_or_else(|| Error::Custom(format!("{name} is not an archive or a disc image")))
}

/// Hand `source` to the system's default application. A file elsewhere
/// than this computer is downloaded first; a directory can't be.
async fn open_default(
    ctx: &MainWindowContext,
    source: VfsPath,
    name: &str,
    is_dir: bool,
) -> Result<(), Error> {
    if is_dir && !ctx.vfs_info()?.is_host_local(source.vfs_id) {
        return Err(Error::Custom(format!(
            "{name} is not on this computer, so it can't be opened in an application"
        )));
    }
    with_local_copy(ctx, source, name, |path| {
        opener::open(path).map_err(|e| e.to_string())
    })
    .await
}

/// Run the `[[command]]` titled `title`, as picking it from the palette
/// would.
async fn run_command_titled(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    title: &str,
) -> Result<(), Error> {
    let app = ctx.window().app_handle().clone();
    let global: tauri::State<crate::GlobalContext> = app.state();
    let index = global
        .preferences()
        .resolved()
        .user_commands
        .iter()
        .position(|command| command.title == title)
        .ok_or_else(|| Error::Custom(format!("There is no user command titled \"{title}\"")))?;
    crate::user_commands::run_user_command(ctx, global, pane_handle, index).await
}

/// Run `then` on `source` as a file on this computer: the file itself when
/// it is one, else — once the download finishes — a copy downloaded with
/// the standard Copy operation into a temporary directory. Only the first
/// case can report an error; a failure after a download is logged.
async fn with_local_copy(
    ctx: &MainWindowContext,
    source: VfsPath,
    filename: &str,
    then: impl FnOnce(std::path::PathBuf) -> Result<(), String> + Send + 'static,
) -> Result<(), Error> {
    let vfs_info = ctx.vfs_info()?;
    if vfs_info.is_host_local(source.vfs_id) {
        let path = source.path.launch_cwd();
        return tokio::task::spawn_blocking(move || then(path))
            .await
            .map_err(|e| Error::Custom(e.to_string()))?
            .map_err(Error::Custom);
    }
    let host_vfs = vfs_info.host_local_vfs_id().ok_or_else(|| {
        Error::Custom("No local filesystem mounted — cannot open files externally".to_string())
    })?;

    let temp_dir = tempfile::tempdir_in(std::env::temp_dir())?.keep();
    let dest_path = temp_dir.join(filename);
    let dest_vfs_path = VfsPath::new(host_vfs, newt_common::path::PathBuf::from_native(&temp_dir));

    super::operations::start_operation_with_callback(
        ctx.clone(),
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![source],
            destination: dest_vfs_path,
            options: CopyOptions::default(),
        },
        Some(Box::new(move || {
            if let Err(e) = then(dest_path) {
                log::warn!("opening a downloaded file: {e}");
            }
        })),
    )
    .await?;

    Ok(())
}

/// The focused file (not a directory) and where it really is.
fn focused_file(ctx: &MainWindowContext, pane_handle: PaneHandle) -> Option<(String, VfsPath)> {
    let pane = ctx.panes().require(pane_handle).ok()?;
    let file = pane
        .get_focused_file_info()
        .filter(|f| f.name != ".." && !f.is_dir)?;
    Some((file.name, pane.get_focused_source()?))
}

/// The applications the system offers for the focused file.
#[tauri::command]
#[specta::specta]
pub async fn open_with_apps(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<Vec<crate::open_with::OpenWithApp>, Error> {
    let Some((name, _)) = focused_file(&ctx, pane_handle) else {
        return Ok(Vec::new());
    };
    tokio::task::spawn_blocking(move || crate::open_with::apps_for(&name))
        .await
        .map_err(|e| Error::Custom(e.to_string()))
}

/// Open the focused file in `app`, an `OpenWithApp::id`.
#[tauri::command]
#[specta::specta]
pub async fn open_with_app(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    app: String,
) -> Result<(), Error> {
    let Some((name, source)) = focused_file(&ctx, pane_handle) else {
        return Ok(());
    };
    with_local_copy(&ctx, source, &name, move |path| {
        crate::open_with::open_with(&path, &app)
    })
    .await
}

/// The system's chooser for the focused file.
#[tauri::command]
#[specta::specta]
pub async fn cmd_open_with(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let Some((name, source)) = focused_file(&ctx, pane_handle) else {
        return Ok(());
    };
    let window = ctx.window();
    with_local_copy(&ctx, source, &name, move |path| {
        crate::open_with::choose(&window, path)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_open(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    let Some(file) = pane.get_focused_file_info().filter(|f| f.name != "..") else {
        return Ok(());
    };
    let Some(source) = pane.get_focused_source() else {
        return Ok(());
    };
    drop(pane);
    open_default(&ctx, source, &file.name, file.is_dir).await
}

/// Into the focused entry whatever Enter does with it: a directory or
/// package is entered, a file browsed as its association's format, or as
/// whatever its first bytes say.
#[tauri::command]
#[specta::specta]
pub async fn cmd_browse_into(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    let Some(file) = pane.get_focused_file_info().filter(|f| f.name != "..") else {
        return Ok(());
    };
    // Mount on the *real* file, not the in-SearchVfs alias.
    let Some(source) = pane.get_focused_source() else {
        return Ok(());
    };
    drop(pane);
    if file.is_dir {
        return enter_directory(ctx, pane_handle).await;
    }
    let format = ctx.preferences().associations().format(&file.name);
    browse(ctx, pane_handle, &file.name, source, format).await
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_follow_symlink(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;

    // "Follow" prefers an alias source (synthetic VFS entries — search
    // results, …) over the entry's own symlink target, since the alias
    // *is* the destination the user is asking us to reveal. Symlink
    // following falls through when there's no alias.
    let resolved: VfsPath = if let Some(source) = pane.get_focused_source()
        && pane
            .get_focused_file()
            .is_some_and(|focused| focused != source)
    {
        source
    } else {
        let Some(link) = pane
            .get_focused_file_info()
            .filter(|f| f.is_symlink())
            .and_then(|_| pane.get_focused_source())
        else {
            return Ok(());
        };
        ctx.fs()?.resolve_link(link).await?
    };

    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        pane.reveal(resolved, None).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_open_folder(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    let vfs_info = ctx.vfs_info()?;
    let focused = pane
        .get_focused_file_info()
        .filter(|f| f.key() != PARENT_KEY)
        .and_then(|_| pane.get_focused_source());
    let (target, select) = match focused {
        Some(source) => (source, true),
        None => (pane.path(), false),
    };
    if !vfs_info.is_host_local(target.vfs_id) {
        return Ok(());
    }

    // Both block on another process: the file manager's D-Bus reply (which
    // can mean starting it), the shell's COM call, or `open` exiting.
    // The shell takes no verbatim (`\\?\`) paths.
    let native = target.path.launch_cwd();
    tokio::task::spawn_blocking(move || {
        if select {
            opener::reveal(native)
        } else {
            opener::open(native)
        }
    })
    .await
    .map_err(|e| Error::Custom(e.to_string()))??;

    Ok(())
}

/// Jump to the root of the filesystem the pane is on — `/`, or the current
/// drive or share root where the filesystem has several (Windows).
#[tauri::command]
#[specta::specta]
pub async fn cmd_navigate_root(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let vfs_info = ctx.vfs_info()?;
    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        let current = pane.path();
        let Some((descriptor, meta)) = vfs_info.descriptor(current.vfs_id) else {
            return Ok(());
        };
        let root = descriptor.root_of(&current.path, &meta);
        if root == current.path {
            return Ok(());
        }
        pane.navigate_to(VfsPath::new(current.vfs_id, root)).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
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
#[specta::specta]
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
#[specta::specta]
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

/// Remove an entry from a pane's navigation history. Used by the persistent
/// history dialog (the alt-tab overlay shows entries read-only). The
/// modal is *not* closed — the dialog state is refreshed in place by
/// rebuilding the entries list, so the user can keep deleting.
#[tauri::command]
#[specta::specta]
pub fn delete_history_entry(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    target_index: usize,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let pane = gs.panes.require(pane_handle)?;
        pane.delete_history_entry(target_index);

        // Rebuild the modal data so the dialog re-renders without the
        // deleted entry. Preserve persistent flag and initial_direction.
        let mut modal = gs.modal.0.write();
        if let Some(ref mut data) = *modal
            && let crate::main_window::ModalDataKind::HistoryNavigator {
                entries,
                current_index,
                ..
            } = &mut data.kind
        {
            let (new_entries, new_current_index) = pane.history_entries();
            *entries = new_entries;
            *current_index = new_current_index;
        }
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn cmd_toggle_hidden(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
    let show = !global_ctx.runtime_state().state().show_hidden;
    global_ctx
        .runtime_state()
        .update_key("show_hidden", show.into())
        .map_err(Error::Custom)?;
    for window in global_ctx.main_windows_by_focus() {
        window.with_update(|gs| {
            gs.set_show_hidden(show);
            Ok(())
        })?;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn cmd_toggle_maximized(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let mut opts = c.display_options.0.write();
        opts.maximized = !opts.maximized;
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn cmd_compute_size(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    let keys = pane.effective_selection_keys();
    if !keys.is_empty() {
        tauri::async_runtime::spawn(pane.run_manual_enrichment(
            vec!["du".to_string()],
            newt_engine::enrich::EnrichScope::Entries(keys),
        ));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn cmd_compute_all_sizes(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    tauri::async_runtime::spawn(pane.run_manual_enrichment(
        vec!["du".to_string()],
        newt_engine::enrich::EnrichScope::AllEntries,
    ));
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn cmd_refresh(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().require(pane_handle)?;
    pane.refresh(None, true).await?;
    ctx.publish()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shell context menu (Windows). Doc comments on the #[cfg(windows)] and
// stub definitions must stay identical: tauri-specta emits them as JSDoc,
// so a mismatch makes `bindings.ts` depend on the build host.
// ---------------------------------------------------------------------------

/// Show the native Windows shell context menu (classic `IContextMenu`)
/// for the pane's selection — or for the directory itself when
/// `on_background`. `x`/`y` are CSS-pixel client coordinates.
#[cfg(windows)]
#[tauri::command]
#[specta::specta]
pub async fn shell_context_menu(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    x: f64,
    y: f64,
    on_background: bool,
) -> Result<(), Error> {
    crate::main_window::shell_menu::show_shell_context_menu(&ctx, pane_handle, x, y, on_background)
        .await
}

/// Show the native Windows shell context menu (classic `IContextMenu`)
/// for the pane's selection — or for the directory itself when
/// `on_background`. `x`/`y` are CSS-pixel client coordinates.
#[cfg(not(windows))]
#[tauri::command]
#[specta::specta]
pub async fn shell_context_menu(
    _pane_handle: PaneHandle,
    _x: f64,
    _y: f64,
    _on_background: bool,
) -> Result<(), Error> {
    Err(Error::Custom(
        "The shell context menu is only available on Windows".into(),
    ))
}
