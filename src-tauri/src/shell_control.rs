//! Host side of shell integration: the verb handlers behind the `newt` CLI.
//!
//! Local sessions serve the control socket directly from this process; in
//! remote sessions the agent owns the socket and forwards control-plane
//! verbs here over `API_HOST_SHELL_CONTROL`. Both entry points funnel into
//! [`handle_control`].

use std::sync::Arc;

use newt_common::path::{VfsId, VfsPath};
use newt_engine::filesystem::Filesystem;
use newt_engine::filesystem::{ByteStream, file_reader_stream};
use newt_engine::operation::{CopyOptions, OperationId, OperationRequest};
use newt_engine::shell_control::{
    CommandListEntry, ControlRequest, ControlResponse, ControlResult, EntryKind, ListEntry,
    PaneSelector, SelectMode, ShellControlHandler,
};
use newt_rpc::Dispatcher;

use crate::GlobalContext;
use crate::main_window::{MainWindowContext, Outcome, PaneHandle};

/// Look up the owning window's `MainWindowContext` on every request — the
/// session (and this handler) is constructed before the context is
/// registered, and the registry is the one authoritative source.
fn context_for(window: &tauri::WebviewWindow) -> Result<MainWindowContext, String> {
    use tauri::Manager;
    let global: tauri::State<GlobalContext> = window.app_handle().state();
    global
        .main_window_by_label(window.label())
        .ok_or_else(|| "session not ready".to_string())
}

/// Handler for local sessions: the control socket lives in this process.
pub struct HostShellHandler {
    pub window: tauri::WebviewWindow,
    pub fs: Arc<dyn Filesystem>,
}

#[async_trait::async_trait]
impl ShellControlHandler for HostShellHandler {
    async fn control(&self, req: ControlRequest) -> ControlResult {
        let ctx = context_for(&self.window)?;
        handle_control(&ctx, req).await
    }

    async fn read_file(&self, path: VfsPath) -> Result<ByteStream, String> {
        Ok(file_reader_stream(self.fs.clone(), path))
    }
}

/// RPC dispatcher for remote sessions: serves `API_HOST_SHELL_CONTROL`
/// invoked by the agent's control server.
pub struct ShellControlDispatcher {
    pub window: tauri::WebviewWindow,
    /// Whether the session may reach this host. `newt cmd` runs any
    /// command, the clipboard and host apps among them, so it needs this;
    /// the other verbs act on the session's own panes.
    pub host_access: bool,
}

#[async_trait::async_trait]
impl Dispatcher for ShellControlDispatcher {
    async fn invoke(&self, api: newt_rpc::Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        if api != newt_engine::api::API_HOST_SHELL_CONTROL {
            return None;
        }
        let request: ControlRequest = newt_engine::api::decode(&req);
        let result: ControlResult =
            if !self.host_access && matches!(request, ControlRequest::Command { .. }) {
                Err(
                    "newt cmd needs this session to have been allowed access to the \
                     host running Newt (\"Allow access to this host\" when connecting)"
                        .to_string(),
                )
            } else {
                match context_for(&self.window) {
                    Ok(ctx) => handle_control(&ctx, request).await,
                    Err(e) => Err(e),
                }
            };
        Some(bytes::Bytes::from(newt_engine::api::encode(&result)))
    }
}

