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
}

#[derive(Default)]
pub(super) struct WalkOptions {
    /// Classify through symlinks (archive "follow" mode): symlinks to
    /// directories are recursed into, symlinks to files become plain files.
    /// Cycles among followed targets are detected and skipped.
    pub follow_symlinks: bool,
    /// Path on the source VFS to silently omit — the archive being written,
    /// so it doesn't pack itself.
    pub exclude: Option<PathBuf>,
}

/// Longest chain of dir-symlinks the walk will follow before assuming a
/// cycle it failed to detect structurally (mirrors the archive-read side's
/// `MAX_SYMLINK_HOPS`).
const MAX_FOLLOWED_LINKS: usize = 40;

/// Resolve a raw symlink target against the directory containing the link.
/// Best-effort textual normalization — the VFS surface has no realpath.
pub(super) fn resolve_symlink_target(parent: &Path, target: &str) -> PathBuf {
    let mut path = if target.starts_with('/') {
        PathBuf::root()
    } else {
        parent.to_owned()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                path = path
                    .parent()
                    .map(|p| p.to_owned())
                    .unwrap_or_else(PathBuf::root);
            }
            seg => path.push(seg),
        }
    }
    path
}

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
        let mut pending: Vec<(PathBuf, String, File, Arc<Vec<PathBuf>>)> =
            vec![(source.clone(), file_name, file_entry, Arc::new(Vec::new()))];

        loop {
            for (src_path, rel, file, link_ancestry) in pending.drain(..) {
                if options.exclude.as_ref() == Some(&src_path) {
                    continue;
                }

                if has_symlinks && file.is_symlink && !follow {
                    entries.push(WalkedEntry {
                        source: src_path,
                        rel,
                        kind: WalkedKind::Symlink {
                            target: file.symlink_target.clone().unwrap_or_default(),
                        },
                        file,
                    });
                } else if has_symlinks && file.is_symlink && follow && file.is_dir {
                    let parent = src_path
                        .parent()
                        .map(|p| p.to_owned())
                        .unwrap_or_else(PathBuf::root);
                    let target = resolve_symlink_target(
                        &parent,
                        file.symlink_target.as_deref().unwrap_or_default(),
                    );
                    // A target that is itself on the followed chain, or an
                    // ancestor of the link, recurses forever.
                    let cycle = link_ancestry.contains(&target) || src_path.starts_with(&target);
                    if cycle || link_ancestry.len() >= MAX_FOLLOWED_LINKS {
                        reporter
                            .raise_issue(
                                IssueKind::Other("SymlinkCycle".to_string()),
                                format!("Symlink cycle at {}", src_path),
                                Some(format!("target: {}", target)),
                                vec![IssueAction::Skip],
                            )
                            .await?;
                        continue;
                    }
                    let mut ancestry = (*link_ancestry).clone();
                    ancestry.push(target.clone());
                    entries.push(WalkedEntry {
                        source: src_path,
                        rel: rel.clone(),
                        kind: WalkedKind::Directory,
                        file,
                    });
                    // Recurse into the resolved target: identical on a real
                    // FS, and keeps the frame path physical for VFSes that
                    // don't resolve links on access.
                    stack.push(DirFrame {
                        src: target,
                        rel,
                        link_ancestry: Arc::new(ancestry),
                    });
                } else if has_symlinks && file.is_symlink && follow {
                    // Followed file symlink: the dirent's size is the link's
                    // own length — stat the target for the real size (drives
                    // progress totals and the zip writer's zip64 decision).
                    let parent = src_path
                        .parent()
                        .map(|p| p.to_owned())
                        .unwrap_or_else(PathBuf::root);
                    let target = resolve_symlink_target(
                        &parent,
                        file.symlink_target.as_deref().unwrap_or_default(),
                    );
                    let resolved = src_vfs.file_info(&target).await.unwrap_or(file);
                    total_bytes += resolved.size.unwrap_or(0);
                    entries.push(WalkedEntry {
                        source: target,
                        rel,
                        kind: WalkedKind::File,
                        file: resolved,
                    });
                } else if file.is_dir {
                    entries.push(WalkedEntry {
                        source: src_path.clone(),
                        rel: rel.clone(),
                        kind: WalkedKind::Directory,
                        file,
                    });
                    stack.push(DirFrame {
                        src: src_path,
                        rel,
                        link_ancestry,
                    });
                } else {
                    total_bytes += file.size.unwrap_or(0);
                    entries.push(WalkedEntry {
                        source: src_path,
                        rel,
                        kind: WalkedKind::File,
                        file,
                    });
                }
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
                pending.push((src_path, rel, file, frame.link_ancestry.clone()));
            }
            reporter.maybe_send_scanning(entries.len() as u64, total_bytes);
        }
    }

    Ok((entries, total_bytes))
}
