use super::*;
use crate::vfs::Symlink;
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
    vfs: &dyn Vfs,
    path: &Path,
    hint: &str,
) -> Result<(), newt_common::Error> {
    reporter
        .raise_issue(
            IssueKind::Other("MountPoint".into()),
            format!("Skipped mount point {}", shown(vfs, path)),
            Some(format!(
                "The directory is the root of another filesystem. {hint}"
            )),
            vec![IssueAction::Skip],
        )
        .await?;
    Ok(())
}

/// Report a FIFO, socket or device a walk left out: reading one blocks
/// until something writes to it, or never ends (`/dev/zero`). Skip is the
/// only action.
async fn skip_special_file(
    reporter: &mut ProgressReporter,
    vfs: &dyn Vfs,
    path: &Path,
) -> Result<(), newt_common::Error> {
    reporter
        .raise_issue(
            IssueKind::Other("SpecialFile".into()),
            format!("Skipped {}: not a regular file", shown(vfs, path)),
            Some("FIFOs, sockets and devices have no contents to copy.".into()),
            vec![IssueAction::Skip],
        )
        .await?;
    Ok(())
}

/// Route a walk failure through the operation's issue prompt.
pub(super) async fn resume_after(
    reporter: &mut ProgressReporter,
    vfs: &dyn Vfs,
    what: Failed<'_>,
    error: newt_common::Error,
    cancel: &CancellationToken,
) -> Result<Resume, newt_common::Error> {
    let context = match what {
        Failed::Listing(path) => format!("Error scanning directory {}", shown(vfs, path)),
        Failed::Following(path) => format!("Cannot follow {}", shown(vfs, path)),
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
) -> Result<(Vec<WalkedEntry>, u64), newt_common::Error> {
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
        // A selected link is copied or packed as the link.
        follow_root: false,
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

impl Collector<'_> {
    /// The target of a link whose listing couldn't read it, asked about
    /// rather than guessed: a link recreated without its target would point
    /// nowhere. Retry reads the link again; `None` when the user skips it.
    async fn unknown_link_target(
        &mut self,
        path: &Path,
    ) -> Result<Option<String>, newt_common::Error> {
        let context = format!("Cannot read where {} points", shown(self.vfs, path));
        let mut error =
            newt_common::Error::custom("the filesystem did not report the link's target");
        loop {
            match self
                .reporter
                .handle_io_error(error, &context, None, self.cancel, true)
                .await?
            {
                IssueOutcome::Skip => return Ok(None),
                IssueOutcome::Retry => {}
            }
            error = match cancellable(self.cancel, self.vfs.file_info(path, false)).await {
                Ok(file) => match file.symlink {
                    Some(Symlink::Target(target)) => return Ok(Some(target)),
                    _ => newt_common::Error::custom(
                        "the filesystem did not report the link's target",
                    ),
                },
                Err(e) => e,
            };
        }
    }
}

#[async_trait::async_trait]
impl Visitor for Collector<'_> {
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, newt_common::Error> {
        if self.cancel.is_cancelled() {
            return Err(newt_common::Error::cancelled());
        }
        let file = entry.file;
        let mut metadata = None;
        if file.is_dir && !file.is_symlink() && self.capture_directory_metadata {
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
                            self.vfs,
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
        if !file.is_symlink() && !file.is_dir && file.is_special() {
            skip_special_file(self.reporter, self.vfs, entry.path).await?;
            return Ok(Control::Skip);
        }
        let kind = if let Some(link) = &file.symlink {
            let target = match link {
                Symlink::Target(target) => target.clone(),
                Symlink::Unknown => match self.unknown_link_target(entry.path).await? {
                    Some(target) => target,
                    None => return Ok(Control::Skip),
                },
            };
            WalkedKind::Symlink { target }
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
        error: newt_common::Error,
    ) -> Result<Resume, newt_common::Error> {
        resume_after(self.reporter, self.vfs, what, error, self.cancel).await
    }

    async fn cycle(&mut self, path: &Path) -> Result<(), newt_common::Error> {
        self.reporter
            .raise_issue(
                IssueKind::Other("SymlinkCycle".into()),
                format!("Symbolic link cycle at {}", shown(self.vfs, path)),
                None,
                vec![IssueAction::Skip],
            )
            .await?;
        Ok(())
    }
}
