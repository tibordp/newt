use crate::GlobalContext;
use crate::common::Error;
use crate::runtime_state::RuntimeState;

#[tauri::command]
#[specta::specta]
pub fn get_runtime_state(
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<RuntimeState, Error> {
    Ok(global_ctx.runtime_state().state())
}

#[tauri::command]
#[specta::specta]
pub fn update_runtime_state(
    global_ctx: tauri::State<'_, GlobalContext>,
    key: String,
    value: serde_json::Value,
) -> Result<(), Error> {
    global_ctx
        .runtime_state()
        .update_key(&key, value)
        .map_err(Error::Custom)
}

#[tauri::command]
#[specta::specta]
pub fn forget_recent_connection(
    global_ctx: tauri::State<'_, GlobalContext>,
    kind: crate::connections::ConnectionKind,
) -> Result<(), Error> {
    global_ctx
        .runtime_state()
        .forget_recent_connection(&kind.identity())
        .map_err(Error::Custom)
}
