use std::io::Read;
use std::path::PathBuf;

use newt_common::operation::{
    IssueAction, IssueResponse, OperationId, OperationRequest, ResolveIssueRequest,
    StartOperationRequest,
};
use newt_common::terminal::TerminalHandle;
use tauri::ipc::Invoke;
use tauri::Manager;
use tauri::WebviewWindow;
use tauri::Window;
use tauri::Wry;
use url::Url;

use crate::common::Error;

use crate::main_window::pane::Sorting;
use crate::main_window::OperationState;
use crate::main_window::OperationStatus;

use crate::main_window::MainWindowContext;
use crate::main_window::ModalContext;
use crate::main_window::ModalData;
use crate::main_window::ModalDataKind;
use crate::main_window::PaneHandle;

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
    let mut path = path.to_string();
    if !exact {
        path = ctx
            .fs()
            .shell_expand(path.to_string())
            .await?
            .to_string_lossy()
            .to_string()
    }

    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate(path).await?;
        Ok(())
    })
    .await
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
    filename: String,
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
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return,
    };

    let mut url = Url::parse("newt-preview://viewer").unwrap();
    url.query_pairs_mut()
        .append_pair("path", full_path.to_string_lossy().as_ref());

    let window = tauri::WebviewWindowBuilder::new(
        window.app_handle(),
        uuid::Uuid::new_v4().to_string(),
        tauri::WebviewUrl::External(url),
    )
    .title(format!("{} - Viewer", full_path.display()))
    .center()
    .focused(true)
    .build()
    .unwrap();

    window.eval(crate::viewer::SCRIPT).unwrap();
}

#[tauri::command]
async fn new_window(handle: tauri::AppHandle) {
    tauri::WebviewWindowBuilder::new(
        &handle,
        uuid::Uuid::new_v4().to_string(),
        tauri::WebviewUrl::App("/".into()), /* the url */
    )
    .build()
    .unwrap();
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

    opener::open(full_path)?;

    Ok(())
}

#[tauri::command]
async fn open_folder(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let full_path = pane.path();

    opener::open(full_path)?;

    Ok(())
}

