use super::*;
use crate::vfs::walk::{self, Control, Entry, Failed, Resume, Visitor};

// --- Execute Delete (async outer loop, uses Vfs) ---

/// Dialog wording for the mount-point issue raised by a delete.
const DELETE_MOUNT_POINT_HINT: &str =
    "To delete its contents too, tick \"Descend into mount points\" in the delete dialog.";

pub(super) struct DeleteEntry {
    path: PathBuf,
    is_dir: bool,
}

/// Every entry under `root`, root included, in deletion order: files
/// first, then directories deepest first. A root that is a file or a
/// link is the one entry.
///
/// A mount point cannot be removed while mounted, so it and every
/// directory above it up to the root are left out. Without
/// `cross_mount_points` its contents stay too, and the walk says so.
pub(super) async fn collect_delete_entries(
    vfs: &dyn Vfs,
    root: &Path,
    cross_mount_points: bool,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<DeleteEntry>, crate::Error> {
    let mut collector = DeleteCollector {
        reporter,
        cancel,
        cross_mount_points,
        files: Vec::new(),
        dirs: Vec::new(),
        open: Vec::new(),
        kept: std::collections::HashSet::new(),
    };
    cancellable(
        cancel,
        walk::walk(
            vfs,
            std::slice::from_ref(&root.to_owned()),
            &walk::WalkOptions::default(),
            &mut collector,
        ),
    )
    .await?;
    let mut entries = collector.files;
    entries.extend(collector.dirs);
    Ok(entries)
}

struct DeleteCollector<'a> {
    reporter: &'a mut ProgressReporter,
    cancel: &'a CancellationToken,
    cross_mount_points: bool,
    files: Vec<DeleteEntry>,
    /// Filled on `leave`, so deepest first.
    dirs: Vec<DeleteEntry>,
    /// Directories entered and not yet left: the current entry's ancestry.
    open: Vec<String>,
    /// Directories that must survive: mount points and their ancestors.
    kept: std::collections::HashSet<String>,
}

#[async_trait::async_trait]
impl Visitor for DeleteCollector<'_> {
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, crate::Error> {
        if self.cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        self.reporter
            .maybe_send_scanning((self.files.len() + self.dirs.len()) as u64, 0);
        if !entry.file.is_dir || entry.file.is_symlink {
            self.files.push(DeleteEntry {
                path: entry.path.to_owned(),
                is_dir: false,
            });
            return Ok(Control::Descend);
        }
        let key = entry.path.as_wire_str().to_string();
        if entry.mount_point {
            self.kept.extend(self.open.iter().cloned());
            self.kept.insert(key.clone());
            if !self.cross_mount_points && entry.depth > 0 {
                skip_mount_point(self.reporter, entry.path, DELETE_MOUNT_POINT_HINT).await?;
                return Ok(Control::Skip);
            }
        }
        self.open.push(key);
        Ok(Control::Descend)
    }

    async fn leave(&mut self, path: &Path, _file: &File) -> Result<(), crate::Error> {
        self.open.pop();
        if !self.kept.contains(path.as_wire_str()) {
            self.dirs.push(DeleteEntry {
                path: path.to_owned(),
                is_dir: true,
            });
        }
        Ok(())
    }

    async fn failed(
        &mut self,
        what: Failed<'_>,
        error: crate::Error,
    ) -> Result<Resume, crate::Error> {
        resume_after(self.reporter, what, error, self.cancel).await
    }
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
    cross_mount_points: bool,
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
            let entries =
                collect_delete_entries(&*vfs, &local_path, cross_mount_points, reporter, &cancel)
                    .await?;
            for entry in entries {
                all_entries.push(ResolvedDeleteEntry {
                    vfs: vfs.clone(),
                    path: entry.path,
                    is_dir: entry.is_dir,
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
