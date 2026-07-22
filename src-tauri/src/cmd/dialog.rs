use newt_common::vfs::VfsPath;
use tauri::Manager;

use super::{MODE_MASK, usergroup_id};
use crate::common::Error;
use crate::main_window::pane::FilterMode;
use crate::main_window::{
    MainWindowContext, ModalContext, ModalData, ModalDataKind, PaneHandle, PropertySheetState,
};

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
    /// Properties of the volume root containing the pane's current path
    /// (opened by clicking the free-space label in the pane header).
    RootProperties,
    Properties,
    Rename,
    Copy,
    Move,
    CreateArchive,
    ConnectRemote,
    MountSftp,
    MountS3,
    Search,
    // specta's snake_case tokenizer splits at digits ("K8s" → "k_8s"), but
    // serde's keeps it joined; pin both ends to the wire format.
    #[serde(rename = "mount_k8s")]
    #[specta(rename = "mount_k8s")]
    MountK8s,
    /// Keyboard-launched quick sort menu, anchored to the pane header.
    Sort,
    /// The connect dialog, but scoped to a pane mount (VFS selector entry).
    MountRemote,
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
    let default_open_in = if ctx.connection_target().is_remote() {
        crate::connections::OpenIn::Pane
    } else {
        crate::connections::OpenIn::Window
    };
    let rt_state = {
        let app_handle = ctx.window().app_handle().clone();
        let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
        global_ctx.runtime_state().state()
    };
    // Set when a Properties modal wants its extended-property sheet
    // fetched after opening (open-then-fill): (modal paths — the write
    // guard, sheet paths — the file entries to actually fetch).
    let mut sheet_fetch: Option<(Vec<VfsPath>, Vec<VfsPath>)> = None;
    // Set when a SelectVfs modal wants per-target free space fetched
    // after opening: (target index, volume path to stat).
    let mut free_space_fetch: Option<Vec<(usize, VfsPath)>> = None;
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

                    let descriptor = ctx
                        .vfs_info()
                        .ok()
                        .and_then(|vi| vi.descriptor(pane_path.vfs_id));
                    let can_set_metadata = descriptor
                        .as_ref()
                        .is_some_and(|(d, _)| d.can_set_metadata());
                    // Directories only carry a sheet where they're real,
                    // stattable objects (not S3-style synthetic prefixes).
                    let sheet = if descriptor.as_ref().is_some_and(|(d, _)| {
                        d.has_extended_properties() && d.can_stat_directories()
                    }) {
                        sheet_fetch = Some((vec![pane_path.clone()], vec![pane_path.clone()]));
                        PropertySheetState::Loading
                    } else {
                        PropertySheetState::Hidden
                    };

                    let name = pane_path
                        .file_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| pane_path.to_string());

                    let mode = dir_entry.and_then(|f| f.mode.as_ref().map(|m| m.0));

                    // Volume details only at the volume's own root (the
                    // stats' mount point where known, else the navigable
                    // root) — the header's free-space label opens
                    // RootProperties for the deep case.
                    let stats = pane.view_state().fs_stats.clone();
                    let at_volume_root = match stats
                        .as_ref()
                        .and_then(|s| s.volume())
                        .and_then(|v| v.mount_point.as_deref())
                    {
                        Some(mp) => {
                            pane_path.path == newt_common::vfs::path::PathBuf::from_wire_str(mp)
                        }
                        None => descriptor.as_ref().is_some_and(|(d, meta)| {
                            d.navigable_parent(&pane_path.path, meta).is_none()
                        }),
                    };
                    let fs_stats = at_volume_root.then_some(stats).flatten();

                    ModalDataKind::Properties {
                        paths: vec![pane_path],
                        can_set_metadata,
                        name,
                        size: dir_entry.and_then(|f| f.size),
                        allocated_size: dir_entry.and_then(|f| f.allocated_size),
                        hard_links: dir_entry.and_then(|f| f.hard_links),
                        inode: dir_entry.and_then(|f| f.inode),
                        device_id: dir_entry.and_then(|f| f.device_id),
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
                        sheet,
                        fs_stats,
                    }
                }
                DialogKind::RootProperties => {
                    let pane = pane.unwrap();
                    let pane_path = pane.path();
                    let fs_stats = pane.view_state().fs_stats.clone();

                    // The volume root the stats actually describe (the
                    // statvfs mount point — `/proc`, not `/`). Fall back
                    // to walking up to the navigable root for stats
                    // without one.
                    let root = match fs_stats
                        .as_ref()
                        .and_then(|s| s.volume())
                        .and_then(|v| v.mount_point.as_deref())
                    {
                        Some(mp) => newt_common::vfs::path::PathBuf::from_wire_str(mp),
                        None => {
                            let Some((descriptor, mount_meta)) = ctx
                                .vfs_info()
                                .ok()
                                .and_then(|vi| vi.descriptor(pane_path.vfs_id))
                            else {
                                return Ok(());
                            };
                            let mut root = pane_path.path.clone();
                            while let Some(parent) = descriptor.navigable_parent(&root, &mount_meta)
                            {
                                root = parent;
                            }
                            root
                        }
                    };
                    let root_path = VfsPath::new(pane_path.vfs_id, root);
                    let name = ctx.format_vfs_path(&root_path);

                    // The pane's listing stats describe this same volume;
                    // no stat of the root itself, so the file rows
                    // (mode/owner/times) stay hidden.
                    ModalDataKind::Properties {
                        paths: vec![root_path],
                        can_set_metadata: false,
                        name,
                        size: None,
                        allocated_size: None,
                        hard_links: None,
                        inode: None,
                        device_id: None,
                        is_dir: true,
                        is_symlink: false,
                        symlink_target: None,
                        mode_set: 0,
                        mode_clear: 0,
                        has_mode: false,
                        owner: None,
                        group: None,
                        owner_id: None,
                        group_id: None,
                        modified: None,
                        accessed: None,
                        created: None,
                        sheet: PropertySheetState::Hidden,
                        fs_stats,
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

                    // Sheet capability follows the *dereferenced* paths'
                    // VFS — a search-result entry gets its source VFS's
                    // sheet, not the search VFS's.
                    let sheet_descriptor = ctx
                        .vfs_info()
                        .ok()
                        .and_then(|vi| vi.descriptor(paths[0].vfs_id));

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

                    let allocated_size = if files.iter().all(|f| f.allocated_size.is_some()) {
                        Some(files.iter().map(|f| f.allocated_size.unwrap_or(0)).sum())
                    } else {
                        None
                    };

                    let (hard_links, inode, device_id) = if files.len() == 1 {
                        (files[0].hard_links, files[0].inode, files[0].device_id)
                    } else {
                        (None, None, None)
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

                    // Fetch sheets from file entries only where directories
                    // are synthetic (S3 prefixes — nothing to head); the
                    // apply still targets the whole selection, so recursive
                    // prefix apply keeps working.
                    let sheet = match sheet_descriptor {
                        Some((d, _)) if d.has_extended_properties() => {
                            let sheet_paths: Vec<VfsPath> = if d.can_stat_directories() {
                                paths.clone()
                            } else {
                                display_paths
                                    .iter()
                                    .zip(paths.iter())
                                    .filter_map(|(dp, p)| {
                                        let key = dp.file_name()?;
                                        let f = view_files.iter().find(|f| f.key() == key)?;
                                        (!f.is_dir).then(|| p.clone())
                                    })
                                    .collect()
                            };
                            if sheet_paths.is_empty() {
                                PropertySheetState::Hidden
                            } else {
                                sheet_fetch = Some((paths.clone(), sheet_paths));
                                PropertySheetState::Loading
                            }
                        }
                        _ => PropertySheetState::Hidden,
                    };

                    ModalDataKind::Properties {
                        paths,
                        can_set_metadata,
                        name,
                        size,
                        allocated_size,
                        hard_links,
                        inode,
                        device_id,
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
                        sheet,
                        fs_stats: None,
                    }
                }
                DialogKind::Rename => {
                    let pane = pane.unwrap();
                    // Focused rather than the selection — F2 renames what's
                    // under the cursor. `actionable_focus` so that `..` opens
                    // no dialog at all.
                    let name = match pane.view_state().actionable_focus() {
                        Some(name) => name.clone(),
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
                    let default_name = if sources.len() == 1 {
                        sources[0].file_name().map(str::to_string)
                    } else {
                        None
                    };
                    ModalDataKind::CopyMove {
                        default_name,
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
                        defaults: rt_state.copy_move.clone(),
                    }
                }
                DialogKind::CreateArchive => {
                    let pane = pane.unwrap();
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
                    // Single selection → its stem; multiple → the directory
                    // they sit in.
                    let default_name = if sources.len() == 1 {
                        sources[0]
                            .file_name()
                            .map(|name| match name.rsplit_once('.') {
                                Some((stem, _)) if !stem.is_empty() => stem.to_string(),
                                _ => name.to_string(),
                            })
                            .unwrap_or_default()
                    } else {
                        sources[0]
                            .parent()
                            .and_then(|p| p.file_name().map(str::to_string))
                            .unwrap_or_else(|| "archive".to_string())
                    };
                    let settings = ctx.preferences().load();
                    let prefs = &settings.archives;
                    ModalDataKind::CreateArchive {
                        sources,
                        destination,
                        display_destination,
                        summary,
                        default_name,
                        defaults: crate::main_window::ArchiveDialogDefaults {
                            format: prefs.default_format.into(),
                            preserve_symlinks: prefs.preserve_symlinks,
                            zip_level: prefs.zip_level,
                            gzip_level: prefs.gzip_level,
                            xz_level: prefs.xz_level,
                            zstd_level: prefs.zstd_level,
                        },
                    }
                }
                DialogKind::ConnectRemote | DialogKind::MountRemote => {
                    ModalDataKind::ConnectRemote {
                        initial: crate::connections::ConnectionKind::Ssh {
                            host: String::new(),
                            forward_agent: false,
                            login_shell: true,
                        },
                        // MountRemote comes from the VFS selector, whose other
                        // entries all mount into the pane — default to that
                        // scope regardless of session.
                        default_open_in: if dialog == DialogKind::MountRemote {
                            crate::connections::OpenIn::Pane
                        } else {
                            default_open_in
                        },
                    }
                }
                DialogKind::MountSftp => ModalDataKind::MountSftp {
                    host: String::new(),
                },
                DialogKind::MountS3 => ModalDataKind::MountS3,
                DialogKind::Search => {
                    let pane = pane.unwrap();
                    let path = pane.path();
                    // cmd+f inside a search refines it: re-root the dialog
                    // at the original search root and prefill its params.
                    let refine = ctx.vfs_info().ok().and_then(|vi| {
                        let (desc, meta) = vi.descriptor(path.vfs_id)?;
                        let params = desc.search_params(&meta)?;
                        Some((vi.origin(path.vfs_id)?, params))
                    });
                    let (path, prefill) = match refine {
                        Some((root, params)) => (root, Some(params)),
                        None => (path, None),
                    };
                    let display_path = ctx.format_vfs_path(&path);
                    ModalDataKind::Search {
                        path,
                        display_path,
                        prefill,
                        defaults: rt_state.search.clone(),
                    }
                }
                DialogKind::MountK8s => ModalDataKind::MountK8s {
                    k8s_context: String::new(),
                },
                DialogKind::Sort => {
                    let pane = pane.unwrap();
                    ModalDataKind::SortMenu {
                        sorting: pane.view_state().sorting.clone(),
                        folders_first: ctx.preferences().load().appearance.folders_first,
                    }
                }
                DialogKind::QuickConnect => {
                    let app_handle = ctx.window().app_handle().clone();
                    let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
                    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
                    let connections = crate::connections::list_connections(&config_dir);
                    // Hide any recent that now matches a saved profile — the
                    // profile is already one keystroke away in this palette.
                    let saved: std::collections::HashSet<String> =
                        connections.iter().map(|c| c.kind.identity()).collect();
                    let recent_connections = rt_state
                        .recent_connections
                        .iter()
                        .filter(|r| !saved.contains(&r.kind.identity()))
                        .cloned()
                        .collect();
                    ModalDataKind::QuickConnect {
                        connections,
                        recent_connections,
                    }
                }
                DialogKind::SelectVfs => {
                    let targets = ctx.compute_vfs_targets()?;
                    // Free space is filled in after opening — see
                    // `spawn_free_space_fetch`.
                    free_space_fetch = Some(
                        targets
                            .iter()
                            .enumerate()
                            .filter_map(|(i, t)| {
                                let vfs_id = t.vfs_id?;
                                let path = match &t.root {
                                    Some(root) => root.clone(),
                                    None => {
                                        // Unified-root mounts: only where
                                        // the VFS reports stats at all
                                        // (skips pointless RPCs on S3 & co).
                                        let (d, _) = ctx.vfs_info().ok()?.descriptor(vfs_id)?;
                                        if !d.can_fs_stats() {
                                            return None;
                                        }
                                        newt_common::vfs::path::PathBuf::root()
                                    }
                                };
                                Some((i, VfsPath::new(vfs_id, path)))
                            })
                            .collect(),
                    );
                    ModalDataKind::SelectVfs { targets }
                }
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
    })?;

    if let Some((modal_paths, sheet_paths)) = sheet_fetch {
        spawn_property_sheet_fetch(&ctx, modal_paths, sheet_paths);
    }
    if let Some(queries) = free_space_fetch {
        spawn_free_space_fetch(&ctx, queries);
    }

    Ok(())
}

