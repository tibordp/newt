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
) -> Result<Vec<DeleteEntry>, newt_common::Error> {
    let mut collector = DeleteCollector {
        vfs,
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
    vfs: &'a dyn Vfs,
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
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, newt_common::Error> {
        if self.cancel.is_cancelled() {
            return Err(newt_common::Error::cancelled());
        }
        self.reporter
            .maybe_send_scanning((self.files.len() + self.dirs.len()) as u64, 0);
        if !entry.file.is_dir || entry.file.is_symlink() {
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
                skip_mount_point(self.reporter, self.vfs, entry.path, DELETE_MOUNT_POINT_HINT)
                    .await?;
                return Ok(Control::Skip);
            }
        }
        self.open.push(key);
        Ok(Control::Descend)
    }

    async fn leave(&mut self, path: &Path, _file: &File) -> Result<(), newt_common::Error> {
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
        error: newt_common::Error,
    ) -> Result<Resume, newt_common::Error> {
        resume_after(self.reporter, self.vfs, what, error, self.cancel).await
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
) -> Result<(), newt_common::Error> {
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
            return Err(newt_common::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;
        all_entries.extend(
            resolve_delete_entries(vfs, local_path, cross_mount_points, reporter, &cancel).await?,
        );
        reporter.maybe_send_scanning(all_entries.len() as u64, 0);
    }

    // Phase 2: Execute
    let total_items = all_entries.len() as u64;
    reporter.send_prepared(0, total_items);

    let mut items_done = 0u64;

    for entry in &all_entries {
        if cancel.is_cancelled() {
            return Err(newt_common::Error::cancelled());
        }
        reporter.maybe_send_progress(0, items_done, &shown(&*entry.vfs, &entry.path));
        delete_entry(reporter, entry, &cancel).await?;
        items_done += 1;
    }

    reporter.maybe_send_progress(0, items_done, "");
    Ok(())
}

/// Everything to delete for `root`, in deletion order: the one atomic
/// removal where the filesystem has it, else every entry under it.
async fn resolve_delete_entries(
    vfs: Arc<dyn Vfs>,
    root: PathBuf,
    cross_mount_points: bool,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<ResolvedDeleteEntry>, newt_common::Error> {
    if vfs.descriptor().can_remove_tree() {
        return Ok(vec![ResolvedDeleteEntry {
            vfs,
            path: root,
            is_dir: true,
            use_remove_tree: true,
        }]);
    }
    let entries =
        collect_delete_entries(&*vfs, &root, cross_mount_points, reporter, cancel).await?;
    Ok(entries
        .into_iter()
        .map(|entry| ResolvedDeleteEntry {
            vfs: vfs.clone(),
            path: entry.path,
            is_dir: entry.is_dir,
            use_remove_tree: false,
        })
        .collect())
}

/// Delete one entry, asking what to do when that fails.
async fn delete_entry(
    reporter: &mut ProgressReporter,
    entry: &ResolvedDeleteEntry,
    cancel: &CancellationToken,
) -> Result<(), newt_common::Error> {
    loop {
        let result = if entry.use_remove_tree {
            entry.vfs.remove_tree(&entry.path).await
        } else if entry.is_dir {
            entry.vfs.remove_dir(&entry.path).await
        } else {
            entry.vfs.remove_file(&entry.path).await
        };
        let Err(e) = result else {
            return Ok(());
        };
        // Already gone, which is what deleting it was for: on macOS, deleting
        // a file on a volume without xattr support (exFAT, FAT) takes
        // its `._` AppleDouble companion along, still listed to come next.
        if e.kind == newt_common::ErrorKind::NotFound {
            return Ok(());
        }
        match reporter
            .handle_io_error(
                e,
                &format!("Error deleting {}", shown(&*entry.vfs, &entry.path)),
                None,
                cancel,
                true,
            )
            .await?
        {
            IssueOutcome::Skip => return Ok(()),
            IssueOutcome::Retry => {}
        }
    }
}

pub(super) async fn execute_trash(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    paths: Vec<VfsPath>,
    cross_mount_points: bool,
    cancel: CancellationToken,
) -> Result<(), newt_common::Error> {
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
            return Err(newt_common::Error::cancelled());
        }

        let (vfs, local_path) = context.registry.resolve(vfs_path)?;

        let display = shown(&*vfs, &local_path);
        reporter.maybe_send_progress(0, items_done, &display);

        loop {
            let result = if vfs.descriptor().can_trash() {
                vfs.trash_item(&local_path).await
            } else {
                Err(newt_common::Error::not_supported())
            };
            let Err(e) = result else {
                break;
            };

            // What no trash takes is never deleted unasked.
            if e.kind == newt_common::ErrorKind::NotSupported {
                let action = reporter
                    .raise_issue(
                        IssueKind::NotTrashable,
                        format!("{display} can't be moved to the Trash: {e}"),
                        None,
                        vec![IssueAction::Skip, IssueAction::DeletePermanently],
                    )
                    .await?;
                if action == IssueAction::DeletePermanently {
                    let entries = resolve_delete_entries(
                        vfs.clone(),
                        local_path.clone(),
                        cross_mount_points,
                        reporter,
                        &cancel,
                    )
                    .await?;
                    for entry in &entries {
                        if cancel.is_cancelled() {
                            return Err(newt_common::Error::cancelled());
                        }
                        reporter.maybe_send_progress(
                            0,
                            items_done,
                            &shown(&*entry.vfs, &entry.path),
                        );
                        delete_entry(reporter, entry, &cancel).await?;
                    }
                }
                break;
            }

            match reporter
                .handle_io_error(
                    e,
                    &format!("Error moving {display} to Trash"),
                    None,
                    &cancel,
                    true,
                )
                .await?
            {
                IssueOutcome::Skip => break,
                IssueOutcome::Retry => {}
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(0, items_done, "");
    Ok(())
}
