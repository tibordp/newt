use tauri::Manager;

use super::{MODE_MASK, usergroup_id};
use crate::common::Error;
use crate::main_window::pane::FilterMode;
use crate::main_window::{MainWindowContext, ModalContext, ModalData, ModalDataKind, PaneHandle};

/// Every dialog the host can open. Serialized as snake_case so the frontend
/// can keep sending string literals like `"navigate"` / `"mount_s3"` over
/// IPC — but a typo on either side now fails to compile rather than producing
/// `Error::Custom("unknown dialog: …")` at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum DialogKind {
    Navigate,
    CreateDirectory,
    CreateFile,
    CreateAndEdit,
    DirectoryProperties,
    Properties,
    Rename,
    Copy,
    Move,
    ConnectRemote,
    MountSftp,
    MountS3,
    Search,
    // specta's snake_case tokenizer splits at digits ("K8s" → "k_8s"), but
    // serde's keeps it joined; pin both ends to the wire format.
    #[serde(rename = "mount_k8s")]
    #[specta(rename = "mount_k8s")]
    MountK8s,
    QuickConnect,
    SelectVfs,
    HistoryBack,
    HistoryForward,
    History,
    CommandPalette,
    UserCommands,
    HotPaths,
    Settings,
    Debug,
    ConnectionLog,
    About,
}