/// Fill per-target free space in an open SelectVfs modal (open-then-fill,
/// one patch per result as it lands — a dead network drive delays only its
/// own entry, never the dropdown). Each write is guarded on the modal
/// still being a SelectVfs whose target at that index matches the queried
/// volume.
fn spawn_free_space_fetch(ctx: &MainWindowContext, queries: Vec<(usize, VfsPath)>) {
    let ctx = ctx.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(fs) = ctx.fs() else { return };
        let mut join_set = tokio::task::JoinSet::new();
        for (idx, path) in queries {
            let fs = fs.clone();
            join_set.spawn(async move {
                let stats = fs.fs_stats(path.clone()).await;
                (idx, path, stats)
            });
        }
        while let Some(res) = join_set.join_next().await {
            let Ok((idx, path, Ok(Some(stats)))) = res else {
                continue;
            };
            let _ = ctx.with_update(|gs| {
                let mut modal = gs.modal.0.write();
                if let Some(ModalData {
                    kind: ModalDataKind::SelectVfs { targets },
                    ..
                }) = modal.as_mut()
                    && let Some(target) = targets.get_mut(idx)
                    && target.vfs_id == Some(path.vfs_id)
                    && target.root.as_ref().map_or_else(
                        || path.path == newt_common::vfs::path::PathBuf::root(),
                        |r| *r == path.path,
                    )
                {
                    target.available_bytes = Some(stats.available_bytes());
                }
                Ok(())
            });
        }
    });
}