fn select_pane(ctx: &MainWindowContext, selector: PaneSelector) -> PaneHandle {
    let active = ctx.active_pane_handle();
    match selector {
        PaneSelector::Active => active,
        PaneSelector::Other => {
            if active == PaneHandle::left() {
                PaneHandle::right()
            } else {
                PaneHandle::left()
            }
        }
        PaneSelector::Left => PaneHandle::left(),
        PaneSelector::Right => PaneHandle::right(),
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Resolve a CLI path argument the way the Go To dialog does (URLs of
/// mounted VFSes, native absolutes, `~`). Relative fragments resolve
/// against `base` — the CLI's cwd for shell-like verbs, the pane's path
/// for pane-relative ones — using the same descriptor-aware
/// `Pane::resolve_relative` that pane-relative navigation uses, so `..`
/// and friends behave identically everywhere. Native strings cross into
/// the VFS domain only at the `shell_expand` boundary (on the session
/// side that owns the filesystem).
async fn resolve_arg(
    ctx: &MainWindowContext,
    pane: PaneHandle,
    path: &str,
    base: ArgBase<'_>,
) -> Result<VfsPath, String> {
    if let Some(vfs_path) = ctx.resolve_display_path(path) {
        return Ok(vfs_path);
    }
    let shell = ctx.shell_service().map_err(err)?;
    if let Some(expanded) = shell.shell_expand(path.to_string()).await.map_err(err)? {
        return Ok(VfsPath::new(VfsId::ROOT, expanded));
    }
    let pane = ctx
        .panes()
        .get(pane)
        .ok_or_else(|| "no such pane".to_string())?;
    let base = match base {
        ArgBase::Cwd(cwd) => match shell.shell_expand(cwd.to_string()).await.map_err(err)? {
            Some(expanded) => VfsPath::new(VfsId::ROOT, expanded),
            None => return Err(format!("cannot resolve relative path: {path}")),
        },
        ArgBase::Pane => pane.path(),
    };
    Ok(pane.resolve_relative(&base, path))
}

/// What a relative CLI argument resolves against.
#[derive(Clone, Copy)]
enum ArgBase<'a> {
    /// The CLI's own cwd (shell-like verbs: cd/focus/cp/mv).
    Cwd(&'a str),
    /// The pane's current path (`cat`/`open`/`edit` read what you see).
    Pane,
}

pub async fn handle_control(ctx: &MainWindowContext, req: ControlRequest) -> ControlResult {
    match req {
        ControlRequest::Pwd { pane } => {
            let handle = select_pane(ctx, pane);
            let pane = ctx
                .panes()
                .get(handle)
                .ok_or_else(|| "no such pane".to_string())?;
            let display = ctx.format_vfs_path(&pane.path());
            Ok(ControlResponse::Text(display))
        }
        ControlRequest::List {
            pane,
            selection,
            all,
        } => {
            use crate::main_window::pane::EntrySet;
            let handle = select_pane(ctx, pane);
            let pane = ctx
                .panes()
                .get(handle)
                .ok_or_else(|| "no such pane".to_string())?;
            let set = if all {
                EntrySet::All
            } else if selection {
                EntrySet::Selection
            } else {
                EntrySet::Listed
            };
            let entries = pane
                .listed_entries(set)
                .into_iter()
                .map(|(path, file)| list_entry(ctx, &path, file))
                .collect();
            Ok(ControlResponse::Entries(entries))
        }
        ControlRequest::Navigate { pane, path, cwd } => {
            let handle = select_pane(ctx, pane);
            let target = resolve_arg(ctx, handle, &path, ArgBase::Cwd(&cwd)).await?;
            // Mirror cmd::pane::navigate: the wrapper's publish delivers the
            // final navigation state (navigate_impl only publishes interim
            // streamed batches — a batchless listing like S3 would
            // otherwise leave the pane on the spinner until the next
            // unrelated publish).
            ctx.with_pane_update_async(handle, |gs, pane| async move {
                gs.close_modal();
                pane.navigate_to(target).await
            })
            .await
            .map_err(err)?;
            Ok(ControlResponse::Ok)
        }
        ControlRequest::Select {
            pane,
            patterns,
            names,
            cwd,
            mode,
        } => {
            let handle = select_pane(ctx, pane);
            let mut keys = if patterns.is_empty() {
                pane_entry_keys(ctx, handle, &names, &cwd).await?
            } else {
                let pane = ctx
                    .panes()
                    .get(handle)
                    .ok_or_else(|| "no such pane".to_string())?;
                let vs = pane.view_state();
                let mut keys = Vec::new();
                for p in &patterns {
                    keys.extend(
                        vs.matching_keys(p)
                            .ok_or_else(|| format!("invalid pattern: {p}"))?,
                    );
                }
                keys
            };
            keys.sort();
            keys.dedup();
            let hits = ctx
                .with_pane_update(handle, |_, pane| {
                    let mut vs = pane.view_state_mut();
                    if mode == SelectMode::Replace {
                        vs.deselect_all();
                    }
                    Ok(vs.select_keys(&keys, mode == SelectMode::Remove))
                })
                .map_err(err)?;
            Ok(ControlResponse::Text(format!("{hits} matched")))
        }
        ControlRequest::Command { pane, id } => {
            let handle = select_pane(ctx, pane);
            let id = command_id(ctx, &id)?;
            if crate::cmd::invoke_palette_command(ctx, &id, handle)
                .await
                .map_err(err)?
            {
                Ok(ControlResponse::Ok)
            } else {
                Err(format!("unknown command: {id}"))
            }
        }
        ControlRequest::ListCommands => {
            use tauri::Manager;
            let app_handle = ctx.window().app_handle().clone();
            let global: tauri::State<GlobalContext> = app_handle.state();
            let commands = global
                .preferences()
                .resolved()
                .commands
                .into_iter()
                .filter(|c| c.scope == crate::preferences::commands::CommandScope::Main)
                .map(|c| CommandListEntry {
                    id: c.id,
                    name: c.name,
                })
                .collect();
            Ok(ControlResponse::Commands(commands))
        }
        ControlRequest::ResolveFile { pane, path, cwd } => {
            let _ = cwd;
            let handle = select_pane(ctx, pane);
            let resolved = resolve_arg(ctx, handle, &path, ArgBase::Pane).await?;
            Ok(ControlResponse::ResolvedFile(resolved))
        }
        ControlRequest::Open {
            pane,
            path,
            cwd,
            edit,
            wait,
        } => {
            let _ = cwd;
            let handle = select_pane(ctx, pane);
            let resolved = resolve_arg(ctx, handle, &path, ArgBase::Pane).await?;
            let details = ctx
                .fs()
                .map_err(err)?
                .file_info(resolved.clone(), true)
                .await;
            if details.is_ok_and(|d| {
                d.mode
                    .as_ref()
                    .is_some_and(newt_engine::vfs::Mode::is_special)
            }) {
                return Err(err(crate::cmd::window::not_regular(ctx, &resolved)));
            }
            let window = if edit {
                crate::cmd::window::open_editor_window(ctx, &resolved).map_err(err)?
            } else {
                crate::cmd::window::open_viewer_window(ctx, &resolved).map_err(err)?
            };
            if wait {
                closed(&window).await;
            }
            Ok(ControlResponse::Ok)
        }
        ControlRequest::Transfer {
            pane,
            move_files,
            sources,
            dest,
            cwd,
            wait,
        } => {
            let handle = select_pane(ctx, pane);
            let request = build_transfer(ctx, handle, move_files, sources, &dest, &cwd).await?;
            run_operation(ctx, request, wait).await
        }
        ControlRequest::Delete {
            pane,
            paths,
            cwd,
            recursive,
            permanent,
            wait,
        } => {
            let handle = select_pane(ctx, pane);
            let request = build_delete(ctx, handle, &paths, &cwd, recursive, permanent).await?;
            run_operation(ctx, request, wait).await
        }
        ControlRequest::MakeDirectory {
            pane,
            paths,
            cwd,
            parents,
        } => {
            let handle = select_pane(ctx, pane);
            let fs = ctx.fs().map_err(err)?;
            let mut created = Vec::new();
            for path in &paths {
                let dir = resolve_arg(ctx, handle, path, ArgBase::Cwd(&cwd)).await?;
                let result = if parents {
                    create_with_parents(fs.as_ref(), &dir, &mut created).await
                } else {
                    fs.create_directory(dir.clone())
                        .await
                        .map(|()| created.push(dir.clone()))
                };
                if let Err(e) = result {
                    ctx.refresh_panes_at(&parent_dirs(&created)).await;
                    return Err(format!("cannot create {}: {e}", ctx.format_vfs_path(&dir)));
                }
            }
            ctx.refresh_panes_at(&parent_dirs(&created)).await;
            Ok(ControlResponse::Ok)
        }
    }
}

fn list_entry(ctx: &MainWindowContext, path: &VfsPath, file: newt_engine::vfs::File) -> ListEntry {
    let kind = if file.symlink.is_some() {
        EntryKind::Symlink
    } else if file.is_dir {
        EntryKind::Dir
    } else if file
        .mode
        .as_ref()
        .is_some_and(newt_engine::vfs::Mode::is_special)
    {
        EntryKind::Special
    } else {
        EntryKind::File
    };
    let owner = |owner: Option<newt_engine::vfs::UserGroup>| {
        owner.map(|owner| match owner {
            newt_engine::vfs::UserGroup::Name(name) => name,
            newt_engine::vfs::UserGroup::Id(id) => id.to_string(),
        })
    };
    ListEntry {
        path: ctx.format_vfs_path(path),
        name: file.name,
        kind,
        size: file.size,
        modified: file.modified.map(|ms| ms.div_euclid(1000)),
        mode: file.mode.map(|mode| mode.0),
        user: owner(file.user),
        group: owner(file.group),
        target: file
            .symlink
            .and_then(|link| link.target().map(str::to_string)),
    }
}

/// The registry id `newt cmd` was given, or the id of the one user command
/// titled that way (ignoring case).
fn command_id(ctx: &MainWindowContext, name: &str) -> Result<String, String> {
    use tauri::Manager;
    let app_handle = ctx.window().app_handle().clone();
    let global: tauri::State<GlobalContext> = app_handle.state();
    let commands = global.preferences().resolved().commands;
    if commands.iter().any(|c| c.id == name) {
        return Ok(name.to_string());
    }
    let wanted = name.to_lowercase();
    let mut titled = commands
        .into_iter()
        .filter(|c| c.id.starts_with("user_command_") && c.name.to_lowercase() == wanted);
    match (titled.next(), titled.next()) {
        (Some(command), None) => Ok(command.id),
        (Some(_), Some(_)) => Err(format!("several user commands are titled {name:?}")),
        (None, _) => Err(format!("unknown command: {name}")),
    }
}

/// Resolves once `window` has been destroyed.
async fn closed(window: &tauri::WebviewWindow) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = parking_lot::Mutex::new(Some(tx));
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed)
            && let Some(tx) = tx.lock().take()
        {
            let _ = tx.send(());
        }
    });
    // An error means the window, and its handler with it, is already gone.
    let _ = rx.await;
}

