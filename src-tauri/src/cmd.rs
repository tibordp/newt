use std::io::Read;

use tauri::Invoke;
use tauri::Manager;
use tauri::Window;
use tauri::Wry;

use crate::common::Error;
use crate::main_window::pane::Sorting;
use crate::main_window::MainWindowContext;
use crate::main_window::PaneHandle;

#[tauri::command]
pub fn navigate(ctx: MainWindowContext, pane_handle: PaneHandle, path: &str) -> Result<(), Error> {
    ctx.with_update_pane(pane_handle, |pane| {
        pane.navigate(path)?;
        Ok(())
    })
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
            state.write().focus(filename);
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
    ctx.with_update_pane(pane_handle, |pane| {
        pane.set_sorting(sorting);
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
    ctx.with_update_pane(pane_handle, |pane| {
        pane.toggle_selected(filename, focus_next);
        Ok(())
    })
}

#[tauri::command]
pub fn select_range(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filename: String,
) -> Result<(), Error> {
    ctx.with_update_pane(pane_handle, |pane| {
        pane.select_range(filename);
        Ok(())
    })
}

#[tauri::command]
pub fn select_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update_pane(pane_handle, |pane| {
        pane.select_all();
        Ok(())
    })
}

#[tauri::command]
pub fn deselect_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update_pane(pane_handle, |pane| {
        pane.deselect_all();
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
    ctx.with_update_pane(pane_handle, |pane| {
        pane.relative_jump(offset, with_selection);
        Ok(())
    })
}

#[tauri::command]
pub fn set_filter(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    filter: Option<String>,
) -> Result<(), Error> {
    ctx.with_update_pane(pane_handle, |pane| {
        pane.set_filter(filter);
        Ok(())
    })
}

#[tauri::command]
pub fn copy_pane(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|gs| {
        gs.copy_pane(pane_handle);
        Ok(())
    })
}

#[tauri::command]
async fn view(window: Window, ctx: MainWindowContext, pane_handle: PaneHandle, filename: String) {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let pane = pane.read();

    let full_path = pane.path.join(filename);

    let base_url = window.url();
    let mut url = base_url.join("/viewer").unwrap();
    url.query_pairs_mut()
        .append_pair("path", full_path.to_string_lossy().as_ref());

    tauri::WindowBuilder::new(
        &window.app_handle(),
        uuid::Uuid::new_v4().to_string(),
        tauri::WindowUrl::App(url.to_string().into()), /* the url */
    )
    .center()
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
    let pane = pane.read();

    let full_path = pane.path.join(filename);
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
    ctx.with_update(|_| Ok(()))
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
    let pane = pane.read();

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
pub fn paste_from_clipboard(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let mut clipboard = arboard::Clipboard::new()?;
    let text = clipboard.get_text()?;

    ctx.with_update_pane(pane_handle, |pane| {
        pane.navigate(text.trim())?;
        Ok(())
    })
}

pub fn create_handler() -> Box<dyn Fn(Invoke<Wry>) + Send + Sync + 'static> {
    Box::new(tauri::generate_handler![
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
        paste_from_clipboard
    ])
}