/// Fetch per-file property sheets for an open Properties modal, fold
/// them, and patch the modal state (open-then-fill). The write is
/// guarded on the modal still being the same Properties instance — the
/// user may have closed it (or opened another) while the fetch ran.
fn spawn_property_sheet_fetch(
    ctx: &MainWindowContext,
    modal_paths: Vec<VfsPath>,
    sheet_paths: Vec<VfsPath>,
) {
    let ctx = ctx.clone();
    tauri::async_runtime::spawn(async move {
        let state = match fetch_folded_sheet(&ctx, &sheet_paths).await {
            Ok(sheet) => PropertySheetState::Loaded { sheet },
            Err(e) => PropertySheetState::Failed {
                error: e.to_string(),
            },
        };
        let _ = ctx.with_update(|gs| {
            let mut modal = gs.modal.0.write();
            if let Some(ModalData {
                kind:
                    ModalDataKind::Properties {
                        paths: current_paths,
                        sheet,
                        ..
                    },
                ..
            }) = modal.as_mut()
                && *current_paths == modal_paths
            {
                *sheet = state;
            }
            Ok(())
        });
    });
}

async fn fetch_folded_sheet(
    ctx: &MainWindowContext,
    paths: &[VfsPath],
) -> Result<newt_common::vfs::PropertySheet, Error> {
    const CONCURRENCY: usize = 8;

    let reader = ctx.file_reader()?;
    let mut iter = paths.iter().cloned();
    let mut join_set = tokio::task::JoinSet::new();
    for path in iter.by_ref().take(CONCURRENCY) {
        let reader = reader.clone();
        join_set.spawn(async move { reader.get_property_sheet(path).await });
    }

    let mut sheets = Vec::with_capacity(paths.len());
    while let Some(res) = join_set.join_next().await {
        sheets.push(res.map_err(|e| Error::Custom(e.to_string()))??);
        if let Some(path) = iter.next() {
            let reader = reader.clone();
            join_set.spawn(async move { reader.get_property_sheet(path).await });
        }
    }

    Ok(newt_common::vfs::fold_sheets(&sheets))
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
cmd_dialog!(cmd_create_archive, DialogKind::CreateArchive);
cmd_dialog!(cmd_connect_remote, DialogKind::ConnectRemote);
cmd_dialog!(cmd_select_vfs, DialogKind::SelectVfs);
cmd_dialog!(cmd_history_back, DialogKind::HistoryBack);
cmd_dialog!(cmd_history_forward, DialogKind::HistoryForward);
cmd_dialog!(cmd_history, DialogKind::History);
cmd_dialog!(cmd_quick_connect, DialogKind::QuickConnect);
cmd_dialog!(cmd_mount_s3, DialogKind::MountS3);
cmd_dialog!(cmd_mount_sftp, DialogKind::MountSftp);
cmd_dialog!(cmd_mount_k8s, DialogKind::MountK8s);
cmd_dialog!(cmd_sort, DialogKind::Sort);
/// cmd+f. Unlike the other dialog shims this one isn't built with
/// `cmd_dialog!`: on a search VFS the dialog reopens pre-filled to refine
/// the current search (`VfsDescriptor::search_params`); on any other VFS
/// that opts out of recursive search (`VfsDescriptor::can_search`), we
/// transparently fall back to opening the in-pane quick filter — the
/// same effect as pressing `/`.
#[tauri::command]
#[specta::specta]
pub fn cmd_start_search(ctx: MainWindowContext, pane_handle: PaneHandle) -> Result<(), Error> {
    let supports_search = ctx.with_pane_update(pane_handle, |_, pane| {
        Ok(ctx
            .vfs_info()
            .ok()
            .and_then(|vi| vi.descriptor(pane.path().vfs_id))
            .is_none_or(|(d, m)| d.can_search() || d.search_params(&m).is_some()))
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
