use tauri::State;

use crate::{common::Error, pane::Sorting, PaneHandle, WindowContext};

#[tauri::command]
pub fn navigate(
    ctx: State<WindowContext>,
    pane_handle: PaneHandle,
    path: &str,
) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        state.lock().unwrap().navigate(path.into())?;

        Ok(())
    })
}

#[tauri::command]
pub fn focus(
    ctx: State<WindowContext>,
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
    ctx: State<WindowContext>,
    pane_handle: PaneHandle,
    sorting: Sorting,
) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        state.lock().unwrap().set_sorting(sorting);
        Ok(())
    })
}

#[tauri::command]
pub fn toggle_selected(ctx: State<WindowContext>, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        state.lock().unwrap().toggle_selected();
        Ok(())
    })
}

#[tauri::command]
pub fn select_all(ctx: State<WindowContext>, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        state.lock().unwrap().select_all();
        Ok(())
    })
}

#[tauri::command]
pub fn deselect_all(ctx: State<WindowContext>, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        state.lock().unwrap().deselect_all();
        Ok(())
    })
}

#[tauri::command]
pub fn relative_jump(
    ctx: State<WindowContext>,
    pane_handle: PaneHandle,
    offset: i32,
) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        state.lock().unwrap().relative_jump(offset);
        Ok(())
    })
}

#[tauri::command]
pub fn set_filter(
    ctx: State<WindowContext>,
    pane_handle: PaneHandle,
    filter: Option<String>,
) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        let state = gs.panes.get(pane_handle).unwrap();
        state.lock().unwrap().set_filter(filter);
        Ok(())
    })
}

#[tauri::command]
pub fn copy_pane(ctx: State<WindowContext>, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_updates(|gs| {
        gs.copy_pane(pane_handle);
        Ok(())
    })
}

#[tauri::command]
pub fn ping(ctx: State<WindowContext>) -> Result<(), Error> {
    ctx.with_updates(|_| Ok(()))
}
