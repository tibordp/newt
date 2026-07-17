//! Recursive directory-size enricher ("du").
//!
//! Manual-only: triggered per entry or for a whole listing by pane
//! keybinds, never on navigation. Walks via the `Vfs` trait on the side
//! that owns the filesystem, so it generalizes to any VFS (S3 prefixes,
//! archives, SFTP) and never crosses registry mount boundaries — child
//! mounts live in the registry, not on the walked VFS.
//!
//! Streams running totals per sized entry (replace-by-key,
//! `complete: false`, throttled by the sink) so directories visibly
//! grow while the walk runs, flipping to `complete: true` per subtree.
//! No directory-total badge: the pane's selection totals include
//! computed sizes, so select-all after a whole-listing run reads the
//! directory total off the status bar. Cancellation is by drop, like
//! every enricher; values already applied stay displayed (marked
//! partial) until navigation.
//!
//! Sizing matches `du`: allocated bytes (`File::allocated_size`) when
//! the filesystem reports them — so sparse files (VM disk images,
//! Docker.raw) count what they occupy, not their apparent size — with
//! apparent-size fallback on VFSes without block metadata (S3, SFTP,
//! archives, where the two coincide anyway). Hardlinked files are
//! counted once per sized entry (`(device_id, inode)` dedup, gated on
//! `hard_links > 1`), and the walk never crosses filesystem boundaries
//! (`du -x`): directories whose `device_id` differs from the walk
//! root's are not descended into — a mountpoint entry reports the
//! mounted filesystem's device, so sizing the mountpoint itself still
//! works and stops at further nested mounts.

use std::collections::HashSet;

use futures::StreamExt;

use super::{
    Annotation, EnrichScope, EnrichSink, Enricher, EnricherDescriptor, RegisteredEnricher,
};
use crate::Error;
use crate::vfs::path::PathBuf;
use crate::vfs::{Vfs, VfsDescriptor, VfsPath, VfsRegistry};

/// Directory walks (one per sized entry) running concurrently. Within a
/// walk, listing is serial DFS.
const WALK_CONCURRENCY: usize = 16;

pub struct DuEnricherDescriptor;

pub static DU_ENRICHER_DESCRIPTOR: DuEnricherDescriptor = DuEnricherDescriptor;
inventory::submit! { RegisteredEnricher(&DU_ENRICHER_DESCRIPTOR) }

impl EnricherDescriptor for DuEnricherDescriptor {
    fn id(&self) -> &'static str {
        "du"
    }

    fn activity(&self) -> &'static str {
        "Computing sizes"
    }

    fn automatic(&self) -> bool {
        false
    }

    fn applies_to_vfs(&self, _vfs: &dyn VfsDescriptor) -> bool {
        true
    }
}

pub struct DuEnricher;

#[async_trait::async_trait]
impl Enricher for DuEnricher {
    fn descriptor(&self) -> &'static dyn EnricherDescriptor {
        &DU_ENRICHER_DESCRIPTOR
    }

    async fn enrich(
        &self,
        registry: &VfsRegistry,
        path: &VfsPath,
        scope: &EnrichScope,
        sink: &EnrichSink,
    ) -> Result<(), Error> {
        let (vfs, dir) = registry.resolve(path)?;
        let listing = vfs.list_files(&dir, None).await?;

        // Futures are built eagerly (they run nothing until polled);
        // in-future concurrency (no spawns) so dropping this future
        // cancels every walk.
        let walks: Vec<_> = listing
            .files
            .iter()
            .filter(|f| f.name != ".." && f.is_dir && !f.is_symlink)
            .filter(|f| match scope {
                EnrichScope::AllEntries => true,
                EnrichScope::Entries(keys) => keys.iter().any(|k| k == f.key()),
            })
            .map(|f| {
                walk_entry(
                    vfs.as_ref(),
                    f.key().to_string(),
                    dir.join(&f.name),
                    // The sized entry's own device: a mountpoint entry
                    // reports the mounted fs, so the walk covers it and
                    // stops at the next boundary down.
                    f.device_id,
                    sink,
                )
            })
            .collect();
        futures::stream::iter(walks)
            .buffer_unordered(WALK_CONCURRENCY)
            .collect::<Vec<()>>()
            .await;

        Ok(())
    }
}

/// The bytes an entry contributes: allocated when known (matches
/// `du`), apparent size otherwise.
fn occupied(f: &crate::filesystem::File) -> u64 {
    f.allocated_size.or(f.size).unwrap_or(0)
}

/// Size one directory entry: serial DFS summing file sizes, emitting a
/// growing running total for the entry after every directory listed.
async fn walk_entry(
    vfs: &dyn Vfs,
    key: String,
    root: PathBuf,
    root_device: Option<u64>,
    sink: &EnrichSink,
) {
    let mut bytes = 0u64;
    let mut listed_any = false;
    // Hardlinked inodes already counted within this entry's walk.
    let mut seen_links: HashSet<(u64, u64)> = HashSet::new();
    let mut stack = vec![root];
    while let Some(dir_path) = stack.pop() {
        let listing = match vfs.list_files(&dir_path, None).await {
            Ok(l) => l,
            Err(e) => {
                // A single unreadable subtree shouldn't kill the walk
                // (permission-denied being the canonical case).
                log::debug!("du walker: list_files {} failed: {}", dir_path, e);
                continue;
            }
        };
        listed_any = true;
        for entry in listing.files {
            if entry.name == ".." {
                continue;
            }
            let entry_path = dir_path.join(&entry.name);
            // Mirror the search walker: never descend into /proc, and
            // don't follow directory symlinks (loops, double counting).
            let is_proc = entry_path.components().next() == Some("proc");
            if entry.is_dir {
                // `du -x`: a child directory on a different device is a
                // mount boundary — don't cross it.
                let crosses_fs = matches!(
                    (root_device, entry.device_id),
                    (Some(root), Some(dev)) if root != dev
                );
                if !entry.is_symlink && !is_proc && !crosses_fs {
                    stack.push(entry_path);
                }
            } else {
                // Count each hardlinked inode once per sized entry.
                if entry.hard_links.is_some_and(|n| n > 1)
                    && let (Some(dev), Some(ino)) = (entry.device_id, entry.inode)
                    && !seen_links.insert((dev, ino))
                {
                    continue;
                }
                bytes += occupied(&entry);
            }
        }
        sink.emit_entry(
            key.clone(),
            Annotation::RecursiveSize {
                bytes,
                complete: false,
            },
        );
        sink.maybe_flush().await;
    }
    // An entry we couldn't list at all stays unannotated — a final
    // "0, complete" would read as an authoritative empty directory.
    if listed_any {
        sink.emit_entry(
            key,
            Annotation::RecursiveSize {
                bytes,
                complete: true,
            },
        );
    }
}
