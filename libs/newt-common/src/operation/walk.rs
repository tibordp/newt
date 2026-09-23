use super::*;

// --- Shared source-tree walk (copy and archive planning) ---

pub(super) enum WalkedKind {
    File,
    Directory,
    Symlink { target: String },
}

pub(super) struct WalkedEntry {
    pub source: PathBuf,
    /// Path relative to the selection: the top-level source's file name, then
    /// dirent names down the tree, `/`-joined.
    pub rel: String,
    pub kind: WalkedKind,
    /// The dirent as seen during the walk — carries the metadata (mode,
    /// owner, mtime, size) so consumers don't need a second stat pass.
    pub file: File,
    pub metadata: Option<crate::vfs::VfsMetadata>,
    /// A directory that is the root of another filesystem than its
    /// parent's. Removing it fails while it is mounted.
    pub mount_point: bool,
}

#[derive(Default)]
pub(super) struct WalkOptions {
    /// Classify through symlinks (archive "follow" mode): symlinks to
    /// directories are recursed into, symlinks to files become plain files.
    /// Cycles among followed targets are detected and skipped.
    pub follow_symlinks: bool,
    pub capture_directory_metadata: bool,
    /// Path on the source VFS to silently omit — the archive being written,
    /// so it doesn't pack itself.
    pub exclude: Option<PathBuf>,
    /// Stay on each source's filesystem: a mount point under it is walked
    /// as an empty directory (rsync's `-x`). A source that is itself a
    /// mount point is always walked.
    pub one_file_system: bool,
}

/// Whether a directory listed under a parent on `parent_device` is the
/// root of another filesystem. Only the local filesystem reports devices,
/// so nothing is ever a mount point elsewhere. Bind mounts share their
/// source's device and go unnoticed; btrfs subvolumes have their own and
/// count, as they do for `rm --one-file-system`.
pub(super) fn is_mount_point(parent_device: Option<u64>, file: &File) -> bool {
    file.is_dir
        && !file.is_symlink
        && matches!((parent_device, file.device_id), (Some(p), Some(d)) if p != d)
}

/// The device of a selected path's parent, for telling whether the
/// selection is itself a mount point. Asked only when the entry reports a
/// device at all.
pub(super) async fn parent_device(vfs: &dyn Vfs, path: &Path, file: &File) -> Option<u64> {
    file.device_id?;
    let parent = path.parent()?;
    vfs.file_info(parent).await.ok().and_then(|f| f.device_id)
}

/// Report a mount point a walk stayed out of. Skip is the only action;
/// `hint` names the dialog option that would have walked into it.
pub(super) async fn skip_mount_point(
    reporter: &mut ProgressReporter,
    path: &Path,
    hint: &str,
) -> Result<(), crate::Error> {
    reporter
        .raise_issue(
            IssueKind::Other("MountPoint".into()),
            format!("Skipped mount point {path}"),
            Some(format!(
                "The directory is the root of another filesystem. {hint}"
            )),
            vec![IssueAction::Skip],
        )
        .await?;
    Ok(())
}

/// Longest chain of dir-symlinks the walk will follow before assuming a
/// cycle it failed to detect structurally (mirrors the archive-read side's
/// `MAX_SYMLINK_HOPS`).
const MAX_FOLLOWED_LINKS: usize = 40;

