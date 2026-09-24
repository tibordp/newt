use super::*;
use crate::vfs::walk::{self, Control, Entry, Failed, Resume, Visitor};

const MOUNT_POINT_HINT: &str =
    "To change its contents too, tick \"Descend into mount points\" in the Properties dialog.";

/// Every entry under `root`, root first, as `(path, is_dir)`; a root
/// that is a file or a link is the one entry. Without
/// `cross_mount_points` a mount point under the root is left out whole,
/// its own entry included, and the walk says so.
pub(super) async fn collect_chmod_entries(
    vfs: &dyn Vfs,
    root: &Path,
    cross_mount_points: bool,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Vec<(PathBuf, bool)>, crate::Error> {
    let mut collector = AttributeCollector {
        reporter,
        cancel,
        cross_mount_points,
        entries: Vec::new(),
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
    Ok(collector.entries)
}

struct AttributeCollector<'a> {
    reporter: &'a mut ProgressReporter,
    cancel: &'a CancellationToken,
    cross_mount_points: bool,
    entries: Vec<(PathBuf, bool)>,
}

#[async_trait::async_trait]
impl Visitor for AttributeCollector<'_> {
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, crate::Error> {
        if self.cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        let is_dir = entry.file.is_dir && !entry.file.is_symlink;
        if is_dir && entry.mount_point && !self.cross_mount_points && entry.depth > 0 {
            skip_mount_point(self.reporter, entry.path, MOUNT_POINT_HINT).await?;
            return Ok(Control::Skip);
        }
        self.entries.push((entry.path.to_owned(), is_dir));
        Ok(Control::Descend)
    }

    async fn failed(
        &mut self,
        what: Failed<'_>,
        error: crate::Error,
    ) -> Result<Resume, crate::Error> {
        resume_after(self.reporter, what, error, self.cancel).await
    }
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
    cross_mount_points: bool,
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

        if recursive {
            let entries =
                collect_chmod_entries(&*vfs, &local_path, cross_mount_points, reporter, &cancel)
                    .await?;
            for (entry, _) in entries {
                let display = format!("{}:{}", vfs_path.vfs_id, entry);
                all_entries.push((vfs.clone(), entry, display));
            }
            continue;
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
    cross_mount_points: bool,
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
            let entries =
                collect_chmod_entries(&*vfs, &local_path, cross_mount_points, reporter, &cancel)
                    .await?;
            for (entry, entry_is_dir) in entries {
                if entry_is_dir && !include_dirs {
                    continue;
                }
                let display = format!("{}:{}", vfs_path.vfs_id, entry);
                all_entries.push((vfs.clone(), entry, display));
            }
            continue;
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
