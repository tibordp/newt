use tauri::{State, Window};

use crate::{common::Error, pane::Sorting, GlobalState, PaneHandle, UpdatePayload};

#[tauri::command]
pub fn navigate(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
    path: &str,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    state.lock().unwrap().navigate(path.into())?;

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;
    Ok(())
}

#[tauri::command]
pub fn focus(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
    filename: Option<String>,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    if let Some(filename) = filename {
        state.lock().unwrap().focus(filename);
    }

    gs.activate_pane(pane_handle);
    window.emit("updated", UpdatePayload::new((*gs).clone()))?;

    Ok(())
}

#[tauri::command]
pub fn set_sorting(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
    sorting: Sorting,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    state.lock().unwrap().set_sorting(sorting);

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;

    Ok(())
}

#[tauri::command]
pub fn toggle_selected(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    state.lock().unwrap().toggle_selected();

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;

    Ok(())
}

#[tauri::command]
pub fn select_all(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    state.lock().unwrap().select_all();

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;

    Ok(())
}

#[tauri::command]
pub fn deselect_all(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    state.lock().unwrap().deselect_all();

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;

    Ok(())
}

#[tauri::command]
pub fn relative_jump(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
    offset: i32,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    state.lock().unwrap().relative_jump(offset);

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;

    Ok(())
}

#[tauri::command]
pub fn set_filter(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
    filter: Option<String>,
) -> Result<(), Error> {
    let state = gs.panes.get(pane_handle).unwrap();
    state.lock().unwrap().set_filter(filter);

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;
    Ok(())
}

#[tauri::command]
pub fn copy_pane(
    gs: State<GlobalState>,
    window: Window,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    gs.copy_pane(pane_handle);

    window.emit("updated", UpdatePayload::new((*gs).clone()))?;
    Ok(())
}

#[tauri::command]
pub fn ping(gs: State<GlobalState>, window: Window) -> Result<(), Error> {
    window.emit("updated", UpdatePayload::new((*gs).clone()))?;
    Ok(())
}
