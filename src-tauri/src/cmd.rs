use newt_common::file_reader::FileChunk;
use newt_common::file_reader::FileInfo;
use newt_common::operation::{
    IssueAction, IssueResponse, OperationId, OperationRequest, ResolveIssueRequest,
    StartOperationRequest,
};
use newt_common::terminal::TerminalHandle;
use newt_common::vfs::{MountRequest, VfsPath};
use shell_quote::Quote;
use tauri::ipc::Invoke;
use tauri::Manager;
use tauri::WebviewWindow;
use tauri::Window;
use tauri::Wry;

use crate::common::Error;

use crate::main_window::pane::Sorting;
use crate::main_window::OperationState;
use crate::main_window::OperationStatus;

use crate::main_window::DndData;
use crate::main_window::DndFile;
use crate::main_window::InitEvent;
use crate::main_window::MainWindowContext;
use crate::main_window::ModalContext;
use crate::main_window::ModalData;
use crate::main_window::ModalDataKind;
use crate::main_window::PaneHandle;
use crate::GlobalContext;

#[tauri::command]
pub async fn init(
    webview: tauri::Webview,
    global_ctx: tauri::State<'_, GlobalContext>,
    on_event: tauri::ipc::Channel<InitEvent>,
) -> Result<(), Error> {
    // Already initialized (e.g. local mode via on_page_load) — just publish state.
    if let Some(ctx) = global_ctx.main_window(&webview) {
        ctx.publish_full()?;
        return Ok(());
    }

    let label = webview.label().to_string();
    let webview_window = webview
        .app_handle()
        .get_webview_window(&label)
        .expect("webview window not found");

    let ctx = MainWindowContext::create(
        webview_window,
        global_ctx.connection_target.clone(),
        global_ctx.window_title.clone(),
        Some(&on_event),
    )
    .await?;

    global_ctx.main_windows.lock().insert(label, ctx.clone());
    ctx.publish_full()?;
    Ok(())
}

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
        let expanded = ctx.shell_service().shell_expand(path.to_string()).await?;

        ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
            gs.close_modal();
            pane.navigate_to(expanded).await?;
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
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().set_sorting(sorting);
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
pub fn select_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().select_all();
        Ok(())
    })
}

#[tauri::command]
pub fn deselect_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().deselect_all();
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
pub fn set_filter(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filter: Option<String>,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().set_filter(filter);
        Ok(())
    })
}

#[tauri::command]
pub async fn copy_pane(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update_async(|gs| async move { gs.copy_pane(pane_handle).await })
        .await
}

#[tauri::command]
async fn view(window: WebviewWindow, ctx: MainWindowContext, pane_handle: PaneHandle) {
    let pane = ctx.panes().get(pane_handle).unwrap();
    if pane.is_focused_dir() {
        return;
    }
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return,
    };

    let viewer_label = uuid::Uuid::new_v4().to_string();

    // Pre-register the parent's MainWindowContext for the viewer window label
    // so that on_page_load sees it and doesn't spawn a new agent.
    {
        let app_handle = window.app_handle();
        let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
        global_ctx
            .main_windows
            .lock()
            .insert(viewer_label.clone(), ctx.clone());
    }

    let path_display = full_path.to_string();
    let vfs_path_json = serde_json::to_string(&full_path).unwrap();
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", &path_display)
        .append_pair("vfs_path", &vfs_path_json)
        .finish();
    let url_path = format!("/viewer?{}", query);

    tauri::WebviewWindowBuilder::new(
        window.app_handle(),
        &viewer_label,
        tauri::WebviewUrl::App(url_path.into()),
    )
    .title(format!("{} - Viewer", path_display))
    .center()
    .focused(true)
    .inner_size(800.0, 600.0)
    .build()
    .unwrap();
}

#[tauri::command]
async fn new_window() -> Result<(), Error> {
    let exe = std::env::current_exe()?;
    tokio::process::Command::new(exe).spawn()?;
    Ok(())
}

#[tauri::command]
async fn open(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filename: Option<String>,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();

    let full_path = if let Some(filename) = filename {
        pane.path().join(filename)
    } else {
        match pane.get_focused_file() {
            Some(s) => s,
            None => return Ok(()),
        }
    };

    // open only works for local paths
    opener::open(&full_path.path)?;

    Ok(())
}

#[tauri::command]
async fn open_folder(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let full_path = pane.path();

    opener::open(&full_path.path)?;

    Ok(())
}

#[tauri::command]
async fn file_info(ctx: MainWindowContext, path: VfsPath) -> Result<FileInfo, Error> {
    let info = ctx.file_reader().file_info(path).await?;
    Ok(info)
}

#[tauri::command]
async fn read_file_range(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    length: u64,
) -> Result<FileChunk, Error> {
    let chunk = ctx.file_reader().read_range(path, offset, length).await?;
    Ok(chunk)
}

#[tauri::command]
pub fn ping(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.publish_full()
}

#[tauri::command]
pub fn toggle_hidden(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_update(|c| {
        c.toggle_hidden();
        Ok(())
    })
}