/// Start `request`. Without `wait`, answer with its id at once; with it,
/// answer once it has ended, cancelling it if this future is dropped first
/// — which is what a `newt` interrupted mid-wait does to it.
async fn run_operation(
    ctx: &MainWindowContext,
    request: OperationRequest,
    wait: bool,
) -> ControlResult {
    if !wait {
        let id = crate::cmd::operations::start_operation(ctx.clone(), request)
            .await
            .map_err(err)?;
        return Ok(ControlResponse::Text(format!("operation {id}")));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let id = crate::cmd::operations::start_operation_with_end_hook(
        ctx.clone(),
        request,
        Some(Box::new(move |outcome| {
            let _ = tx.send(outcome);
        })),
    )
    .await
    .map_err(err)?;
    let mut cancel = CancelOnDrop(Some((ctx.clone(), id)));
    let outcome = rx.await;
    cancel.0 = None;
    match outcome {
        Ok(Outcome::Completed) => Ok(ControlResponse::Ok),
        Ok(Outcome::Failed) => Err(ctx
            .operations()
            .state
            .read()
            .get(&id)
            .and_then(|op| op.error.clone())
            .unwrap_or_else(|| "operation failed".to_string())),
        Ok(Outcome::Cancelled) => Err("operation cancelled".to_string()),
        Err(_) => Err("the session ended".to_string()),
    }
}

/// Cancels the operation it holds when dropped.
struct CancelOnDrop(Option<(MainWindowContext, OperationId)>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some((ctx, id)) = self.0.take() {
            tauri::async_runtime::spawn(async move {
                if let Ok(client) = ctx.operations_client() {
                    let _ = client.cancel_operation(id).await;
                }
            });
        }
    }
}

