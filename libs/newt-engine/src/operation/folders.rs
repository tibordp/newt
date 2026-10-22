//! A folder landing on an existing folder: merged into it, replaced, or
//! skipped — unless the existing folder is a package to its filesystem,
//! which makes it a file's conflict.

use crate::vfs::attributes::RenameOptions;
use std::hash::{BuildHasher, Hasher};

use super::*;
use crate::vfs::walk::{self, Control, Entry, Visitor, WalkOptions};

pub(super) enum FolderAnswer {
    Merge,
    Replace,
    Skip,
}

/// What a folder from `source` does about `existing`, the folder at `dest`.
/// The destination's filesystem is asked first whether `existing` is a
/// package; if so it is replaced like a file, by the "Existing files"
/// answers. Otherwise `folders` decides.
#[allow(clippy::too_many_arguments)]
pub(super) async fn folder_conflict(
    reporter: &mut ProgressReporter,
    folders: FolderConflict,
    src_vfs: &dyn Vfs,
    source: &Path,
    source_file: &File,
    dst_vfs: &dyn Vfs,
    dest: &Path,
    existing: &File,
    cancel: &CancellationToken,
) -> Result<FolderAnswer, newt_common::Error> {
    let package = match cancellable(cancel, dst_vfs.is_package(dest)).await {
        Ok(package) => package,
        Err(_) if cancel.is_cancelled() => return Err(newt_common::Error::cancelled()),
        // Unknown is a folder, as it was before anyone asked.
        Err(e) => {
            debug!("folder_conflict: is_package {dest}: {e}");
            false
        }
    };
    if package {
        let action = reporter
            .ask_replace(format!("Package already exists: {}", shown(dst_vfs, dest)))
            .await?;
        let replace = match action {
            // A package's size is its contents'.
            IssueAction::OverwriteIfSizeDiffers | IssueAction::OverwriteIfSizeOrDateDiffers => {
                let source = File {
                    size: Some(tree_size(src_vfs, source, cancel).await?),
                    ..source_file.clone()
                };
                let existing = File {
                    size: Some(tree_size(dst_vfs, dest, cancel).await?),
                    ..existing.clone()
                };
                replaces(action, &source, &existing)
            }
            action => replaces(action, source_file, existing),
        };
        return Ok(if replace {
            FolderAnswer::Replace
        } else {
            FolderAnswer::Skip
        });
    }
    let action = match folders {
        FolderConflict::Merge => IssueAction::Merge,
        FolderConflict::Replace => IssueAction::Overwrite,
        FolderConflict::Ask => {
            reporter
                .raise_issue(
                    IssueKind::FolderExists,
                    format!("Folder already exists: {}", shown(dst_vfs, dest)),
                    None,
                    vec![
                        IssueAction::Merge,
                        IssueAction::Overwrite,
                        IssueAction::Skip,
                    ],
                )
                .await?
        }
    };
    Ok(match action {
        IssueAction::Merge => FolderAnswer::Merge,
        IssueAction::Overwrite => FolderAnswer::Replace,
        IssueAction::Skip => FolderAnswer::Skip,
        _ => unreachable!("not offered"),
    })
}

/// The bytes of the files under `root`.
async fn tree_size(
    vfs: &dyn Vfs,
    root: &Path,
    cancel: &CancellationToken,
) -> Result<u64, newt_common::Error> {
    struct Sum(u64);
    #[async_trait::async_trait]
    impl Visitor for Sum {
        async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, newt_common::Error> {
            if !entry.file.is_dir {
                self.0 += entry.file.size.unwrap_or(0);
            }
            Ok(Control::Descend)
        }
    }
    let mut sum = Sum(0);
    cancellable(
        cancel,
        walk::walk(
            vfs,
            std::slice::from_ref(&root.to_owned()),
            &WalkOptions::default(),
            &mut sum,
        ),
    )
    .await?;
    Ok(sum.0)
}

/// Put what is at `new` where `old` is, in one step where the filesystem
/// can exchange them. Returns where the old one is now: `new`'s former
/// name, as an exchange leaves it, or a name beside it.
pub(super) async fn swap_in(
    vfs: &dyn Vfs,
    new: &Path,
    old: &Path,
) -> Result<PathBuf, newt_common::Error> {
    match vfs.exchange(new, old).await {
        Ok(()) => return Ok(new.to_owned()),
        Err(e) if e.kind != newt_common::ErrorKind::NotSupported => return Err(e),
        Err(_) => {}
    }
    // No exchange: the old one steps aside, the new one takes its name, and
    // the old one comes back if it can't.
    let aside = sibling(old, "aside");
    vfs.rename(old, &aside, &RenameOptions::default()).await?;
    if let Err(e) = vfs.rename(new, old, &RenameOptions::default()).await {
        if let Err(back) = vfs.rename(&aside, old, &RenameOptions::default()).await {
            warn!("swap_in: cannot move {aside} back to {old}: {back}");
        }
        return Err(e);
    }
    Ok(aside)
}

/// Remove the tree at `path`, asking about what fails.
pub(super) async fn remove_whole(
    vfs: &dyn Vfs,
    path: &Path,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<(), newt_common::Error> {
    loop {
        match remove_tree(vfs, path).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Cannot remove {}", shown(vfs, path)),
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
    }
}

/// Remove the tree at `path` as far as it goes, asking nobody: for a
/// staging copy left by an operation that has stopped.
pub(super) async fn remove_quietly(vfs: &dyn Vfs, path: &Path) {
    if let Err(e) = remove_tree(vfs, path).await {
        warn!("cannot remove {path}: {e}");
    }
}

async fn remove_tree(vfs: &dyn Vfs, path: &Path) -> Result<(), newt_common::Error> {
    if vfs.descriptor().can_remove_tree() {
        return vfs.remove_tree(path).await;
    }
    // Files on the way down, folders on the way back up.
    struct Removal<'a> {
        vfs: &'a dyn Vfs,
    }
    #[async_trait::async_trait]
    impl Visitor for Removal<'_> {
        async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, newt_common::Error> {
            if !entry.file.is_dir || entry.file.is_symlink() {
                self.vfs.remove_file(entry.path).await?;
            }
            Ok(Control::Descend)
        }

        async fn leave(&mut self, path: &Path, _file: &File) -> Result<(), newt_common::Error> {
            self.vfs.remove_dir(path).await
        }
    }
    walk::walk(
        vfs,
        std::slice::from_ref(&path.to_owned()),
        &WalkOptions::default(),
        &mut Removal { vfs },
    )
    .await
}

/// A hidden name beside `path` nothing else has.
pub(super) fn sibling(path: &Path, purpose: &str) -> PathBuf {
    let name = hidden_name(purpose);
    match path.parent() {
        Some(parent) => parent.join(&name),
        None => PathBuf::from_wire_str(&name),
    }
}

/// `.newt-<purpose>-<random>`.
pub(super) fn hidden_name(purpose: &str) -> String {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!(".newt-{purpose}-{:016x}", hasher.finish())
}