pub(super) async fn walk_sources(
    src_vfs: &dyn Vfs,
    src_descriptor: &dyn VfsDescriptor,
    sources: &[PathBuf],
    options: &WalkOptions,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<(Vec<WalkedEntry>, u64), crate::Error> {
    struct DirFrame {
        src: PathBuf,
        rel: String,
        /// Normalized targets of the dir-symlinks followed to reach here.
        link_ancestry: Arc<Vec<PathBuf>>,
        device: Option<u64>,
    }

    struct Pending {
        src: PathBuf,
        rel: String,
        file: File,
        link_ancestry: Arc<Vec<PathBuf>>,
        parent_device: Option<u64>,
        top_level: bool,
    }

    let mut entries: Vec<WalkedEntry> = Vec::new();
    let mut total_bytes = 0u64;
    let has_symlinks = src_descriptor.has_symlinks();
    let follow = options.follow_symlinks;

    for source in sources {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let file_name = source
            .file_name()
            .ok_or_else(|| crate::Error::custom("source has no file name".to_string()))?
            .to_string();

        // Classify the top-level source. Use file_info (stat) when available,
        // fall back to listing the parent directory for VFSes like S3 where
        // directories aren't real filesystem entries.
        let file_entry = if src_descriptor.can_stat_directories() {
            src_vfs.file_info(source).await?
        } else {
            let parent = source
                .parent()
                .ok_or_else(|| crate::Error::custom("source has no parent".to_string()))?;
            let file_list = cancellable(cancel, src_vfs.list_files(parent, None)).await?;
            file_list
                .files
                .into_iter()
                .find(|f| f.name == file_name)
                .ok_or_else(|| crate::Error::custom(format!("source not found: {}", source)))?
        };

        let mut stack: Vec<DirFrame> = Vec::new();
        // The top-level source enters classification as a pseudo-child;
        // directory listings feed the same queue below.
        let parent_device = parent_device(src_vfs, source, &file_entry).await;
        let mut pending: Vec<Pending> = vec![Pending {
            src: source.clone(),
            rel: file_name,
            file: file_entry,
            link_ancestry: Arc::new(Vec::new()),
            parent_device,
            top_level: true,
        }];

        loop {
            'entry: for Pending {
                src: mut src_path,
                rel,
                mut file,
                link_ancestry,
                parent_device,
                top_level,
            } in pending.drain(..)
            {
                if options.exclude.as_ref() == Some(&src_path) {
                    continue;
                }
                let mut ancestry = (*link_ancestry).clone();
                if has_symlinks && file.is_symlink && follow {
                    let mut hops = 0;
                    while file.is_symlink {
                        if hops >= MAX_FOLLOWED_LINKS {
                            reporter
                                .raise_issue(
                                    IssueKind::Other("SymlinkCycle".into()),
                                    format!("Symbolic link cycle at {src_path}"),
                                    None,
                                    vec![IssueAction::Skip],
                                )
                                .await?;
                            continue 'entry;
                        }
                        let resolved = loop {
                            let result = async {
                                let target = src_vfs.resolve_link(&src_path).await?;
                                let file = src_vfs.file_info(&target).await?;
                                Ok((target, file))
                            };
                            match cancellable(cancel, result).await {
                                Ok(resolved) => break resolved,
                                Err(e) => match reporter
                                    .handle_io_error(
                                        e,
                                        &format!("Cannot follow {src_path}"),
                                        None,
                                        cancel,
                                        true,
                                    )
                                    .await?
                                {
                                    IssueOutcome::Retry => {}
                                    IssueOutcome::Skip => continue 'entry,
                                },
                            }
                        };
                        let (target, target_file) = resolved;
                        if ancestry.contains(&target)
                            || (target_file.is_dir && src_path.starts_with(&target))
                        {
                            reporter
                                .raise_issue(
                                    IssueKind::Other("SymlinkCycle".into()),
                                    format!("Symbolic link cycle at {src_path}"),
                                    None,
                                    vec![IssueAction::Skip],
                                )
                                .await?;
                            continue 'entry;
                        }
                        ancestry.push(target.clone());
                        src_path = target;
                        file = target_file;
                        hops += 1;
                    }
                }
                let mut metadata = None;
                if file.is_dir && !file.is_symlink && options.capture_directory_metadata {
                    loop {
                        match cancellable(cancel, src_vfs.get_metadata(&src_path)).await {
                            Ok(meta) => {
                                metadata = Some(meta);
                                break;
                            }
                            Err(e) => {
                                if !super::preserve::resolve_preservation(
                                    reporter,
                                    AttributeKind::Metadata,
                                    &src_path,
                                    e,
                                )
                                .await?
                                {
                                    break;
                                }
                            }
                        }
                    }
                }
                let mount_point = is_mount_point(parent_device, &file);
                let kind = if file.is_symlink {
                    WalkedKind::Symlink {
                        target: file.symlink_target.clone().unwrap_or_default(),
                    }
                } else if file.is_dir {
                    if !(mount_point && options.one_file_system && !top_level) {
                        stack.push(DirFrame {
                            src: src_path.clone(),
                            rel: rel.clone(),
                            link_ancestry: Arc::new(ancestry),
                            device: file.device_id,
                        });
                    }
                    WalkedKind::Directory
                } else {
                    total_bytes += file.size.unwrap_or(0);
                    WalkedKind::File
                };
                entries.push(WalkedEntry {
                    source: src_path,
                    rel,
                    kind,
                    file,
                    metadata,
                    mount_point,
                });
            }

            let Some(frame) = stack.pop() else { break };
            if cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }

            let file_list = loop {
                match cancellable(cancel, src_vfs.list_files(&frame.src, None)).await {
                    Ok(list) => break list,
                    Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                    Err(e) => {
                        match reporter
                            .handle_io_error(
                                e,
                                &format!("Error scanning directory {}", frame.src),
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

            for file in file_list.files {
                if file.name == ".." {
                    continue;
                }
                let src_path = frame.src.join(&file.name);
                let rel = format!("{}/{}", frame.rel, file.name);
                pending.push(Pending {
                    src: src_path,
                    rel,
                    file,
                    link_ancestry: frame.link_ancestry.clone(),
                    parent_device: frame.device,
                    top_level: false,
                });
            }
            reporter.maybe_send_scanning(entries.len() as u64, total_bytes);
        }
    }

    Ok((entries, total_bytes))
}