#[tauri::command]
#[specta::specta]
pub fn dialog(
    ctx: MainWindowContext,
    dialog: DialogKind,
    pane_handle: Option<PaneHandle>,
) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let pane = pane_handle.map(|h| gs.panes.get(h).unwrap());
        let mut modal_state = gs.modal.0.write();
        *modal_state = Some(ModalData {
            kind: match dialog {
                DialogKind::Navigate => {
                    let pane = pane.unwrap();
                    let path = pane.path();
                    let display_path = ctx.format_vfs_path(&path);
                    ModalDataKind::Navigate { path, display_path }
                }
                DialogKind::CreateDirectory => ModalDataKind::CreateDirectory {
                    path: pane.unwrap().path(),
                },
                DialogKind::CreateFile => ModalDataKind::CreateFile {
                    path: pane.unwrap().path(),
                    open_editor: false,
                },
                DialogKind::CreateAndEdit => ModalDataKind::CreateFile {
                    path: pane.unwrap().path(),
                    open_editor: true,
                },
                DialogKind::DirectoryProperties => {
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
                        .file_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| pane_path.to_string());

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
                DialogKind::Properties => {
                    let pane = pane.unwrap();
                    // Display the real underlying paths in the properties
                    // dialog; for a SearchVfs entry the user expects to see
                    // where the file actually lives. The op execution path
                    // will redo dereferencing on its own (registry layer).
                    let paths = pane.get_effective_selection_dereferenced();
                    if paths.is_empty() {
                        return Ok(());
                    }
                    // We still need to look up the entries' display info
                    // (size, mode, ...) by their *in-pane* identity — not
                    // the dereferenced paths.
                    let display_paths = pane.get_effective_selection();

                    let can_set_metadata = ctx
                        .vfs_info()
                        .ok()
                        .and_then(|vi| vi.descriptor(pane.path().vfs_id))
                        .is_some_and(|(d, _)| d.can_set_metadata());

                    // Look up entries by *in-pane* identity (key) rather
                    // than basename — flat search results may share names.
                    let view_state = pane.view_state();
                    let view_files = view_state.files();
                    let files: Vec<&newt_common::filesystem::File> = display_paths
                        .iter()
                        .filter_map(|p| {
                            let key = p.file_name()?;
                            view_files.iter().find(|f| f.key() == key)
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
                        files[0].symlink_target.clone()
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
                DialogKind::Rename => {
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
                DialogKind::Copy | DialogKind::Move => {
                    let pane = pane.unwrap();
                    // Op runner derefs at the registry layer too, but
                    // emitting deref'd paths here keeps the confirmation
                    // copy showing where the bytes actually come from.
                    let sources = pane.get_effective_selection_dereferenced();
                    if sources.is_empty() {
                        return Ok(());
                    }
                    let other_pane = gs.other_pane(pane_handle.unwrap());
                    let destination = other_pane.path();
                    let display_destination = ctx.format_vfs_path(&destination);
                    let summary = if sources.len() == 1 {
                        sources[0]
                            .file_name()
                            .map(str::to_string)
                            .unwrap_or_default()
                    } else {
                        format!("{} items", sources.len())
                    };
                    ModalDataKind::CopyMove {
                        // Frontend distinguishes copy/move by this string.
                        kind: match dialog {
                            DialogKind::Copy => "copy".to_string(),
                            DialogKind::Move => "move".to_string(),
                            _ => unreachable!(),
                        },
                        sources,
                        destination,
                        display_destination,
                        summary,
                    }
                }
                DialogKind::ConnectRemote => ModalDataKind::ConnectRemote {
                    initial: crate::connections::ConnectionKind::Ssh {
                        host: String::new(),
                        forward_agent: false,
                    },
                },
                DialogKind::MountSftp => ModalDataKind::MountSftp {
                    host: String::new(),
                },
                DialogKind::MountS3 => ModalDataKind::MountS3,
                DialogKind::Search => {
                    let pane = pane.unwrap();
                    let path = pane.path();
                    let display_path = ctx.format_vfs_path(&path);
                    ModalDataKind::Search { path, display_path }
                }
                DialogKind::MountK8s => ModalDataKind::MountK8s {
                    k8s_context: String::new(),
                },
                DialogKind::QuickConnect => {
                    let app_handle = ctx.window().app_handle().clone();
                    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
                    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
                    ModalDataKind::QuickConnect {
                        connections: crate::connections::list_connections(&config_dir),
                    }
                }
                DialogKind::SelectVfs => ModalDataKind::SelectVfs {
                    targets: ctx.compute_vfs_targets()?,
                },
                DialogKind::HistoryBack | DialogKind::HistoryForward | DialogKind::History => {
                    let pane = pane.unwrap();
                    let (entries, current_index) = pane.history_entries();
                    // The list is ordered forward-on-top, back-on-bottom (see
                    // Pane::history_entries). So alt+left (back) steps DOWN
                    // (+1 list index) and alt+right (forward) steps UP (-1).
                    ModalDataKind::HistoryNavigator {
                        entries,
                        current_index,
                        initial_direction: if dialog == DialogKind::HistoryForward {
                            -1
                        } else {
                            1
                        },
                        persistent: dialog == DialogKind::History,
                    }
                }
                DialogKind::CommandPalette => ModalDataKind::CommandPalette {
                    category_filter: None,
                },
                DialogKind::UserCommands => ModalDataKind::CommandPalette {
                    category_filter: Some("User".to_string()),
                },
                DialogKind::HotPaths => ModalDataKind::HotPaths,
                DialogKind::Settings => ModalDataKind::Settings,
                DialogKind::Debug => {
                    if !cfg!(debug_assertions) {
                        return Err(Error::Custom(
                            "debug dialog is only available in debug builds".into(),
                        ));
                    }
                    ModalDataKind::Debug
                }
                DialogKind::ConnectionLog => ModalDataKind::ConnectionLog,
                DialogKind::About => ModalDataKind::About {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    git_revision: option_env!("NEWT_GIT_REVISION").map(|s| s.to_string()),
                    target_triple: env!("NEWT_TARGET_TRIPLE").to_string(),
                },
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
        #[specta::specta]
        pub fn $name(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
            dialog(ctx, $dialog, Some(pane_handle))
        }
    };
}

cmd_dialog!(cmd_rename, DialogKind::Rename);
cmd_dialog!(cmd_properties, DialogKind::Properties);
cmd_dialog!(cmd_directory_properties, DialogKind::DirectoryProperties);
cmd_dialog!(cmd_create_directory, DialogKind::CreateDirectory);
cmd_dialog!(cmd_create_file, DialogKind::CreateFile);
cmd_dialog!(cmd_create_and_edit, DialogKind::CreateAndEdit);
cmd_dialog!(cmd_navigate, DialogKind::Navigate);
cmd_dialog!(cmd_copy, DialogKind::Copy);
cmd_dialog!(cmd_move, DialogKind::Move);
cmd_dialog!(cmd_connect_remote, DialogKind::ConnectRemote);
cmd_dialog!(cmd_select_vfs, DialogKind::SelectVfs);
cmd_dialog!(cmd_history_back, DialogKind::HistoryBack);
cmd_dialog!(cmd_history_forward, DialogKind::HistoryForward);
cmd_dialog!(cmd_history, DialogKind::History);
cmd_dialog!(cmd_quick_connect, DialogKind::QuickConnect);
cmd_dialog!(cmd_mount_s3, DialogKind::MountS3);
cmd_dialog!(cmd_mount_sftp, DialogKind::MountSftp);
cmd_dialog!(cmd_mount_k8s, DialogKind::MountK8s);
/// cmd+f. Unlike the other dialog shims this one isn't built with
/// `cmd_dialog!`: if the active pane's VFS opts out of recursive search
/// (`VfsDescriptor::can_search`), we transparently fall back to opening
/// the in-pane quick filter — the same effect as pressing `/`.
#[tauri::command]
#[specta::specta]
pub fn cmd_start_search(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let supports_search = ctx.with_pane_update(pane_handle, |_, pane| {
        Ok(ctx
            .vfs_info()
            .ok()
            .and_then(|vi| vi.descriptor(pane.path().vfs_id))
            .is_none_or(|(d, _)| d.can_search()))
    })?;

    if supports_search {
        dialog(ctx, DialogKind::Search, Some(pane_handle))
    } else {
        ctx.with_pane_update(pane_handle, |_, pane| {
            pane.view_state_mut()
                .set_filter_with_mode(Some(String::new()), FilterMode::Filter);
            Ok(())
        })
    }
}
cmd_dialog!(cmd_command_palette, DialogKind::CommandPalette);
cmd_dialog!(cmd_user_commands, DialogKind::UserCommands);
cmd_dialog!(cmd_hot_paths, DialogKind::HotPaths);
cmd_dialog!(cmd_open_settings, DialogKind::Settings);
cmd_dialog!(cmd_debug, DialogKind::Debug);
cmd_dialog!(cmd_connection_log, DialogKind::ConnectionLog);
cmd_dialog!(cmd_about, DialogKind::About);
