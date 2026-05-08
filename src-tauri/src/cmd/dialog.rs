use tauri::Manager;

use super::{MODE_MASK, usergroup_id};
use crate::common::Error;
use crate::main_window::{MainWindowContext, ModalContext, ModalData, ModalDataKind, PaneHandle};

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
                "directory_properties" => {
                    let pane = pane.unwrap();
                    let pane_path = pane.path();
                    let file_list = pane.file_list();
                    let dir_entry = file_list.files().iter().find(|f| f.name == "..");

                    let can_set_metadata = ctx
                        .vfs_info()
                        .ok()
                        .and_then(|vi| vi.descriptor(pane_path.vfs_id))
                        .is_some_and(|(d, _)| d.can_set_metadata());

                    let name = pane_path
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| pane_path.path.to_string_lossy().to_string());

                    let mode = dir_entry.and_then(|f| f.mode.as_ref().map(|m| m.0));

                    ModalDataKind::Properties {
                        paths: vec![pane_path],
                        can_set_metadata,
                        name,
                        size: dir_entry.and_then(|f| f.size),
                        is_dir: true,
                        is_symlink: false,
                        symlink_target: None,
                        mode_set: mode.unwrap_or(0),
                        mode_clear: mode.map(|m| !m & MODE_MASK).unwrap_or(0),
                        has_mode: mode.is_some(),
                        owner: dir_entry.and_then(|f| f.user.clone()),
                        group: dir_entry.and_then(|f| f.group.clone()),
                        owner_id: dir_entry.and_then(|f| usergroup_id(f.user.as_ref())),
                        group_id: dir_entry.and_then(|f| usergroup_id(f.group.as_ref())),
                        modified: dir_entry.and_then(|f| f.modified),
                        accessed: dir_entry.and_then(|f| f.accessed),
                        created: dir_entry.and_then(|f| f.created),
                    }
                }
                "properties" => {
                    let pane = pane.unwrap();
                    let paths = pane.get_effective_selection();
                    if paths.is_empty() {
                        return Ok(());
                    }

                    let can_set_metadata = ctx
                        .vfs_info()
                        .ok()
                        .and_then(|vi| vi.descriptor(pane.path().vfs_id))
                        .is_some_and(|(d, _)| d.can_set_metadata());

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

                    // Tri-state mode: mode_set = bits ON in ALL files,
                    // mode_clear = bits OFF in ALL files.
                    // Bits in neither are indeterminate (mixed).
                    let has_mode = files.iter().any(|f| f.mode.is_some());
                    let (mode_set, mode_clear) = if has_mode {
                        let all_set = files.iter().fold(MODE_MASK, |acc, f| {
                            acc & f.mode.as_ref().map(|m| m.0).unwrap_or(0)
                        });
                        let all_clear = files.iter().fold(MODE_MASK, |acc, f| {
                            acc & !f.mode.as_ref().map(|m| m.0).unwrap_or(0) & MODE_MASK
                        });
                        (all_set, all_clear)
                    } else {
                        (0, 0)
                    };

                    // Owner/group: show only if identical across all files
                    let owner = if let Some(first) = files[0].user.as_ref() {
                        if files.iter().all(|f| f.user.as_ref() == Some(first)) {
                            Some(first.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let group = if let Some(first) = files[0].group.as_ref() {
                        if files.iter().all(|f| f.group.as_ref() == Some(first)) {
                            Some(first.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let owner_id = owner.as_ref().and_then(|u| usergroup_id(Some(u)));
                    let group_id = group.as_ref().and_then(|g| usergroup_id(Some(g)));

                    // Timestamps: only for single file
                    let (modified, accessed, created) = if files.len() == 1 {
                        (files[0].modified, files[0].accessed, files[0].created)
                    } else {
                        (None, None, None)
                    };

                    ModalDataKind::Properties {
                        paths,
                        can_set_metadata,
                        name,
                        size,
                        is_dir,
                        is_symlink,
                        symlink_target,
                        mode_set,
                        mode_clear,
                        has_mode,
                        owner,
                        group,
                        owner_id,
                        group_id,
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
                "mount_s3" => ModalDataKind::MountS3,
                "mount_k8s" => ModalDataKind::MountK8s {
                    k8s_context: String::new(),
                },
                "quick_connect" => {
                    let app_handle = ctx.window().app_handle().clone();
                    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
                    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
                    ModalDataKind::QuickConnect {
                        connections: crate::connections::list_connections(&config_dir),
                    }
                }
                "select_vfs" => ModalDataKind::SelectVfs {
                    targets: ctx.compute_vfs_targets()?,
                },
                "history_back" | "history_forward" => {
                    let pane = pane.unwrap();
                    let (entries, current_index) = pane.history_entries();
                    // The list is ordered forward-on-top, back-on-bottom (see
                    // Pane::history_entries). So alt+left (back) steps DOWN
                    // (+1 list index) and alt+right (forward) steps UP (-1).
                    ModalDataKind::HistoryNavigator {
                        entries,
                        current_index,
                        initial_direction: if dialog == "history_back" { 1 } else { -1 },
                    }
                }
                "command_palette" => ModalDataKind::CommandPalette {
                    category_filter: None,
                },
                "user_commands" => ModalDataKind::CommandPalette {
                    category_filter: Some("User".to_string()),
                },
                "hot_paths" => ModalDataKind::HotPaths,
                "settings" => ModalDataKind::Settings,
                "debug" => {
                    if !cfg!(debug_assertions) {
                        return Err(Error::Custom(
                            "debug dialog is only available in debug builds".into(),
                        ));
                    }
                    ModalDataKind::Debug
                }
                "connection_log" => ModalDataKind::ConnectionLog,
                "about" => ModalDataKind::About {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    git_revision: option_env!("NEWT_GIT_REVISION").map(|s| s.to_string()),
                    build_date: option_env!("NEWT_BUILD_DATE").map(|s| s.to_string()),
                    target_triple: env!("NEWT_TARGET_TRIPLE").to_string(),
                },
                _ => return Err(Error::Custom(format!("unknown dialog: {}", dialog))),
            },
            context: ModalContext { pane_handle },
        });

        Ok(())
    })
}

// ---------------------------------------------------------------------------
// cmd_* dialog-opening commands
//
// Each is a thin shim that calls `dialog()` with a hard-coded dialog name.
// They exist so that the `cmd_` prefix is visible to the middleware in
// `create_handler`, which closes the current modal before forwarding.
// ---------------------------------------------------------------------------

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
cmd_dialog!(cmd_directory_properties, "directory_properties");
cmd_dialog!(cmd_create_directory, "create_directory");
cmd_dialog!(cmd_create_file, "create_file");
cmd_dialog!(cmd_create_and_edit, "create_and_edit");
cmd_dialog!(cmd_navigate, "navigate");
cmd_dialog!(cmd_copy, "copy");
cmd_dialog!(cmd_move, "move");
cmd_dialog!(cmd_connect_remote, "connect_remote");
cmd_dialog!(cmd_select_vfs, "select_vfs");
cmd_dialog!(cmd_history_back, "history_back");
cmd_dialog!(cmd_history_forward, "history_forward");
cmd_dialog!(cmd_quick_connect, "quick_connect");
cmd_dialog!(cmd_mount_s3, "mount_s3");
cmd_dialog!(cmd_mount_sftp, "mount_sftp");
cmd_dialog!(cmd_mount_k8s, "mount_k8s");
cmd_dialog!(cmd_command_palette, "command_palette");
cmd_dialog!(cmd_user_commands, "user_commands");
cmd_dialog!(cmd_hot_paths, "hot_paths");
cmd_dialog!(cmd_open_settings, "settings");
cmd_dialog!(cmd_debug, "debug");
cmd_dialog!(cmd_connection_log, "connection_log");
cmd_dialog!(cmd_about, "about");
