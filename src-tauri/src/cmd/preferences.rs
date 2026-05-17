use tauri::Manager;

use crate::GlobalContext;
use crate::common::Error;
use crate::main_window::{MainWindowContext, PaneHandle};

#[tauri::command]
#[specta::specta]
pub fn get_preferences(
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<crate::preferences::ResolvedPreferences, Error> {
    Ok(global_ctx.preferences().resolved())
}

#[tauri::command]
#[specta::specta]
pub fn update_preference(
    global_ctx: tauri::State<'_, GlobalContext>,
    key: String,
    value: serde_json::Value,
) -> Result<(), Error> {
    let prefs = global_ctx.preferences();
    prefs
        .update_preference(&key, value)
        .map_err(Error::Custom)?;
    prefs.reload();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn reset_preference(
    global_ctx: tauri::State<'_, GlobalContext>,
    key: String,
) -> Result<(), Error> {
    let prefs = global_ctx.preferences();
    prefs.reset_preference(&key).map_err(Error::Custom)?;
    prefs.reload();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_preferences_schema(
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<serde_json::Value, Error> {
    Ok(global_ctx.preferences().resolved().schema)
}

#[tauri::command]
#[specta::specta]
pub fn set_command_keybinding(
    global_ctx: tauri::State<'_, GlobalContext>,
    command_id: String,
    new_key: Option<String>,
    new_when: Option<String>,
) -> Result<(), Error> {
    let prefs = global_ctx.preferences();
    prefs
        .set_command_keybinding(&command_id, new_key, new_when)
        .map_err(Error::Custom)?;
    prefs.reload();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn reset_command_keybinding(
    global_ctx: tauri::State<'_, GlobalContext>,
    command_id: String,
) -> Result<(), Error> {
    let prefs = global_ctx.preferences();
    prefs
        .reset_command_keybinding(&command_id)
        .map_err(Error::Custom)?;
    prefs.reload();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn open_config_file(global_ctx: tauri::State<'_, GlobalContext>) -> Result<(), Error> {
    let path = global_ctx.preferences().settings_file_path();
    // Create the file with defaults if it doesn't exist
    if !path.exists() {
        std::fs::write(
            &path,
            "# Newt settings\n# See documentation for available options.\n\n[appearance]\n\n[behavior]\n",
        )?;
    }
    opener::open(&path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Hot paths and bookmarks
// ---------------------------------------------------------------------------

#[tauri::command]
#[specta::specta]
pub async fn get_hot_paths(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<Vec<newt_common::hot_paths::HotPathEntry>, Error> {
    use newt_common::hot_paths::{HotPathCategory, HotPathEntry};
    use newt_common::vfs::VfsPath;

    let prefs = global_ctx.preferences().resolved();
    let hp_settings = &prefs.settings.hot_paths;

    // Fetch system-provided paths from the provider (runs on agent in remote mode)
    let mut entries = ctx.hot_paths_provider()?.system_hot_paths().await?;

    // Filter by enabled categories
    entries.retain(|e| match e.category {
        HotPathCategory::UserBookmark => true, // always shown
        HotPathCategory::StandardFolder => hp_settings.standard_folders,
        HotPathCategory::Bookmark => hp_settings.system_bookmarks,
        HotPathCategory::Mount => hp_settings.mounts,
        HotPathCategory::RecentFolder => hp_settings.recent_folders,
    });

    // Add user-defined bookmarks from preferences (always included)
    for bm in &prefs.bookmarks {
        entries.push(HotPathEntry {
            path: VfsPath::new(
                newt_common::vfs::VfsId::ROOT,
                newt_common::vfs::local::local_path_from_native(std::path::Path::new(&bm.path)),
            ),
            display_path: String::new(),
            name: bm.name.clone(),
            category: HotPathCategory::UserBookmark,
        });
    }

    // Render each path through the VFS descriptor — the provider can't
    // (no mounted-VFS context), so the menu would otherwise show raw
    // sentinel paths (`/?/C:/…`) instead of `C:\…`.
    for entry in entries.iter_mut() {
        entry.display_path = ctx.format_vfs_path(&entry.path);
    }

    Ok(entries)
}

#[tauri::command]
#[specta::specta]
pub fn add_bookmark(
    global_ctx: tauri::State<'_, GlobalContext>,
    path: String,
    name: Option<String>,
) -> Result<(), Error> {
    global_ctx
        .preferences()
        .add_bookmark(&path, name.as_deref())
        .map_err(Error::Custom)
}

#[tauri::command]
#[specta::specta]
pub fn remove_bookmark(
    global_ctx: tauri::State<'_, GlobalContext>,
    path: String,
) -> Result<(), Error> {
    global_ctx
        .preferences()
        .remove_bookmark(&path)
        .map_err(Error::Custom)
}

#[tauri::command]
#[specta::specta]
pub fn cmd_add_bookmark(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<GlobalContext> = app_handle.state();

    let pane = ctx.panes().get(pane_handle).unwrap();
    let path = pane.path();

    let path_str = ctx.format_vfs_path(&path);
    let name = path.file_name().map(str::to_string);

    global_ctx
        .preferences()
        .add_bookmark(&path_str, name.as_deref())
        .map_err(Error::Custom)
}

#[tauri::command]
#[specta::specta]
pub fn cmd_open_config_file(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<GlobalContext> = app_handle.state();
    open_config_file(global_ctx)
}