/// `rm` semantics: directories need `recursive`; items go to the trash
/// when `behavior.delete_to_trash` says so, and a target with no trash is
/// refused rather than deleted unless `permanent`.
async fn build_delete(
    ctx: &MainWindowContext,
    pane: PaneHandle,
    paths: &[String],
    cwd: &str,
    recursive: bool,
    permanent: bool,
) -> Result<OperationRequest, String> {
    use tauri::Manager;
    let fs = ctx.fs().map_err(err)?;
    let mut targets = Vec::with_capacity(paths.len());
    for path in paths {
        let target = resolve_arg(ctx, pane, path, ArgBase::Cwd(cwd)).await?;
        let details = fs
            .file_info(target.clone(), false)
            .await
            .map_err(|e| format!("{path}: {e}"))?;
        if details.is_dir && !recursive {
            return Err(format!("{path} is a directory (use --recursive)"));
        }
        targets.push(target);
    }
    let app_handle = ctx.window().app_handle().clone();
    let global: tauri::State<GlobalContext> = app_handle.state();
    let to_trash = !permanent && global.preferences().settings().behavior.delete_to_trash;
    if to_trash {
        let vfs_info = ctx.vfs_info().map_err(err)?;
        if let Some(target) = targets.iter().find(|t| {
            !vfs_info
                .descriptor(t.vfs_id)
                .is_some_and(|(d, _)| d.can_trash())
        }) {
            return Err(format!(
                "{} has no trash here (use --permanent to delete it)",
                ctx.format_vfs_path(target)
            ));
        }
    }
    Ok(OperationRequest::Delete {
        paths: targets,
        to_trash,
        cross_mount_points: false,
    })
}

/// Create `dir` and whichever of its ancestors are missing, recording each
/// directory created. An existing directory is no error.
async fn create_with_parents(
    fs: &dyn Filesystem,
    dir: &VfsPath,
    created: &mut Vec<VfsPath>,
) -> Result<(), newt_common::Error> {
    let mut missing = Vec::new();
    let mut cursor = Some(dir.clone());
    while let Some(path) = cursor {
        match fs.file_info(path.clone(), true).await {
            Ok(details) if details.is_dir => break,
            Ok(_) => return Err(newt_common::Error::custom("not a directory")),
            Err(_) => {
                cursor = path.parent();
                missing.push(path);
            }
        }
    }
    for path in missing.into_iter().rev() {
        fs.create_directory(path.clone()).await?;
        created.push(path);
    }
    Ok(())
}

