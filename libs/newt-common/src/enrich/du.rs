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
//! (`du -x`, the shared walker's `one_file_system`): a mount point below
//! the sized entry is not entered, while the entry itself may be one and
//! is sized regardless.

use std::collections::HashSet;

use futures::StreamExt;

use super::{
    Annotation, EnrichScope, EnrichSink, Enricher, EnricherDescriptor, RegisteredEnricher,
};
use crate::Error;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::walk::{self, Control, Entry, Failed, Resume, Visitor};
use crate::vfs::{File, Vfs, VfsDescriptor, VfsPath, VfsRegistry};

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
            .map(|f| walk_entry(vfs.as_ref(), f.key().to_string(), dir.join(&f.name), sink))
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
fn occupied(f: &crate::vfs::File) -> u64 {
    f.allocated_size.or(f.size).unwrap_or(0)
}

/// Size one directory entry: serial DFS summing file sizes, emitting a
/// growing running total for the entry once per directory entered.
async fn walk_entry(vfs: &dyn Vfs, key: String, root: PathBuf, sink: &EnrichSink) {
    let mut sizer = Sizer {
        key,
        sink,
        bytes: 0,
        seen_links: HashSet::new(),
        left: 0,
        unlisted: 0,
    };
    let options = walk::WalkOptions {
        one_file_system: true,
        excludes: vec![PathBuf::from_wire_str("/proc")],
        ..Default::default()
    };
    if let Err(e) = walk::walk(vfs, std::slice::from_ref(&root), &options, &mut sizer).await {
        log::debug!("du walker: {} failed: {}", root, e);
    }
    // An entry we couldn't list at all stays unannotated — a final
    // "0, complete" would read as an authoritative empty directory.
    if sizer.left > sizer.unlisted {
        sizer.sink.emit_entry(
            sizer.key,
            Annotation::RecursiveSize {
                bytes: sizer.bytes,
                complete: true,
            },
        );
    }
}

struct Sizer<'a> {
    key: String,
    sink: &'a EnrichSink,
    bytes: u64,
    /// Hardlinked inodes already counted within this entry's walk.
    seen_links: HashSet<(u64, u64)>,
    /// Directories left, and those among them whose listing failed.
    left: u64,
    unlisted: u64,
}

#[async_trait::async_trait]
impl Visitor for Sizer<'_> {
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, Error> {
        let file = entry.file;
        if file.is_dir {
            if !file.is_symlink {
                self.sink.emit_entry(
                    self.key.clone(),
                    Annotation::RecursiveSize {
                        bytes: self.bytes,
                        complete: false,
                    },
                );
                self.sink.maybe_flush().await;
            }
            return Ok(Control::Descend);
        }
        // Count each hardlinked inode once per sized entry.
        if file.hard_links.is_some_and(|n| n > 1)
            && let (Some(dev), Some(ino)) = (file.device_id, file.inode)
            && !self.seen_links.insert((dev, ino))
        {
            return Ok(Control::Descend);
        }
        self.bytes += occupied(file);
        Ok(Control::Descend)
    }

    async fn leave(&mut self, _path: &Path, _file: &File) -> Result<(), Error> {
        self.left += 1;
        Ok(())
    }

    async fn failed(&mut self, what: Failed<'_>, _error: Error) -> Result<Resume, Error> {
        if let Failed::Listing(_) = what {
            self.unlisted += 1;
        }
        Ok(Resume::Skip)
    }
}
