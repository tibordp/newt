use tauri::{Invoke, Wry};

use crate::common::Error;
use crate::main_window::pane::Sorting;
use crate::main_window::{PaneHandle, MainWindowContext};

#[tauri::command]
pub fn navigate(ctx: MainWindowContext, pane_handle: PaneHandle, path: &str) -> Result<(), Error> {
    ctx.with_pane(pane_handle, |pane| {
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
    ctx.with_updates(|gs| {
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
    ctx.with_pane(pane_handle, |pane| {
        pane.set_sorting(sorting);
        Ok(())
    })
}

#[tauri::command]
pub fn toggle_selected(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane(pane_handle, |pane| {
        pane.toggle_selected();
        Ok(())
    })
}

#[tauri::command]
pub fn select_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane(pane_handle, |pane| {
        pane.select_all();
        Ok(())
    })
}

#[tauri::command]
pub fn deselect_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane(pane_handle, |pane| {
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
    ctx.with_pane(pane_handle, |pane| {
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
    ctx.with_pane(pane_handle, |pane| {
        pane.set_filter(filter);
        Ok(())
    })
}

#[tauri::command]
pub fn copy_pane(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        gs.copy_pane(pane_handle);
        Ok(())
    })
}

#[tauri::command]
async fn new_window(handle: tauri::AppHandle) {
    tauri::WindowBuilder::new(
        &handle,
        uuid::Uuid::new_v4().to_string(),
        tauri::WindowUrl::App("/hello/moto".into()), /* the url */
    )
    .build()
    .unwrap();
}

#[tauri::command]
pub fn ping(ctx: MainWindowContext) -> Result<(), Error> {
    ctx.with_updates(|_| Ok(()))
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
        new_window
    ])
}