fn parent_dirs(paths: &[VfsPath]) -> Vec<VfsPath> {
    let mut dirs: Vec<VfsPath> = paths.iter().filter_map(VfsPath::parent).collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Entry keys for `newt select` names. A bare name (no separator) is a
/// pane entry as listed — the only form that makes sense on S3 or inside
/// an archive, and what `ls | newt select` produces. Anything else is a
/// path relative to the shell's cwd and only counts if it resolves into
/// the pane's own directory (`git ls-files` at the pane's path yields
/// bare names for the top level and skips the rest).
async fn pane_entry_keys(
    ctx: &MainWindowContext,
    handle: PaneHandle,
    names: &[String],
    cwd: &str,
) -> Result<Vec<String>, String> {
    let pane_path = ctx
        .panes()
        .get(handle)
        .ok_or_else(|| "no such pane".to_string())?
        .path();
    let mut keys = Vec::with_capacity(names.len());
    for name in names {
        if !name.contains(['/', '\\']) {
            keys.push(name.clone());
            continue;
        }
        let resolved = resolve_arg(ctx, handle, name, ArgBase::Cwd(cwd)).await?;
        if resolved.parent() == Some(pane_path.clone())
            && let Some(leaf) = resolved.file_name()
        {
            keys.push(leaf.to_string());
        }
    }
    Ok(keys)
}

/// `cp`/`mv` semantics: multiple sources require an existing directory
/// destination; a single source may target a non-existent leaf (copy/move
/// under a new name; a same-directory move is a plain rename). A trailing
/// slash asserts directory-ness. Existence is checked through the session
/// VFS, not the local filesystem.
async fn build_transfer(
    ctx: &MainWindowContext,
    pane: PaneHandle,
    move_files: bool,
    sources: Vec<String>,
    dest: &str,
    cwd: &str,
) -> Result<OperationRequest, String> {
    if sources.is_empty() {
        return Err("no sources".into());
    }
    let mut resolved_sources = Vec::with_capacity(sources.len());
    for s in &sources {
        resolved_sources.push(resolve_arg(ctx, pane, s, ArgBase::Cwd(cwd)).await?);
    }
    let wants_dir = dest.ends_with('/') || dest.ends_with('\\');
    let dest_path = resolve_arg(ctx, pane, dest, ArgBase::Cwd(cwd)).await?;

    let fs = ctx.fs().map_err(err)?;
    let dest_details = fs.file_info(dest_path.clone(), true).await.ok();

    match dest_details {
        Some(details) if details.is_dir => {
            Ok(copy_or_move(move_files, resolved_sources, dest_path, None))
        }
        Some(_) => Err(format!("destination exists and is not a directory: {dest}")),
        None if wants_dir => Err(format!("destination is not a directory: {dest}")),
        None => {
            // Single source → non-existent leaf: copy/move under a new name.
            if resolved_sources.len() > 1 {
                return Err(format!("destination is not an existing directory: {dest}"));
            }
            let source = resolved_sources.into_iter().next().unwrap();
            let parent = dest_path
                .parent()
                .ok_or_else(|| format!("destination has no parent: {dest}"))?;
            let new_name = dest_path
                .file_name()
                .ok_or_else(|| format!("destination has no file name: {dest}"))?
                .to_string();
            let parent_details = fs
                .file_info(parent.clone(), true)
                .await
                .map_err(|_| format!("destination directory does not exist: {dest}"))?;
            if !parent_details.is_dir {
                return Err(format!("destination directory does not exist: {dest}"));
            }
            if move_files && source.parent() == Some(parent.clone()) {
                // A move to a new leaf name in the same directory is a
                // plain rename.
                Ok(OperationRequest::Rename { source, new_name })
            } else {
                Ok(copy_or_move(
                    move_files,
                    vec![source],
                    parent,
                    Some(new_name),
                ))
            }
        }
    }
}

fn copy_or_move(
    move_files: bool,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    rename_to: Option<String>,
) -> OperationRequest {
    if move_files {
        OperationRequest::Move {
            sources,
            destination,
            options: CopyOptions::default(),
            rename_to,
        }
    } else {
        OperationRequest::Copy {
            sources,
            destination,
            options: CopyOptions::default(),
            rename_to,
        }
    }
}
