use newt_common::file_reader::FileChunk;
use newt_common::file_reader::FileDetails;
use newt_common::operation::{
    CopyOptions, IssueAction, IssueResponse, OperationId, OperationRequest, ResolveIssueRequest,
    StartOperationRequest,
};
use newt_common::terminal::TerminalHandle;
use newt_common::vfs::{MountRequest, VfsId, VfsPath, lookup_descriptor};
use shell_quote::Quote;
use tauri::Manager;
use tauri::Window;
use tauri::Wry;
use tauri::ipc::Invoke;

use crate::common::Error;

use crate::main_window::OperationState;
use crate::main_window::OperationStatus;
use crate::main_window::pane::{FilterMode, Sorting};

use crate::GlobalContext;
use crate::main_window::ConfirmAction;
use crate::main_window::DndData;
use crate::main_window::DndFile;
use crate::main_window::MainWindowContext;
use crate::main_window::ModalContext;
use crate::main_window::ModalData;
use crate::main_window::ModalDataKind;
use crate::main_window::PaneHandle;

#[tauri::command]
pub fn askpass_respond(ctx: MainWindowContext, response: Option<String>) -> Result<(), Error> {
    ctx.askpass_respond(response);
    Ok(())
}

#[tauri::command]
pub async fn init(
    webview: tauri::Webview,
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<(), Error> {
    let ctx = global_ctx
        .main_window(&webview)
        .ok_or_else(|| Error::Custom("window not initialized".into()))?;

    // Already connected (e.g. local mode via on_page_load).
    if ctx.is_connected() {
        return Ok(());
    }

    let agent_resolver = global_ctx.agent_resolver();
    if let Err(e) = ctx.connect(agent_resolver).await {
        ctx.set_connection_failed(e.to_string());
        return Err(e);
    }
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
        // First try resolving as a VFS display path (handles s3://, etc.)
        let target = if let Some(vfs_path) = ctx.resolve_display_path(path) {
            vfs_path
        } else {
            // Fall back to shell expansion (handles local paths, ~, relative paths)
            ctx.shell_service()?.shell_expand(path.to_string()).await?
        };

        ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
            gs.close_modal();
            pane.navigate_to(target).await?;
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
    let folders_first = ctx.preferences().load().appearance.folders_first;
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().set_sorting(sorting, folders_first);
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
pub fn cmd_select_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        pane.view_state_mut().select_all();
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_deselect_all(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
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
    mode: Option<FilterMode>,
) -> Result<(), Error> {
    ctx.with_pane_update(pane_handle, |_, pane| {
        if let Some(mode) = mode {
            pane.view_state_mut().set_filter_with_mode(filter, mode);
        } else {
            pane.view_state_mut().set_filter(filter);
        }
        Ok(())
    })
}

#[tauri::command]
pub async fn cmd_copy_pane(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update_async(|gs| async move { gs.copy_pane(pane_handle).await })
        .await
}

#[tauri::command]
pub async fn cmd_view(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    if pane.is_focused_dir() {
        return Ok(());
    }
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return Ok(()),
    };

    let viewer_label = uuid::Uuid::new_v4().to_string();

    let window = ctx.window().clone();

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

    let path_display = ctx.format_vfs_path(&full_path);
    let vfs_path_json = serde_json::to_string(&full_path).unwrap();
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", &path_display)
        .append_pair("vfs_path", &vfs_path_json)
        .append_pair("file_server_base", &ctx.file_server_base_url()?)
        .finish();
    let url_path = format!("/viewer?{}", query);

    let app_handle = window.app_handle();
    let viewer_window = tauri::WebviewWindowBuilder::new(
        app_handle,
        &viewer_label,
        tauri::WebviewUrl::App(url_path.into()),
    )
    .title(format!("{} - Viewer", path_display))
    .center()
    .focused(true)
    .inner_size(1100.0, 800.0)
    .build()
    .unwrap();

    let viewer = crate::viewer::create_viewer_window(app_handle, &viewer_label, &viewer_window)?;
    {
        let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
        global_ctx
            .register_viewer_window(&viewer_label, crate::viewer::ViewerWindowContext(viewer));
    }

    Ok(())
}

fn open_editor_window(ctx: &MainWindowContext, full_path: &VfsPath) -> Result<(), Error> {
    let editor_label = uuid::Uuid::new_v4().to_string();

    let window = ctx.window().clone();

    // Pre-register the parent's MainWindowContext for the editor window label
    // so that on_page_load sees it and doesn't spawn a new agent.
    {
        let app_handle = window.app_handle();
        let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
        global_ctx
            .main_windows
            .lock()
            .insert(editor_label.clone(), ctx.clone());
    }

    let path_display = ctx.format_vfs_path(full_path);
    let vfs_path_json = serde_json::to_string(full_path).unwrap();
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", &path_display)
        .append_pair("vfs_path", &vfs_path_json)
        .finish();
    let url_path = format!("/editor?{}", query);

    let app_handle = window.app_handle();
    let editor_window = tauri::WebviewWindowBuilder::new(
        app_handle,
        &editor_label,
        tauri::WebviewUrl::App(url_path.into()),
    )
    .title(format!("{} - Editor", path_display))
    .center()
    .focused(true)
    .inner_size(900.0, 700.0)
    .build()
    .unwrap();

    let editor_ctx =
        crate::editor::create_editor_window(app_handle, &editor_label, &editor_window)?;
    {
        let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
        global_ctx.register_editor_window(
            &editor_label,
            crate::editor::EditorWindowContext(editor_ctx),
        );
    }

    Ok(())
}

#[tauri::command]
pub async fn cmd_edit(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    if pane.is_focused_dir() {
        return Ok(());
    }
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return Ok(()),
    };

    open_editor_window(&ctx, &full_path)
}

