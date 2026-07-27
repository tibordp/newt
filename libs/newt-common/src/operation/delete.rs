use super::*;

// --- Execute Delete (async outer loop, uses Vfs) ---

/// Whether a path is a directory *to descend into*.
///
/// A symlink or Windows junction pointing at a directory is deliberately
/// **not** one. `File::is_dir` says otherwise — `file_info` reports the
/// target's type for a link so panes can enter it — but every caller here
/// is about to walk the path and act on what it finds, and following the
/// link would delete or chmod the target's contents instead of the link.
/// The parent-listing branch has always excluded links; the stat branch
/// must match it.
///
/// When `can_stat_directories` is true (most VFSes), uses `file_info` directly.
/// When false (e.g. S3), falls back to listing the parent directory.
pub(super) async fn probe_is_dir(
    vfs: &dyn Vfs,
    descriptor: &dyn VfsDescriptor,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<bool, crate::Error> {
    if descriptor.can_stat_directories() {
        let file = vfs.file_info(path).await?;
        return Ok(file.is_dir && !file.is_symlink);
    }

    let root = PathBuf::root();
    let parent = path.parent().unwrap_or(&root);
    let file_name = path.file_name();
    match file_name {
        Some(name) => {
            let listing = cancellable(cancel, vfs.list_files(parent, None)).await?;
            Ok(listing
                .files
                .iter()
                .find(|f| f.name == name)
                .is_some_and(|f| f.is_dir && !f.is_symlink))
        }
        None => Ok(true), // root-level path, treat as directory
    }
}

/// Walk a directory tree depth-first and collect all entries for deletion.
/// Returns entries in deletion order: files first, then directories (deepest first).
pub(super) struct DeleteEntry {
    path: PathBuf,
    is_dir: bool,
}

pub(super) async fn collect_delete_entries(
    vfs: &dyn Vfs,
    path: &Path,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<DeleteEntry>, crate::Error> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
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
            if file.is_dir && !file.is_symlink {
                stack.push(entry_path.clone());
                dirs.push(DeleteEntry {
                    path: entry_path,
                    is_dir: true,
                });
            } else {
                files.push(DeleteEntry {
                    path: entry_path,
                    is_dir: false,
                });
            }
        }
    }

    // Files first, then directories in reverse order (deepest first)
    dirs.reverse();
    files.extend(dirs);
    Ok(files)
}

/// Walk a directory tree and collect every entry (root included) as
/// `(path, is_dir)`, for per-item recursive apply (chmod, properties).
/// Flattened delete entry with the VFS it belongs to.
pub(super) struct ResolvedDeleteEntry {
    vfs: Arc<dyn Vfs>,
    path: PathBuf,
    is_dir: bool,
    /// Whether to use atomic remove_tree (skips per-item walk).
    use_remove_tree: bool,
}

pub(super) async fn execute_delete(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!("execute_delete: {} paths", paths.len());

    // Follow redirect_target so deletes from a SearchVfs hit the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

    // Phase 1: Scan — collect all entries into a flat list so we know the
    // real total before we start deleting.
    let mut all_entries: Vec<ResolvedDeleteEntry> = Vec::new();

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;
        let descriptor = vfs.descriptor();

        if descriptor.can_remove_tree() {
            // Fast path: single atomic removal, counts as 1 item.
            all_entries.push(ResolvedDeleteEntry {
                vfs,
                path: local_path,
                is_dir: true,
                use_remove_tree: true,
            });
        } else {
            let is_dir = probe_is_dir(&*vfs, descriptor, &local_path, &cancel).await?;
            if is_dir {
                let children =
                    collect_delete_entries(&*vfs, &local_path, reporter, &cancel).await?;
                for entry in children {
                    all_entries.push(ResolvedDeleteEntry {
                        vfs: vfs.clone(),
                        path: entry.path,
                        is_dir: entry.is_dir,
                        use_remove_tree: false,
                    });
                }
                // The top-level directory itself (removed last)
                all_entries.push(ResolvedDeleteEntry {
                    vfs,
                    path: local_path,
                    is_dir: true,
                    use_remove_tree: false,
                });
            } else {
                all_entries.push(ResolvedDeleteEntry {
                    vfs,
                    path: local_path,
                    is_dir: false,
                    use_remove_tree: false,
                });
            }
        }

        reporter.maybe_send_scanning(all_entries.len() as u64, 0);
    }

    // Phase 2: Execute
    let total_items = all_entries.len() as u64;
    reporter.send_prepared(0, total_items);

    let mut items_done = 0u64;

    for entry in &all_entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let display = entry.path.to_string();
        reporter.maybe_send_progress(0, items_done, &display);

        let mut retry = true;
        while retry {
            retry = false;

            let result = if entry.use_remove_tree {
                entry.vfs.remove_tree(&entry.path).await
            } else if entry.is_dir {
                entry.vfs.remove_dir(&entry.path).await
            } else {
                entry.vfs.remove_file(&entry.path).await
            };

            if let Err(e) = result {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error deleting {}", entry.path),
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

pub(super) async fn execute_trash(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    debug!("execute_trash: {} paths", paths.len());

    // Follow redirect_target so trashing from a SearchVfs hits the real files.
    let mut paths = paths;
    for p in paths.iter_mut() {
        *p = context.registry.dereference(p).await;
    }

    // No scan phase: each top-level item is trashed wholesale and counts
    // as one item, like the remove_tree fast path.
    reporter.send_prepared(0, paths.len() as u64);

    let mut items_done = 0u64;

    for vfs_path in &paths {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;

        let display = local_path.to_string();
        reporter.maybe_send_progress(0, items_done, &display);

        let mut retry = true;
        while retry {
            retry = false;

            let result = if vfs.descriptor().can_trash() {
                vfs.trash_item(&local_path).await
            } else {
                Err(crate::Error::not_supported())
            };

            if let Err(e) = result {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Error moving {} to Trash", local_path),
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