#[tauri::command]
pub fn copy_to_clipboard(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();

    #[cfg(windows)]
    const LINE_ENDING: &'static str = "\r\n";
    #[cfg(not(windows))]
    const LINE_ENDING: &str = "\n";

    let mut text = String::new();
    for line in pane.get_effective_selection() {
        text.push_str(&line.to_string());
        text.push_str(LINE_ENDING);
    }

    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text)?;

    Ok(())
}

#[tauri::command]
pub async fn paste_from_clipboard(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let mut clipboard = arboard::Clipboard::new()?;
    let text = clipboard.get_text()?;

    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        pane.navigate(text.trim()).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn zoom(webview: tauri::Webview, factor: f64) -> Result<(), Error> {
    webview
        .set_zoom(factor)
        .map_err(|_| Error::Custom("terminal does not exit".into()))?;

    Ok(())
}

#[tauri::command]
pub async fn send_to_terminal(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let terminal = if let Some(terminal) = ctx.active_terminal() {
        ctx.with_update(|c| {
            c.display_options.0.write().panes_focused = false;
            Ok(())
        })?;
        terminal
    } else {
        ctx.create_terminal(Some(&pane.path().path)).await?
    };

    let input: Vec<_> = pane
        .get_effective_selection()
        .iter()
        .filter_map(|p| {
            p.path
                .file_name()
                .map(shell_quote::Bash::quote)
                .map(|mut b: Vec<u8>| {
                    b.push(b' ');
                    b
                })
        })
        .flatten()
        .collect();

    terminal.input(input).await?;

    Ok(())
}

#[tauri::command]
pub async fn terminal_write(
    ctx: MainWindowContext,
    handle: TerminalHandle,
    data: Vec<u8>,
) -> Result<(), Error> {
    let term = ctx
        .terminals()
        .get(handle)
        .ok_or_else(|| Error::Custom("terminal does not exit".into()))?;
    term.input(data).await?;

    Ok(())
}

#[tauri::command]
pub async fn terminal_resize(
    ctx: MainWindowContext,
    handle: TerminalHandle,
    rows: u16,
    cols: u16,
) -> Result<(), Error> {
    let term = ctx
        .terminals()
        .get(handle)
        .ok_or_else(|| Error::Custom("terminal does not exit".into()))?;
    term.resize(rows, cols).await?;

    Ok(())
}

#[tauri::command]
pub fn terminal_focus(ctx: MainWindowContext, handle: TerminalHandle) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let mut opts = gs.display_options.0.write();
        opts.active_terminal = Some(handle);
        opts.panes_focused = false;

        Ok(())
    })
}

#[tauri::command]
pub fn close_modal(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_update(|gs| {
        gs.close_modal();
        Ok(())
    })
}

#[tauri::command]
pub fn dialog(
    ctx: MainWindowContext,
    dialog: String,
    pane_handle: Option<PaneHandle>,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let pane = pane_handle.map(|h| gs.panes.get(h).unwrap());
        let mut modal_state = gs.modal.0.write();
        *modal_state = Some(ModalData {
            kind: match &dialog[..] {
                "navigate" => ModalDataKind::Navigate {
                    path: pane.unwrap().path(),
                },
                "create_directory" => ModalDataKind::CreateDirectory {
                    path: pane.unwrap().path(),
                },
                "create_file" => ModalDataKind::CreateFile {
                    path: pane.unwrap().path(),
                },
                "properties" => {
                    todo!()
                }
                "rename" => {
                    let pane = pane.unwrap();
                    let name = match pane.view_state().focused {
                        Some(ref selected) => selected.clone(),
                        None => return Ok(()),
                    };
                    ModalDataKind::Rename {
                        base_path: pane.path(),
                        name,
                    }
                }
                "copy" | "move" => {
                    let pane = pane.unwrap();
                    let sources = pane.get_effective_selection();
                    if sources.is_empty() {
                        return Ok(());
                    }
                    let other_pane = gs.other_pane(pane_handle.unwrap());
                    ModalDataKind::CopyMove {
                        kind: dialog.clone(),
                        sources,
                        destination: other_pane.path(),
                    }
                }
                "connect_remote" => ModalDataKind::ConnectRemote {
                    host: String::new(),
                },
                _ => return Err(Error::Custom(format!("unknown dialog: {}", dialog))),
            },
            context: ModalContext { pane_handle },
        });

        Ok(())
    })
}

#[tauri::command]
pub async fn create_directory(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    path: VfsPath,
    name: String,
) -> Result<(), Error> {
    let dir_path = path.join(&name);

    ctx.fs().create_directory(dir_path).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None).await?;
            pane.view_state_mut().focus(name);
        }

        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn touch_file(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    path: VfsPath,
    name: String,
) -> Result<(), Error> {
    let file_path = path.join(&name);

    ctx.fs().touch(file_path).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None).await?;
            pane.view_state_mut().focus(name);
        }

        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn delete_selected(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let paths = pane.get_effective_selection();
    if paths.is_empty() {
        return Ok(());
    }

    let request = OperationRequest::Delete { paths };
    start_operation(ctx, request).await?;
    Ok(())
}