#[tauri::command]
pub async fn cmd_new_window(_pane_handle: PaneHandle) -> Result<(), Error> {
    let exe = std::env::current_exe()?;
    tokio::process::Command::new(exe).spawn()?;
    Ok(())
}

#[tauri::command]
pub async fn cmd_open(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let full_path = match pane.get_focused_file() {
        Some(s) => s,
        None => return Ok(()),
    };

    // open only works for local paths
    opener::open(&full_path.path)?;

    Ok(())
}

#[tauri::command]
pub async fn cmd_follow_symlink(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let target = match pane.get_focused_symlink_target() {
        Some(t) => t,
        None => return Ok(()),
    };

    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        let resolved = if target.is_absolute() {
            target
        } else {
            pane.path().path.join(&target)
        };
        let parent = resolved.parent().unwrap_or(&resolved).to_path_buf();
        let filename = resolved
            .file_name()
            .map(|n: &std::ffi::OsStr| n.to_string_lossy().to_string());
        pane.navigate(&parent).await?;
        if let Some(name) = filename {
            pane.view_state_mut().focus(name);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cmd_open_folder(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let full_path = pane.path();

    opener::open(&full_path.path)?;

    Ok(())
}

#[tauri::command]
pub async fn cmd_navigate_back(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    ctx.with_pane_update_async(
        pane_handle,
        |_, pane| async move { pane.navigate_back().await },
    )
    .await
}

#[tauri::command]
pub async fn cmd_navigate_forward(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        pane.navigate_forward().await
    })
    .await
}

#[tauri::command]
async fn file_details(ctx: MainWindowContext, path: VfsPath) -> Result<FileDetails, Error> {
    let info = ctx.file_reader()?.file_details(path).await?;
    Ok(info)
}

#[tauri::command]
async fn read_file_range(
    ctx: MainWindowContext,
    path: VfsPath,
    offset: u64,
    length: u64,
) -> Result<FileChunk, Error> {
    let chunk = ctx.file_reader()?.read_range(path, offset, length).await?;
    Ok(chunk)
}

#[tauri::command]
async fn read_file(ctx: MainWindowContext, path: VfsPath, max_size: u64) -> Result<Vec<u8>, Error> {
    let data = ctx.file_reader()?.read_file(path, max_size).await?;
    Ok(data)
}

