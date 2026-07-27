use super::*;

pub(super) async fn collect_chmod_entries(
    vfs: &dyn Vfs,
    path: &Path,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<(PathBuf, bool)>, crate::Error> {
    let mut entries = vec![(path.to_owned(), true)];
    let mut stack = vec![path.to_owned()];

    while let Some(dir) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let file_list = loop {
            match cancellable(cancel, vfs.list_files(&dir, None)).await {
                Ok(list) => break list,
                Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                Err(e) => {
                    match reporter
                        .handle_io_error(
                            e,
                            &format!("Error scanning directory {}", dir),
                            None,
                            cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => break crate::vfs::VfsFileList::default(),
                        IssueOutcome::Retry => continue,
                    }
                }
            }
        };

        for file in &file_list.files {
            if file.name == ".." {
                continue;
            }
            let entry_path = dir.join(&file.name);
            let is_dir = file.is_dir && !file.is_symlink;
            if is_dir {
                stack.push(entry_path.clone());
            }
            entries.push((entry_path, is_dir));
        }
    }

    Ok(entries)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_set_metadata(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    mode_set: u32,
    mode_clear: u32,
    uid: Option<u32>,
    gid: Option<u32>,
    recursive: bool,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!(
        "execute_set_metadata: {} paths, mode_set={:o}, mode_clear={:o}, uid={:?}, gid={:?}, recursive={}",
        paths.len(),
        mode_set,
        mode_clear,
        uid,
        gid,
        recursive
    );

    // Follow redirect_target so chmod from a SearchVfs hits the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

    let mut all_entries: Vec<(Arc<dyn Vfs>, PathBuf, String)> = Vec::new();

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;
        let descriptor = vfs.descriptor();

        if recursive {
            let is_dir = probe_is_dir(&*vfs, descriptor, &local_path, &cancel).await?;
            if is_dir {
                let entries = collect_chmod_entries(&*vfs, &local_path, reporter, &cancel).await?;
                for (entry, _) in entries {
                    let display = format!("{}:{}", vfs_path.vfs_id, entry);
                    all_entries.push((vfs.clone(), entry, display));
                }
                continue;
            }
        }

        let display = vfs_path.to_string();
        all_entries.push((vfs, local_path, display));
    }

    let total_items = all_entries.len() as u64;
    reporter.send_prepared(0, total_items);

    let has_mode_changes = mode_set != 0 || mode_clear != 0;

    let mut items_done = 0u64;

    for (vfs, local_path, display) in &all_entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        reporter.maybe_send_progress(0, items_done, display);

        let mut retry = true;
        while retry {
            retry = false;

            let new_permissions = if has_mode_changes {
                match vfs.file_info(local_path).await {
                    Ok(file_info) => {
                        let old_mode = file_info.mode.map(|m| m.0).unwrap_or(0);
                        Some((old_mode | mode_set) & !mode_clear)
                    }
                    Err(e) => {
                        match reporter
                            .handle_io_error(
                                e,
                                &format!("Error setting metadata on {}", display),
                                None,
                                &cancel,
                                true,
                            )
                            .await?
                        {
                            IssueOutcome::Skip => {
                                break;
                            }
                            IssueOutcome::Retry => {
                                retry = true;
                                continue;
                            }
                        }
                    }
                }
            } else {
                None
            };

            let meta = crate::vfs::VfsMetadata {
                permissions: new_permissions,
                uid,
                gid,
                ..Default::default()
            };

            if let Err(e) = vfs.set_metadata(local_path, &meta).await {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error setting metadata on {}", display),
                        None,
                        &cancel,
                        true,
                    )
                    .await?
                {
                    IssueOutcome::Skip => {}
                    IssueOutcome::Retry => {
                        retry = true;
                    }
                }
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(0, items_done, "");
    Ok(())
}

pub(super) async fn execute_apply_properties(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    patch: crate::vfs::PropertyPatch,
    recursive: bool,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!(
        "execute_apply_properties: {} paths, {} ops, recursive={}",
        paths.len(),
        patch.ops.len(),
        recursive
    );

    if patch.is_empty() {
        return Ok(());
    }

    // Follow redirect_target so applies from a SearchVfs hit the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

    let mut all_entries: Vec<(Arc<dyn Vfs>, PathBuf, String)> = Vec::new();

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;
        let descriptor = vfs.descriptor();
        // On VFSes that can't stat directories (S3), listed "directories"
        // are synthetic prefixes, not objects — nothing to apply to.
        let include_dirs = descriptor.can_stat_directories();

        if recursive {
            let is_dir = probe_is_dir(&*vfs, descriptor, &local_path, &cancel).await?;
            if is_dir {
                let entries = collect_chmod_entries(&*vfs, &local_path, reporter, &cancel).await?;
                for (entry, entry_is_dir) in entries {
                    if entry_is_dir && !include_dirs {
                        continue;
                    }
                    let display = format!("{}:{}", vfs_path.vfs_id, entry);
                    all_entries.push((vfs.clone(), entry, display));
                }
                continue;
            }
        }

        let display = vfs_path.to_string();
        all_entries.push((vfs, local_path, display));
    }

    let total_items = all_entries.len() as u64;
    reporter.send_prepared(0, total_items);

    let mut items_done = 0u64;

    for (vfs, local_path, display) in &all_entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        reporter.maybe_send_progress(0, items_done, display);

        let mut retry = true;
        while retry {
            retry = false;

            if let Err(e) = vfs.apply_properties(local_path, &patch).await {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error applying properties to {}", display),
                        None,
                        &cancel,
                        true,
                    )
                    .await?
                {
                    IssueOutcome::Skip => {}
                    IssueOutcome::Retry => {
                        retry = true;
                    }
                }
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(0, items_done, "");
    Ok(())
}
