use tauri::AppHandle;
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
        pane.navigate(path.into())?;
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
            state.lock().unwrap().focus(filename);
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
pub fn toggle_selected(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update_pane(pane_handle, |pane| {
        pane.toggle_selected();
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
) -> Result<(), Error> {
    ctx.with_update_pane(pane_handle, |pane| {
        pane.relative_jump(offset);
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
    let pane = pane.lock().unwrap();

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
async fn read_file(filename: String) -> Result<String, Error> {
    Ok(std::fs::read_to_string(filename)?)
}


#[tauri::command]
pub fn ping(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_update(|_| Ok(()))
}

pub fn create_handler() -> Box<dyn Fn(Invoke<Wry>) + Send + Sync + 'static> {
    Box::new(tauri::generate_handler![
        navigate,
        ping,
        focus,
        set_sorting,
        toggle_selected,
        select_all,
        deselect_all,
        relative_jump,
        set_filter,
        copy_pane,
        new_window,
        read_file,
        view,
    ])
}
