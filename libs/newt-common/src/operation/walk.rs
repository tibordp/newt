use super::*;
use crate::vfs::walk::{self, Control, Entry, Failed, Resume, Visitor};

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

/// Route a walk failure through the operation's issue prompt.
pub(super) async fn resume_after(
    reporter: &mut ProgressReporter,
    what: Failed<'_>,
    error: crate::Error,
    cancel: &CancellationToken,
) -> Result<Resume, crate::Error> {
    let context = match what {
        Failed::Listing(path) => format!("Error scanning directory {path}"),
        Failed::Following(path) => format!("Cannot follow {path}"),
    };
    Ok(
        match reporter
            .handle_io_error(error, &context, None, cancel, true)
            .await?
        {
            IssueOutcome::Retry => Resume::Retry,
            IssueOutcome::Skip => Resume::Skip,
        },
    )
}

pub(super) async fn walk_sources(
    src_vfs: &dyn Vfs,
    sources: &[PathBuf],
    options: &WalkOptions,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<(Vec<WalkedEntry>, u64), crate::Error> {
    let mut collector = Collector {
        vfs: src_vfs,
        capture_directory_metadata: options.capture_directory_metadata,
        reporter,
        cancel,
        entries: Vec::new(),
        total_bytes: 0,
    };
    let walk_options = walk::WalkOptions {
        follow_symlinks: options.follow_symlinks,
        one_file_system: options.one_file_system,
        excludes: options.exclude.iter().cloned().collect(),
    };
    cancellable(
        cancel,
        walk::walk(src_vfs, sources, &walk_options, &mut collector),
    )
    .await?;
    Ok((collector.entries, collector.total_bytes))
}

struct Collector<'a> {
    vfs: &'a dyn Vfs,
    capture_directory_metadata: bool,
    reporter: &'a mut ProgressReporter,
    cancel: &'a CancellationToken,
    entries: Vec<WalkedEntry>,
    total_bytes: u64,
}

#[async_trait::async_trait]
impl Visitor for Collector<'_> {
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, crate::Error> {
        if self.cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        let file = entry.file;
        let mut metadata = None;
        if file.is_dir && !file.is_symlink && self.capture_directory_metadata {
            loop {
                match cancellable(self.cancel, self.vfs.get_metadata(entry.path)).await {
                    Ok(meta) => {
                        metadata = Some(meta);
                        break;
                    }
                    Err(e) => {
                        if !super::preserve::resolve_preservation(
                            self.reporter,
                            AttributeKind::Metadata,
                            entry.path,
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
        let kind = if file.is_symlink {
            WalkedKind::Symlink {
                target: file.symlink_target.clone().unwrap_or_default(),
            }
        } else if file.is_dir {
            WalkedKind::Directory
        } else {
            self.total_bytes += file.size.unwrap_or(0);
            WalkedKind::File
        };
        self.entries.push(WalkedEntry {
            source: entry.path.to_owned(),
            rel: entry.rel.to_string(),
            kind,
            file: file.clone(),
            metadata,
            mount_point: entry.mount_point,
        });
        self.reporter
            .maybe_send_scanning(self.entries.len() as u64, self.total_bytes);
        Ok(Control::Descend)
    }

    async fn failed(
        &mut self,
        what: Failed<'_>,
        error: crate::Error,
    ) -> Result<Resume, crate::Error> {
        resume_after(self.reporter, what, error, self.cancel).await
    }

    async fn cycle(&mut self, path: &Path) -> Result<(), crate::Error> {
        self.reporter
            .raise_issue(
                IssueKind::Other("SymlinkCycle".into()),
                format!("Symbolic link cycle at {path}"),
                None,
                vec![IssueAction::Skip],
            )
            .await?;
        Ok(())
    }
}