#[tauri::command]
pub async fn rename(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    base_path: VfsPath,
    old_name: String,
    new_name: String,
) -> Result<(), Error> {
    let old_path = base_path.join(&old_name);
    let new_path = base_path.join(&new_name);

    ctx.fs().rename(old_path, new_path).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None).await?;
            pane.view_state_mut().focus(new_name);
        }

        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn start_operation(
    ctx: MainWindowContext,
    request: OperationRequest,
) -> Result<OperationId, Error> {
    let id = ctx.next_operation_id();

    let (kind, description) = match &request {
        OperationRequest::Copy {
            sources,
            destination,
            ..
        } => (
            "copy".to_string(),
            format!("Copying {} item(s) to {}", sources.len(), destination,),
        ),
        OperationRequest::Move {
            sources,
            destination,
            ..
        } => (
            "move".to_string(),
            format!("Moving {} item(s) to {}", sources.len(), destination,),
        ),
        OperationRequest::Delete { paths } => (
            "delete".to_string(),
            format!("Deleting {} item(s)", paths.len()),
        ),
    };

    // Insert initial operation state
    {
        let mut ops = ctx.operations().0.write();
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
            },
        );
    }
    ctx.publish()?;

    // Send to operations client
    let req = StartOperationRequest { id, request };
    if let Err(e) = ctx.operations_client().start_operation(req).await {
        // Operation failed to start — mark as failed so it doesn't get stuck
        let mut ops = ctx.operations().0.write();
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
pub async fn cancel_operation(
    ctx: MainWindowContext,
    operation_id: OperationId,
) -> Result<(), Error> {
    ctx.operations_client()
        .cancel_operation(operation_id)
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn resolve_issue(
    ctx: MainWindowContext,
    operation_id: OperationId,
    issue_id: u64,
    action: String,
    apply_to_all: bool,
) -> Result<(), Error> {
    let issue_action = match action.as_str() {
        "skip" => IssueAction::Skip,
        "overwrite" => IssueAction::Overwrite,
        "retry" => IssueAction::Retry,
        "abort" => IssueAction::Abort,
        _ => return Err(Error::Custom(format!("unknown action: {}", action))),
    };

    let req = ResolveIssueRequest {
        operation_id,
        issue_id,
        response: IssueResponse {
            action: issue_action,
            apply_to_all,
        },
    };

    ctx.operations_client().resolve_issue(req).await?;
    Ok(())
}

#[tauri::command]
pub fn dismiss_operation(ctx: MainWindowContext, operation_id: OperationId) -> Result<(), Error> {
    {
        let mut ops = ctx.operations().0.write();
        ops.remove(&operation_id);
    }
    ctx.publish()?;
    Ok(())
}

#[tauri::command]
pub fn background_operation(
    ctx: MainWindowContext,
    operation_id: OperationId,
) -> Result<(), Error> {
    {
        let mut ops = ctx.operations().0.write();
        if let Some(op) = ops.get_mut(&operation_id) {
            op.backgrounded = true;
        }
    }
    ctx.publish()?;
    Ok(())
}

#[tauri::command]
pub async fn connect_remote(host: String) -> Result<(), Error> {
    let exe = std::env::current_exe()?;
    tokio::process::Command::new(exe)
        .arg("--connect")
        .arg(&host)
        .arg("--title")
        .arg(&host)
        .spawn()?;
    Ok(())
}

#[tauri::command]
pub async fn open_elevated() -> Result<(), Error> {
    let exe = std::env::current_exe()?;
    tokio::process::Command::new(exe)
        .arg("--elevated")
        .arg("--title")
        .arg("Elevated")
        .spawn()?;
    Ok(())
}

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

    start_operation(ctx, request).await
}

#[tauri::command]
pub async fn mount_s3(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let response = ctx.mount_vfs(MountRequest::S3 { region: None }).await?;
    let vfs_path = VfsPath::new(response.vfs_id, "/");

    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_to(vfs_path).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn close_window(window: Window) -> Result<(), Error> {
    window.close()?;

    Ok(())
}

pub fn create_handler() -> Box<dyn Fn(Invoke<Wry>) -> bool + Send + Sync + 'static> {
    Box::new(tauri::generate_handler![
        init,
        cancel,
        navigate,
        ping,
        focus,
        set_sorting,
        toggle_selected,
        select_range,
        select_all,
        deselect_all,
        set_selection,
        relative_jump,
        set_filter,
        copy_pane,
        new_window,
        toggle_hidden,
        file_info,
        read_file_range,
        open,
        open_folder,
        view,
        copy_to_clipboard,
        paste_from_clipboard,
        zoom,
        terminal_write,
        terminal_resize,
        terminal_focus,
        send_to_terminal,
        close_modal,
        dialog,
        create_directory,
        touch_file,
        delete_selected,
        rename,
        start_operation,
        cancel_operation,
        resolve_issue,
        dismiss_operation,
        background_operation,
        connect_remote,
        open_elevated,
        mount_s3,
        close_window,
        start_dnd,
        cancel_dnd,
        execute_dnd
    ])
}
