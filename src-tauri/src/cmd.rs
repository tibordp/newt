use std::io::Read;
use std::path::PathBuf;

use log::debug;
use tauri::Invoke;
use tauri::Manager;
use tauri::Window;
use tauri::Wry;

use crate::common::Error;

use crate::main_window::pane::Sorting;

use crate::main_window::MainWindowContext;
use crate::main_window::ModalContext;
use crate::main_window::ModalData;
use crate::main_window::ModalDataKind;
use crate::main_window::PaneHandle;
use crate::main_window::TerminalHandle;

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
) -> Result<(), Error> {
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
async fn view(window: Window, ctx: MainWindowContext, pane_handle: PaneHandle, filename: String) {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let full_path = pane.path().join(filename);

    let base_url = window.url();
    let mut url = base_url.join("/viewer").unwrap();
    url.query_pairs_mut()
        .append_pair("path", full_path.to_string_lossy().as_ref());

    tauri::WindowBuilder::new(
        &window.app_handle(),
        uuid::Uuid::new_v4().to_string(),
        tauri::WindowUrl::App(url.to_string().into()), /* the url */
    )
    .title(format!("{} - viewer", full_path.display()))
    .center()
    .focused(true)
    .build()
    .unwrap();
}

#[tauri::command]
async fn new_window(handle: tauri::AppHandle) {
    tauri::WindowBuilder::new(
        &handle,
        uuid::Uuid::new_v4().to_string(),
        tauri::WindowUrl::App("/".into()), /* the url */
    )
    .build()
    .unwrap();
}

#[tauri::command]
async fn open(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filename: String,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let full_path = pane.path().join(filename);

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
pub fn zoom(window: Window, factor: f64) -> Result<(), Error> {
    window.with_webview(move |webview| {
        #[cfg(target_os = "linux")]
        {
            // see https://docs.rs/webkit2gtk/0.18.2/webkit2gtk/struct.WebView.html
            // and https://docs.rs/webkit2gtk/0.18.2/webkit2gtk/trait.WebViewExt.html
            use webkit2gtk::traits::WebViewExt;
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
    _filename: String,
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

    terminal.input(input)?;

    Ok(())
}

#[tauri::command]
pub fn terminal_write(
    ctx: MainWindowContext,
    handle: TerminalHandle,
    data: Vec<u8>,
) -> Result<(), Error> {
    let term = ctx
        .terminals()
        .get(handle)
        .ok_or_else(|| Error::Custom("terminal does not exit".into()))?;
    term.input(data)?;

    Ok(())
}

#[tauri::command]
pub fn terminal_resize(
    ctx: MainWindowContext,
    handle: TerminalHandle,
    rows: u16,
    cols: u16,
) -> Result<(), Error> {
    let term = ctx
        .terminals()
        .get(handle)
        .ok_or_else(|| Error::Custom("terminal does not exit".into()))?;
    term.resize(rows, cols)?;

    Ok(())
}

#[tauri::command]
pub fn focus_terminal(ctx: MainWindowContext, handle: TerminalHandle) -> Result<(), Error> {
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
                "rename" => ModalDataKind::Rename {
                    base_path: pane.path(),
                    name: match pane.view_state().focused {
                        Some(ref selected) => selected.clone(),
                        None => return Ok(()),
                    },
                },
                _ => panic!(),
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

pub fn create_handler() -> Box<dyn Fn(Invoke<Wry>) + Send + Sync + 'static> {
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
        view,
        copy_to_clipboard,
        paste_from_clipboard,
        zoom,
        terminal_write,
        terminal_resize,
        send_to_terminal,
        close_modal,
        dialog,
        create_directory,
        delete_selected,
        rename
    ])
}