#[tauri::command]
async fn read_file(filename: String) -> Result<String, Error> {
    let mut file = std::fs::File::open(filename)?;
    let metadata = file.metadata()?;

    if metadata.len() > 10 * 1024 * 1024 {
        return Err(Error::Custom("file too large to be previewed".into()));
    }

    let mut vec = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut vec)?;

    // TODO: do-this in place to avoid allocation
    Ok(String::from_utf8_lossy(&vec).to_string())
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
        text.push_str(line.to_string_lossy().as_ref());
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
pub fn zoom(window: tauri::Webview, factor: f64) -> Result<(), Error> {
    window.with_webview(move |webview| {
        #[cfg(target_os = "linux")]
        {
            // see https://docs.rs/webkit2gtk/0.18.2/webkit2gtk/struct.WebView.html
            // and https://docs.rs/webkit2gtk/0.18.2/webkit2gtk/trait.WebViewExt.html
            use webkit2gtk::WebViewExt;
            webview.inner().set_zoom_level(factor);
        }

        #[cfg(windows)]
        unsafe {
            // see https://docs.rs/webview2-com/0.19.1/webview2_com/Microsoft/Web/WebView2/Win32/struct.ICoreWebView2Controller.html
            webview.controller().SetZoomFactor(factor).unwrap();
        }

        #[cfg(target_os = "macos")]
        unsafe {
            let () = msg_send![webview.inner(), setPageZoom: factor];
        }
    })?;

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
        ctx.create_terminal(Some(&pane.path())).await?
    };

    let input: Vec<_> = pane
        .get_effective_selection()
        .iter()
        .filter_map(|p| {
            p.file_name().map(shell_quote::bash::escape).map(|mut b| {
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
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();

    ctx.with_update(|gs| {
        let mut modal_state = gs.modal.0.write();
        *modal_state = Some(ModalData {
            kind: match &dialog[..] {
                "navigate" => ModalDataKind::Navigate { path: pane.path() },
                "create_directory" => ModalDataKind::CreateDirectory { path: pane.path() },
                "create_file" => ModalDataKind::CreateFile { path: pane.path() },
                "properties" => {
                    /*let vs = pane.view_state();
                    let selection = pane.get_effective_selection();

                    let mode = HashSet<strin>;

                    for f in vs.files {
                        for s in selection {
                            if f.name == s.file_name() {

                            }
                        }
                    }

                    pane.view_state() view_state().
                    ModalDataKind::Properties {
                        path: pane.path(),
                    }*/
                    todo!()
                }
                "rename" => ModalDataKind::Rename {
                    base_path: pane.path(),
                    name: match pane.view_state().focused {
                        Some(ref selected) => selected.clone(),
                        None => return Ok(()),
                    },
                },
                "copy" | "move" => {
                    let sources = pane.get_effective_selection();
                    if sources.is_empty() {
                        return Ok(());
                    }
                    let other_pane = gs.other_pane(pane_handle);
                    ModalDataKind::CopyMove {
                        kind: dialog.clone(),
                        sources,
                        destination: other_pane.path(),
                    }
                }
                _ => return Err(Error::Custom(format!("unknown dialog: {}", dialog))),
            },
            context: ModalContext {
                pane_handle: Some(pane_handle),
            },
        });

        Ok(())
    })
}

#[tauri::command]
pub async fn create_directory(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    path: String,
    name: String,
) -> Result<(), Error> {
    let dir_path = PathBuf::from(path.clone());
    let dir_path = dir_path.join(name.clone());

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
    path: String,
    name: String,
) -> Result<(), Error> {
    let dir_path = PathBuf::from(path.clone());
    let file_path = dir_path.join(name.clone());

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
    let fs = ctx.fs();

    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        let selected = pane.get_effective_selection();

        let ret = fs.delete_all(selected).await;
        pane.refresh(None).await?;

        ret?;

        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn rename(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    base_path: String,
    old_name: String,
    new_name: String,
) -> Result<(), Error> {
    let old_path = PathBuf::from(base_path.clone()).join(old_name.clone());
    let new_path = PathBuf::from(base_path).join(new_name.clone());

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
            format!(
                "Copying {} item(s) to {}",
                sources.len(),
                destination.display()
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
                destination.display()
            ),
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
            },
        );
    }
    ctx.publish()?;

    // Send to agent
    let req = StartOperationRequest { id, request };
    let ret: Result<(), newt_common::Error> = match ctx
        .communicator()
        .invoke(newt_common::api::API_START_OPERATION, &req)
        .await
    {
        Ok(ret) => ret,
        Err(e) => {
            // Agent communication failed — mark operation as failed so it doesn't get stuck
            let mut ops = ctx.operations().0.write();
            if let Some(op) = ops.get_mut(&id) {
                op.status = OperationStatus::Failed;
                op.error = Some(e.to_string());
            }
            ctx.publish()?;
            return Err(e.into());
        }
    };
    if let Err(e) = ret {
        // Agent returned an error — mark operation as failed
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
    let ret: Result<(), newt_common::Error> = ctx
        .communicator()
        .invoke(newt_common::api::API_CANCEL_OPERATION, &operation_id)
        .await?;
    ret?;
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

    let ret: Result<(), newt_common::Error> = ctx
        .communicator()
        .invoke(newt_common::api::API_RESOLVE_ISSUE, &req)
        .await?;
    ret?;
    Ok(())
}

#[tauri::command]
pub fn dismiss_operation(
    ctx: MainWindowContext,
    operation_id: OperationId,
) -> Result<(), Error> {
    {
        let mut ops = ctx.operations().0.write();
        ops.remove(&operation_id);
    }
    ctx.publish()?;
    Ok(())
}

#[tauri::command]
pub fn close_window(window: Window) -> Result<(), Error> {
    window.close()?;

    Ok(())
}

pub fn create_handler() -> Box<dyn Fn(Invoke<Wry>) -> bool + Send + Sync + 'static> {
    Box::new(tauri::generate_handler![
        cancel,
        navigate,
        ping,
        focus,
        set_sorting,
        toggle_selected,
        select_range,
        select_all,
        deselect_all,
        relative_jump,
        set_filter,
        copy_pane,
        new_window,
        toggle_hidden,
        read_file,
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
        close_window
    ])
}