#[tauri::command]
async fn write_file(ctx: MainWindowContext, path: VfsPath, data: Vec<u8>) -> Result<(), Error> {
    ctx.file_reader()?.write_file(path, data).await?;
    Ok(())
}

#[tauri::command]
pub fn ping(
    webview: tauri::Webview,
    global_ctx: tauri::State<'_, GlobalContext>,
    name: String,
) -> Result<(), Error> {
    let label = webview.label();
    match name.as_str() {
        "viewer" => {
            if let Some(ctx) = global_ctx.viewer_window(label) {
                ctx.0.publish_full();
            }
        }
        "editor" => {
            if let Some(ctx) = global_ctx.editor_window(label) {
                ctx.0.publish_full();
            }
        }
        _ => {
            if let Some(ctx) = global_ctx.main_window(&webview) {
                ctx.publish_full()?;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn cmd_toggle_hidden(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        c.toggle_hidden();
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_copy_to_clipboard(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();

    #[cfg(windows)]
    const LINE_ENDING: &'static str = "\r\n";
    #[cfg(not(windows))]
    const LINE_ENDING: &str = "\n";

    let mut text = String::new();
    for line in pane.get_effective_selection() {
        text.push_str(&ctx.format_vfs_path(&line));
        text.push_str(LINE_ENDING);
    }

    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text)?;

    Ok(())
}

#[tauri::command]
pub async fn cmd_paste_from_clipboard(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let mut clipboard = arboard::Clipboard::new()?;
    let text = clipboard.get_text()?;
    let text = text.trim();

    // Same resolution chain as the navigate command with exact: false
    let target = if let Some(vfs_path) = ctx.resolve_display_path(text) {
        vfs_path
    } else {
        ctx.shell_service()?.shell_expand(text.to_string()).await?
    };

    ctx.with_pane_update_async(pane_handle, |_, pane| async move {
        pane.navigate_to(target).await?;
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
pub async fn cmd_send_to_terminal(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let terminal = if let Some(terminal) = ctx.active_terminal() {
        ctx.with_update(|c| {
            let mut opts = c.display_options.0.write();
            opts.panes_focused = false;
            opts.terminal_panel_visible = true;
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
                "navigate" => {
                    let pane = pane.unwrap();
                    let path = pane.path();
                    let display_path = ctx.format_vfs_path(&path);
                    ModalDataKind::Navigate { path, display_path }
                }
                "create_directory" => ModalDataKind::CreateDirectory {
                    path: pane.unwrap().path(),
                },
                "create_file" => ModalDataKind::CreateFile {
                    path: pane.unwrap().path(),
                    open_editor: false,
                },
                "create_and_edit" => ModalDataKind::CreateFile {
                    path: pane.unwrap().path(),
                    open_editor: true,
                },
                "properties" => {
                    let pane = pane.unwrap();
                    let paths = pane.get_effective_selection();
                    if paths.is_empty() {
                        return Ok(());
                    }

                    let file_list = pane.file_list();
                    let files: Vec<&newt_common::filesystem::File> = paths
                        .iter()
                        .filter_map(|p| {
                            let name = p.file_name()?.to_string_lossy().to_string();
                            file_list.files().iter().find(|f| f.name == name)
                        })
                        .collect();

                    if files.is_empty() {
                        return Ok(());
                    }

                    let name = if files.len() == 1 {
                        files[0].name.clone()
                    } else {
                        format!("{} items", files.len())
                    };

                    let size = if files.iter().all(|f| f.size.is_some()) {
                        Some(files.iter().map(|f| f.size.unwrap_or(0)).sum())
                    } else {
                        None
                    };

                    let is_dir = files.len() == 1 && files[0].is_dir;
                    let is_symlink = files.len() == 1 && files[0].is_symlink;
                    let symlink_target = if files.len() == 1 {
                        files[0]
                            .symlink_target
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string())
                    } else {
                        None
                    };

                    // For mode: if single file, use its mode; if multiple, bitwise AND of all modes
                    let mode = if files.len() == 1 {
                        Some(files[0].mode.0)
                    } else if files.iter().all(|f| f.mode.0 != 0) {
                        Some(files.iter().fold(0o7777, |acc, f| acc & f.mode.0))
                    } else {
                        None
                    };

                    // Owner: show only if identical across all files
                    let owner = if let Some(first) = files[0].user.as_ref() {
                        if files.iter().all(|f| f.user.as_ref() == Some(first)) {
                            Some(first.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Group: show only if identical across all files
                    let group = if let Some(first) = files[0].group.as_ref() {
                        if files.iter().all(|f| f.group.as_ref() == Some(first)) {
                            Some(first.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Timestamps: only for single file
                    let (modified, accessed, created) = if files.len() == 1 {
                        (files[0].modified, files[0].accessed, files[0].created)
                    } else {
                        (None, None, None)
                    };

                    ModalDataKind::Properties {
                        paths,
                        name,
                        size,
                        is_dir,
                        is_symlink,
                        symlink_target,
                        mode,
                        owner,
                        group,
                        modified,
                        accessed,
                        created,
                    }
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
                    let destination = other_pane.path();
                    let display_destination = ctx.format_vfs_path(&destination);
                    let summary = if sources.len() == 1 {
                        sources[0]
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    } else {
                        format!("{} items", sources.len())
                    };
                    ModalDataKind::CopyMove {
                        kind: dialog.clone(),
                        sources,
                        destination,
                        display_destination,
                        summary,
                    }
                }
                "connect_remote" => ModalDataKind::ConnectRemote {
                    host: String::new(),
                },
                "mount_sftp" => ModalDataKind::MountSftp {
                    host: String::new(),
                },
                "select_vfs" => ModalDataKind::SelectVfs {
                    targets: ctx.compute_vfs_targets()?,
                },
                "command_palette" => ModalDataKind::CommandPalette {
                    category_filter: None,
                },
                "user_commands" => ModalDataKind::CommandPalette {
                    category_filter: Some("User".to_string()),
                },
                "hot_paths" => ModalDataKind::HotPaths,
                "settings" => ModalDataKind::Settings,
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

    ctx.fs()?.create_directory(dir_path).await?;

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
    open_editor: Option<bool>,
) -> Result<(), Error> {
    let file_path = path.join(&name);

    ctx.fs()?.touch(file_path.clone()).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None).await?;
            pane.view_state_mut().focus(name);
        }

        Ok(())
    })
    .await?;

    if open_editor.unwrap_or(false) {
        open_editor_window(&ctx, &file_path)?;
    }

    Ok(())
}

#[tauri::command]
pub async fn cmd_delete_selected(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let paths = pane.get_effective_selection();
    if paths.is_empty() {
        return Ok(());
    }

    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<GlobalContext> = app_handle.state();
    let prefs = global_ctx.preferences().settings();

    if prefs.behavior.confirm_delete {
        let message = if paths.len() > 1 {
            format!("Delete {} selected files?", paths.len())
        } else {
            let name = paths[0]
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            format!("Delete {}?", name)
        };
        ctx.with_update(|gs| {
            *gs.modal.0.write() = Some(ModalData {
                kind: ModalDataKind::Confirm {
                    message,
                    action: ConfirmAction::DeleteSelected { paths },
                },
                context: ModalContext {
                    pane_handle: Some(pane_handle),
                },
            });
            Ok(())
        })
    } else {
        let request = OperationRequest::Delete { paths };
        start_operation(ctx, request).await?;
        Ok(())
    }
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

    ctx.fs()?.rename(old_path, new_path).await?;

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
pub async fn set_permissions(
    ctx: MainWindowContext,
    pane_handle: Option<PaneHandle>,
    paths: Vec<VfsPath>,
    mode: u32,
    recursive: bool,
) -> Result<(), Error> {
    let request = OperationRequest::SetPermissions {
        paths,
        mode,
        recursive,
    };
    start_operation(ctx.clone(), request).await?;

    ctx.with_update_async(|gs| async move {
        gs.close_modal();
        if let Some(pane_handle) = pane_handle {
            let pane = gs.panes.get(pane_handle).unwrap();
            pane.refresh(None).await?;
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
    let id = ctx.next_operation_id()?;

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
                ctx.format_vfs_path(destination),
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
                ctx.format_vfs_path(destination),
            ),
        ),
        OperationRequest::Delete { paths } => (
            "delete".to_string(),
            format!("Deleting {} item(s)", paths.len()),
        ),
        OperationRequest::SetPermissions { paths, mode, .. } => (
            "chmod".to_string(),
            format!("Setting permissions {:o} on {} item(s)", mode, paths.len()),
        ),
        OperationRequest::RunCommand { command, .. } => {
            ("command".to_string(), format!("Running: {}", command))
        }
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
    if let Err(e) = ctx.operations_client()?.start_operation(req).await {
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
    ctx.operations_client()?
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

    ctx.operations_client()?.resolve_issue(req).await?;
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
pub async fn cmd_open_elevated(_pane_handle: PaneHandle) -> Result<(), Error> {
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
pub async fn start_copy_move(
    ctx: MainWindowContext,
    kind: String,
    sources: Vec<VfsPath>,
    initial_destination: VfsPath,
    destination_input: String,
    options: CopyOptions,
) -> Result<OperationId, Error> {
    // Resolve the user-typed destination string
    let destination = if let Some(vfs_path) = ctx.resolve_display_path(&destination_input) {
        vfs_path
    } else {
        // No VFS claimed it — treat as a path within the same VFS as the initial destination
        VfsPath::new(initial_destination.vfs_id, destination_input)
    };

    let request = match kind.as_str() {
        "copy" => OperationRequest::Copy {
            sources,
            destination,
            options,
        },
        "move" => OperationRequest::Move {
            sources,
            destination,
            options,
        },
        _ => return Err(Error::Custom(format!("unknown copy/move kind: {}", kind))),
    };

    ctx.with_update(|gs| {
        gs.close_modal();
        Ok(())
    })?;

    start_operation(ctx, request).await
}

#[tauri::command]
pub async fn cmd_mount_s3(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
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
pub async fn mount_sftp(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    host: String,
) -> Result<(), Error> {
    log::info!("cmd: mount_sftp host={} pane={:?}", host, pane_handle);
    let response = ctx
        .mount_vfs(MountRequest::Sftp { host: host.clone() })
        .await
        .map_err(|e| {
            log::error!("cmd: mount_sftp failed for host={}: {}", host, e);
            e
        })?;
    log::info!("cmd: mount_sftp succeeded, vfs_id={:?}", response.vfs_id);
    let vfs_path = VfsPath::new(response.vfs_id, "/");

    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_to(vfs_path).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn switch_vfs(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
    vfs_id: Option<VfsId>,
    type_name: String,
) -> Result<(), Error> {
    let vfs_path = if let Some(id) = vfs_id {
        VfsPath::new(id, "/")
    } else {
        let descriptor = lookup_descriptor(&type_name)
            .ok_or_else(|| Error::Custom(format!("unknown VFS type: {}", type_name)))?;
        let request = descriptor.auto_mount_request().ok_or_else(|| {
            Error::Custom(format!(
                "VFS type {} does not support auto-mount",
                type_name
            ))
        })?;
        let response = ctx.mount_vfs(request).await?;
        VfsPath::new(response.vfs_id, "/")
    };

    ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
        gs.close_modal();
        pane.navigate_to(vfs_path).await?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cmd_create_terminal(
    ctx: MainWindowContext,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    let cwd = ctx.active_pane().map(|p| p.path().path);
    ctx.create_terminal(cwd.as_deref()).await?;
    Ok(())
}

#[tauri::command]
pub fn close_terminal(ctx: MainWindowContext, handle: TerminalHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        c.terminals.remove(handle);
        let mut opts = c.display_options.0.write();
        if opts.active_terminal == Some(handle) {
            opts.active_terminal = c.terminals.first_handle();
        }
        if c.terminals.is_empty() {
            opts.terminal_panel_visible = false;
            opts.panes_focused = true;
        }
        Ok(())
    })
}

#[tauri::command]
pub async fn cmd_toggle_terminal_panel(
    ctx: MainWindowContext,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    let visible = !ctx.terminals().is_empty()
        && ctx.with_update(|c| Ok(c.display_options.0.read().terminal_panel_visible))?;

    if visible {
        // Hide the panel, focus panes
        ctx.with_update(|c| {
            let mut opts = c.display_options.0.write();
            opts.terminal_panel_visible = false;
            opts.panes_focused = true;
            Ok(())
        })
    } else {
        // Show the panel — auto-create a terminal if none exist
        if ctx.terminals().is_empty() {
            let cwd = ctx.active_pane().map(|p| p.path().path);
            ctx.create_terminal(cwd.as_deref()).await?;
        } else {
            ctx.with_update(|c| {
                let mut opts = c.display_options.0.write();
                opts.terminal_panel_visible = true;
                opts.panes_focused = false;
                if opts.active_terminal.is_none() {
                    opts.active_terminal = c.terminals.first_handle();
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

#[tauri::command]
pub fn activate_terminal(ctx: MainWindowContext, handle: TerminalHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let mut opts = c.display_options.0.write();
        opts.active_terminal = Some(handle);
        opts.panes_focused = false;
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_focus_panes(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let mut opts = c.display_options.0.write();
        opts.panes_focused = true;
        Ok(())
    })
}

#[tauri::command]
pub async fn cmd_focus_terminal(
    ctx: MainWindowContext,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    if ctx.terminals().is_empty() {
        let cwd = ctx.active_pane().map(|p| p.path().path);
        ctx.create_terminal(cwd.as_deref()).await?;
    } else {
        ctx.with_update(|c| {
            let mut opts = c.display_options.0.write();
            opts.terminal_panel_visible = true;
            opts.panes_focused = false;
            if opts.active_terminal.is_none() {
                opts.active_terminal = c.terminals.first_handle();
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[tauri::command]
pub fn cmd_next_terminal(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let handles = c.terminals.handles_sorted();
        if handles.is_empty() {
            return Ok(());
        }
        let mut opts = c.display_options.0.write();
        let current = opts.active_terminal;
        let idx = current
            .and_then(|h| handles.iter().position(|&x| x == h))
            .map(|i| (i + 1) % handles.len())
            .unwrap_or(0);
        opts.active_terminal = Some(handles[idx]);
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_prev_terminal(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let handles = c.terminals.handles_sorted();
        if handles.is_empty() {
            return Ok(());
        }
        let mut opts = c.display_options.0.write();
        let current = opts.active_terminal;
        let idx = current
            .and_then(|h| handles.iter().position(|&x| x == h))
            .map(|i| (i + handles.len() - 1) % handles.len())
            .unwrap_or(0);
        opts.active_terminal = Some(handles[idx]);
        Ok(())
    })
}

#[tauri::command]
pub async fn confirm_action(ctx: MainWindowContext) -> Result<(), Error> {
    let action = ctx.with_update(|gs| {
        let modal = gs.modal.0.read().clone();
        let modal = modal.ok_or_else(|| Error::Custom("no modal open".into()))?;
        let action = match modal {
            ModalData {
                kind: ModalDataKind::Confirm { action, .. },
                ..
            } => action,
            _ => return Err(Error::Custom("modal is not a confirm dialog".into())),
        };
        gs.close_modal();
        Ok(action)
    })?;

    match action {
        ConfirmAction::DeleteSelected { paths } => {
            let request = OperationRequest::Delete { paths };
            start_operation(ctx, request).await?;
        }
    }

    Ok(())
}

#[tauri::command]
pub fn close_window(window: Window) -> Result<(), Error> {
    window.close()?;

    Ok(())
}

#[tauri::command]
pub fn destroy_window(window: Window) -> Result<(), Error> {
    window.destroy()?;

    Ok(())
}

/// Update check/radio menu items. If `prefix` is non-empty, acts as a radio
#[tauri::command]
pub fn set_window_title(webview_window: tauri::WebviewWindow, title: String) -> Result<(), Error> {
    // NOTE: set_title doesn't visually update on Wayland (upstream Tauri/GTK bug).
    // Works on X11 and macOS. Keeping it so it works where it can.
    webview_window.set_title(&title)?;

    Ok(())
}

#[tauri::command]
pub fn get_preferences(
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<crate::preferences::ResolvedPreferences, Error> {
    Ok(global_ctx.preferences().resolved())
}

#[tauri::command]
pub fn update_preference(
    global_ctx: tauri::State<'_, GlobalContext>,
    key: String,
    value: serde_json::Value,
) -> Result<(), Error> {
    global_ctx
        .preferences()
        .update_preference(&key, value)
        .map_err(Error::Custom)
}

#[tauri::command]
pub fn get_preferences_schema(
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<serde_json::Value, Error> {
    Ok(global_ctx.preferences().resolved().schema)
}

#[tauri::command]
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

// --- Hot paths commands ---

#[tauri::command]
pub async fn get_hot_paths(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, GlobalContext>,
) -> Result<Vec<newt_common::hot_paths::HotPathEntry>, Error> {
    use newt_common::hot_paths::{HotPathCategory, HotPathEntry};
    use newt_common::vfs::{VfsId, VfsPath};

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
            path: VfsPath {
                vfs_id: VfsId::ROOT,
                path: bm.path.clone().into(),
            },
            name: bm.name.clone(),
            category: HotPathCategory::UserBookmark,
        });
    }

    Ok(entries)
}

#[tauri::command]
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
pub fn cmd_add_bookmark(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<GlobalContext> = app_handle.state();

    let pane = ctx.panes().get(pane_handle).unwrap();
    let path = pane.path();

    let path_str = ctx.format_vfs_path(&path);
    let name = path
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string());

    global_ctx
        .preferences()
        .add_bookmark(&path_str, name.as_deref())
        .map_err(Error::Custom)
}

// --- cmd_* commands ---
// These are commands triggerable from the command palette and keyboard shortcuts.
// The `cmd_` prefix is intercepted by the middleware in `create_handler` which
// closes the current modal before forwarding to the actual handler.

// Dialog-opening commands — each calls `dialog()` which sets the modal.
macro_rules! cmd_dialog {
    ($name:ident, $dialog:expr) => {
        #[tauri::command]
        pub fn $name(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
            dialog(ctx, $dialog.to_string(), Some(pane_handle))
        }
    };
}

cmd_dialog!(cmd_rename, "rename");
cmd_dialog!(cmd_properties, "properties");
cmd_dialog!(cmd_create_directory, "create_directory");
cmd_dialog!(cmd_create_file, "create_file");
cmd_dialog!(cmd_create_and_edit, "create_and_edit");
cmd_dialog!(cmd_navigate, "navigate");
cmd_dialog!(cmd_copy, "copy");
cmd_dialog!(cmd_move, "move");
cmd_dialog!(cmd_connect_remote, "connect_remote");
cmd_dialog!(cmd_select_vfs, "select_vfs");
cmd_dialog!(cmd_mount_sftp, "mount_sftp");
cmd_dialog!(cmd_command_palette, "command_palette");
cmd_dialog!(cmd_user_commands, "user_commands");
cmd_dialog!(cmd_hot_paths, "hot_paths");
cmd_dialog!(cmd_open_settings, "settings");

#[tauri::command]
pub fn cmd_close_window(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.window().close()?;
    Ok(())
}

#[tauri::command]
pub fn cmd_open_config_file(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    let app_handle = ctx.window().app_handle().clone();
    let global_ctx: tauri::State<GlobalContext> = app_handle.state();
    open_config_file(global_ctx)
}

#[tauri::command]
pub fn cmd_reload_window(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    let _ = ctx.window().eval("window.location.reload()");
    Ok(())
}

pub fn create_handler() -> Box<dyn Fn(Invoke<Wry>) -> bool + Send + Sync + 'static> {
    let inner: Box<dyn Fn(Invoke<Wry>) -> bool + Send + Sync> = Box::new(tauri::generate_handler![
        // Core / lifecycle
        init,
        askpass_respond,
        ping,
        close_modal,
        confirm_action,
        dialog,
        close_window,
        destroy_window,
        set_window_title,
        zoom,
        // Pane interaction (called directly by frontend components)
        cancel,
        navigate,
        focus,
        set_sorting,
        toggle_selected,
        select_range,
        set_selection,
        relative_jump,
        set_filter,
        // File operations (called from dialog submissions)
        create_directory,
        touch_file,
        rename,
        set_permissions,
        start_operation,
        start_copy_move,
        cancel_operation,
        resolve_issue,
        dismiss_operation,
        background_operation,
        // File viewing/opening/editing
        file_details,
        read_file_range,
        read_file,
        write_file,
        // Viewer / Editor
        crate::viewer::set_viewer_mode,
        crate::viewer::ping_viewer,
        crate::editor::set_editor_language,
        crate::editor::set_editor_wrap,
        crate::editor::ping_editor,
        connect_remote,
        switch_vfs,
        // Terminal
        terminal_write,
        terminal_resize,
        terminal_focus,
        close_terminal,
        activate_terminal,
        // Drag & drop
        start_dnd,
        cancel_dnd,
        execute_dnd,
        // Preferences
        get_preferences,
        update_preference,
        get_preferences_schema,
        open_config_file,
        // Hot paths
        get_hot_paths,
        add_bookmark,
        remove_bookmark,
        // User commands
        crate::user_commands::run_user_command,
        crate::user_commands::execute_user_command,
        crate::user_commands::add_user_command_entry,
        crate::user_commands::remove_user_command_entry,
        crate::user_commands::update_user_command_entry,
        // cmd_* commands (palette / keyboard shortcut entry points)
        cmd_rename,
        cmd_properties,
        cmd_create_directory,
        cmd_create_file,
        cmd_create_and_edit,
        cmd_navigate,
        cmd_copy,
        cmd_move,
        cmd_connect_remote,
        cmd_select_vfs,
        cmd_command_palette,
        cmd_user_commands,
        cmd_open_settings,
        cmd_new_window,
        cmd_toggle_hidden,
        cmd_close_window,
        cmd_view,
        cmd_edit,
        cmd_open,
        cmd_open_folder,
        cmd_follow_symlink,
        cmd_navigate_back,
        cmd_navigate_forward,
        cmd_copy_pane,
        cmd_select_all,
        cmd_deselect_all,
        cmd_copy_to_clipboard,
        cmd_paste_from_clipboard,
        cmd_send_to_terminal,
        cmd_toggle_terminal_panel,
        cmd_focus_panes,
        cmd_focus_terminal,
        cmd_create_terminal,
        cmd_next_terminal,
        cmd_prev_terminal,
        cmd_open_elevated,
        cmd_mount_s3,
        cmd_mount_sftp,
        mount_sftp,
        cmd_hot_paths,
        cmd_add_bookmark,
        cmd_open_config_file,
        cmd_reload_window,
        cmd_delete_selected,
    ]);

    // Middleware: close the current modal before any cmd_* command runs.
    Box::new(move |invoke: Invoke<Wry>| {
        if invoke.message.command().starts_with("cmd_") {
            let webview = invoke.message.webview();
            let app_handle = webview.app_handle().clone();
            let global_ctx: tauri::State<GlobalContext> = app_handle.state();
            if let Some(mwc) = global_ctx.main_window(&webview) {
                let _ = mwc.with_update(|gs| {
                    gs.close_modal();
                    Ok(())
                });
            }
        }
        inner(invoke)
    })
}
