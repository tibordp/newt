//! Host side of shell integration: the verb handlers behind the `newt` CLI.
//!
//! Local sessions serve the control socket directly from this process; in
//! remote sessions the agent owns the socket and forwards control-plane
//! verbs here over `API_HOST_SHELL_CONTROL`. Both entry points funnel into
//! [`handle_control`].

use std::sync::Arc;

use newt_common::file_reader::FileReader;
use newt_common::operation::{CopyOptions, OperationRequest};
use newt_common::rpc::Dispatcher;
use newt_common::shell_control::{
    ByteStream, CommandListEntry, ControlRequest, ControlResponse, ControlResult, PaneSelector,
    ShellControlHandler, file_reader_stream,
};
use newt_common::vfs::{VfsId, VfsPath};

use crate::GlobalContext;
use crate::main_window::{MainWindowContext, PaneHandle};

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
    pub file_reader: Arc<dyn FileReader>,
}

#[async_trait::async_trait]
impl ShellControlHandler for HostShellHandler {
    async fn control(&self, req: ControlRequest) -> ControlResult {
        let ctx = context_for(&self.window)?;
        handle_control(&ctx, req).await
    }

    async fn read_file(&self, path: VfsPath) -> Result<ByteStream, String> {
        Ok(file_reader_stream(self.file_reader.clone(), path))
    }
}

/// RPC dispatcher for remote sessions: serves `API_HOST_SHELL_CONTROL`
/// invoked by the agent's control server.
pub struct ShellControlDispatcher {
    pub window: tauri::WebviewWindow,
}

#[async_trait::async_trait]
impl Dispatcher for ShellControlDispatcher {
    async fn invoke(
        &self,
        api: newt_common::rpc::Api,
        req: bytes::Bytes,
    ) -> Result<Option<bytes::Bytes>, newt_common::Error> {
        if api != newt_common::api::API_HOST_SHELL_CONTROL {
            return Ok(None);
        }
        let request: ControlRequest = newt_common::api::decode(&req)?;
        let result: ControlResult = match context_for(&self.window) {
            Ok(ctx) => handle_control(&ctx, request).await,
            Err(e) => Err(e),
        };
        Ok(Some(bytes::Bytes::from(newt_common::api::encode(&result)?)))
    }

    async fn notify(
        &self,
        _api: newt_common::rpc::Api,
        _req: bytes::Bytes,
    ) -> Result<bool, newt_common::Error> {
        Ok(false)
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
        ControlRequest::Command { pane, id } => {
            let handle = select_pane(ctx, pane);
            if crate::cmd::dispatch_registry_command(ctx, &id, handle)
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
        } => {
            let _ = cwd;
            let handle = select_pane(ctx, pane);
            let resolved = resolve_arg(ctx, handle, &path, ArgBase::Pane).await?;
            if edit {
                crate::cmd::window::open_editor_window(ctx, &resolved).map_err(err)?;
            } else {
                crate::cmd::window::open_viewer_window(ctx, &resolved).map_err(err)?;
            }
            Ok(ControlResponse::Ok)
        }
        ControlRequest::Transfer {
            move_files,
            sources,
            dest,
            cwd,
        } => {
            let handle = select_pane(ctx, PaneSelector::Active);
            let request = build_transfer(ctx, handle, move_files, sources, &dest, &cwd).await?;
            let id = crate::cmd::operations::start_operation(ctx.clone(), request)
                .await
                .map_err(err)?;
            Ok(ControlResponse::Text(format!("operation {id}")))
        }
    }
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

    let file_reader = ctx.file_reader().map_err(err)?;
    let dest_details = file_reader.file_details(dest_path.clone()).await.ok();

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
            let parent_details = file_reader
                .file_details(parent.clone())
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
