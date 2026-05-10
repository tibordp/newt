//! `SearchVfs` — recursive-search-as-a-VFS.
//!
//! A search becomes a mounted VFS whose entries are the matches: results
//! show up as a flat directory the user can browse, select, open, copy,
//! delete — every existing pane affordance reuses the matching results
//! without any one-off "search results modal" plumbing.
//!
//! ## Shape (see `design_docs/DESIGN_RECURSIVE_SEARCH.md`)
//!
//! - **Flat list, not pruned tree.** Every match is a top-level entry at
//!   `/`. The displayed `name` is the basename; identity within the
//!   listing is the relative path (set on `File::key`); the source path
//!   in the underlying VFS is set on `File::source` for transparent
//!   redirect at the registry layer.
//! - **`has_origin = false`.** The search results live in their own
//!   addressable space — `..` does not unwind into the search root.
//!   Leaving the search is done via history (Alt+Left).
//! - **No mutating ops.** Every leaf op (read, write, rename, delete,
//!   metadata, copy/move) is redirected by `Vfs::redirect_target` at the
//!   registry layer; `SearchVfs` itself only serves `list_files`,
//!   `file_info`, `poll_changes`, `redirect_target`, and `fs_stats`.
//! - **Walker.** Spawned in `SearchVfs::new`, recursive DFS via
//!   `registry.list_files` on the source VFS. Skips entries whose
//!   `vfs_id` differs from the search root (so mounted child VFSes —
//!   archives etc. — are *not* descended into; that falls out for free
//!   because mounts live in `VfsRegistry`, not the underlying file
//!   system). Matches by name first; content match (when requested) is
//!   done via the supplied `FileReader::find_in_file`.
//! - **Status surfacing.** `running` flag on the VFS so the frontend can
//!   show "Searching..." vs "N results"; cancellation token unwinds the
//!   walker promptly on unmount.
//!
//! See `DESIGN_RECURSIVE_SEARCH.md` for rejected alternatives (pruned
//! tree, `File::Real`/`File::Alias` enum, etc.) and what's deliberately
//! deferred (in-place param refinement, archive/S3 native search, …).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::file_reader::{FileReader, SearchPattern};
use crate::filesystem::{File, FsStats};
use crate::{Error, ErrorKind};

use super::{
    Breadcrumb, DisplayPathMatch, RegisteredDescriptor, Vfs, VfsChangeNotifier, VfsDescriptor,
    VfsPath,
};

// ---------------------------------------------------------------------------
// SearchParams
// ---------------------------------------------------------------------------

/// Parameters for a search. Captured at mount time and effectively
/// immutable for the lifetime of the `SearchVfs`. In-place refinement is
/// deliberately deferred — the unmount+remount UX is fine for v1.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SearchParams {
    /// Glob pattern for the basename (e.g. `*.rs`, `Cargo.*`). When
    /// `None`, matches any name.
    pub name_pattern: Option<String>,
    /// Optional content pattern — runs `FileReader::find_in_file` on
    /// every entry whose name matched. When `None`, name-match alone is
    /// sufficient.
    pub content_pattern: Option<SearchPattern>,
    /// Whether the name glob is case-sensitive. Content pattern case
    /// sensitivity is encoded into `SearchPattern` itself (regex flags
    /// or literal bytes).
    pub case_sensitive: bool,
    /// Whether the walker follows symlinks during traversal. Off by
    /// default — symlink loops and double-counting cause more pain than
    /// they save.
    pub follow_symlinks: bool,
    /// Per-file size cap for content search. Files larger than this are
    /// skipped (matched-by-name still surfaces). 0 means unlimited.
    pub content_size_cap: u64,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            name_pattern: None,
            content_pattern: None,
            case_sensitive: false,
            follow_symlinks: false,
            content_size_cap: 10 * 1024 * 1024, // 10 MiB
        }
    }
}

// ---------------------------------------------------------------------------
// SearchStatus
// ---------------------------------------------------------------------------

const STATUS_RUNNING: u8 = 0;
const STATUS_DONE: u8 = 1;
const STATUS_CANCELLED: u8 = 2;

/// Coarse walker state, surfaced to the frontend via `Vfs::status_info`
/// (currently fed through `mount_meta` since adding a trait method is a
/// v1 polish detail rather than a load-bearing decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStatus {
    Running,
    Done,
    Cancelled,
}

impl SearchStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            STATUS_DONE => SearchStatus::Done,
            STATUS_CANCELLED => SearchStatus::Cancelled,
            _ => SearchStatus::Running,
        }
    }
}

// ---------------------------------------------------------------------------
// SearchVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SearchVfsDescriptor;

/// `mount_meta` carries the human-readable label of the search root
/// (rendered through the source VFS's `format_path` at mount time) so
/// the descriptor can render breadcrumbs / display paths even after
/// the source VFS is unmounted.
fn mount_meta_label(mount_meta: &[u8]) -> String {
    String::from_utf8_lossy(mount_meta).into_owned()
}

impl VfsDescriptor for SearchVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "search"
    }
    fn display_name(&self) -> &'static str {
        "Search"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    // The search results aren't a virtual subdirectory of the root —
    // they're a separate addressable space. Backspace at "/" must not
    // escape into the source VFS; leaving the search is via history.
    fn has_origin(&self) -> bool {
        false
    }
    fn auto_refresh(&self) -> bool {
        false
    }
    fn can_revalidate(&self) -> bool {
        false
    }
    fn can_watch(&self) -> bool {
        // Walker pushes batches through `VfsChangeNotifier`, not through
        // an OS watch; keep this `false` so the navigation layer doesn't
        // try to set up a polling watch.
        false
    }

    // Read/write/etc. capabilities are *not* set here — every leaf op
    // is redirected at the registry layer via `redirect_target`, which
    // means the *source* VFS's capabilities apply. Keeping these all
    // `false` ensures any code that only consults the descriptor (and
    // somehow misses the redirect path) fails closed rather than silently
    // operating on synthetic paths.
    fn can_read_sync(&self) -> bool {
        false
    }
    fn can_read_async(&self) -> bool {
        false
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        false
    }
    fn can_create_directory(&self) -> bool {
        false
    }
    fn can_create_symlink(&self) -> bool {
        false
    }
    fn can_touch(&self) -> bool {
        false
    }
    fn can_truncate(&self) -> bool {
        false
    }
    fn can_set_metadata(&self) -> bool {
        false
    }
    fn can_remove(&self) -> bool {
        false
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        false
    }
    fn can_stat_directories(&self) -> bool {
        false
    }
    fn can_fs_stats(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        false
    }
    fn can_copy_within(&self) -> bool {
        false
    }
    fn can_hard_link(&self) -> bool {
        false
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        let label = mount_meta_label(mount_meta);
        let inner = path.to_string_lossy();
        let inner = inner.trim_start_matches('/');
        if inner.is_empty() {
            format!("Search in {}", label)
        } else {
            // SearchVfs is conceptually flat; this branch only fires for
            // paths constructed by joining pane.path + key (which is
            // exactly how identity works for selections).
            format!("Search in {} → {}", label, inner)
        }
    }

    fn breadcrumbs(&self, _path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        let label = mount_meta_label(mount_meta);
        vec![
            Breadcrumb {
                label: "Search: ".to_string(),
                nav_path: "/".to_string(),
            },
            Breadcrumb {
                label,
                nav_path: "/".to_string(),
            },
        ]
    }

    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        None
    }

    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        let s = mount_meta_label(mount_meta);
        if s.is_empty() { None } else { Some(s) }
    }
}

pub static SEARCH_VFS_DESCRIPTOR: SearchVfsDescriptor = SearchVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&SEARCH_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// SearchVfs
// ---------------------------------------------------------------------------

/// A search rooted at `search_root` with parameters captured at mount
/// time. Walker runs in the background; results stream out through
/// `list_files`'s batch sender and the change notifier.
pub struct SearchVfs {
    mount_meta: Vec<u8>,
    /// Accumulated matches keyed by relative path under `search_root`.
    /// Push-only during the walker's lifetime; never mutated afterwards.
    results: Arc<RwLock<Vec<File>>>,
    status: Arc<AtomicU8>,
    notifier: VfsChangeNotifier,
    cancel: CancellationToken,
    /// Best-effort match counter, refreshed atomically as results stream
    /// in. Used by the walker thread to throttle batch publishes.
    hit_count: Arc<AtomicUsize>,
}

impl SearchVfs {
    /// Mount a search rooted at `search_root` with `params`. Spawns the
    /// walker immediately. The search is `Running` until the walker
    /// finishes (`Done`) or is cancelled by `Drop` / unmount.
    pub fn new(
        source_vfs: Arc<dyn Vfs>,
        file_reader: Arc<dyn FileReader>,
        search_root: VfsPath,
        params: SearchParams,
        mount_meta: Vec<u8>,
    ) -> Self {
        let results: Arc<RwLock<Vec<File>>> = Arc::new(RwLock::new(Vec::new()));
        let status = Arc::new(AtomicU8::new(STATUS_RUNNING));
        let notifier = VfsChangeNotifier::new();
        let cancel = CancellationToken::new();
        let hit_count = Arc::new(AtomicUsize::new(0));

        let walker = Walker {
            source_vfs,
            file_reader,
            search_root,
            params,
            results: results.clone(),
            status: status.clone(),
            notifier: notifier.clone(),
            cancel: cancel.clone(),
            hit_count: hit_count.clone(),
        };

        tokio::spawn(walker.run());

        Self {
            mount_meta,
            results,
            status,
            notifier,
            cancel,
            hit_count,
        }
    }

    pub fn status(&self) -> SearchStatus {
        SearchStatus::from_u8(self.status.load(Ordering::Acquire))
    }

    pub fn hit_count(&self) -> usize {
        self.hit_count.load(Ordering::Relaxed)
    }
}

impl Drop for SearchVfs {
    fn drop(&mut self) {
        // Unmount cancels the walker; results map is dropped with the
        // SearchVfs itself.
        self.cancel.cancel();
    }
}

#[async_trait::async_trait]
impl Vfs for SearchVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &SEARCH_VFS_DESCRIPTOR
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.mount_meta.clone()
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
        // SearchVfs is flat: only "/" is a valid listing target. Any
        // sub-path is either a synthetic alias the registry already
        // dereferenced past, or junk.
        if path != Path::new("/") {
            return Err(Error {
                kind: ErrorKind::NotFound,
                message: "search results live at /".into(),
            });
        }

        // Streaming path: keep `list_files` alive until the walker
        // finishes, draining the notifier and emitting fresh snapshots.
        // The navigation layer awaits this future and uses each batch to
        // update the pane progressively; closing the channel by dropping
        // `tx` on completion is what ultimately flips it out of
        // "loading" state.
        if let Some(tx) = batch_tx {
            // The navigation layer accumulates batches by *appending* —
            // see VfsRegistryFs::list_files's caller — so we must emit
            // only the new entries since the last batch, never the full
            // running snapshot. Keep a high-water mark and slice off the
            // tail each time.
            let initial: Vec<File> = self.results.read().clone();
            // First batch (possibly empty) so the navigation layer can
            // clear pending_path and show "loading…" / partial results.
            let _ = tx.send(initial.clone()).await;
            let mut sent_len = initial.len();

            // Drain results until the walker finishes. Order matters:
            // peek the current snapshot first, *then* arm the notifier.
            // Otherwise a notify that fires between our last read and
            // the next watch() is lost and the loop stalls.
            loop {
                let cur_len = self.results.read().len();
                if cur_len > sent_len {
                    let delta: Vec<File> = self.results.read()[sent_len..cur_len].to_vec();
                    if tx.send(delta).await.is_err() {
                        break;
                    }
                    sent_len = cur_len;
                    continue;
                }
                if self.status() != SearchStatus::Running {
                    break;
                }
                // 200ms periodic recheck guards against a race in
                // VfsChangeNotifier: a notify that fires between our
                // last read and re-registering the watcher otherwise
                // gets lost and the loop stalls.
                tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => break,
                    _ = self.notifier.watch(Path::new("/")) => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
                }
            }
            // Final flush — the walker may have queued a few last hits
            // between our last wake and exit. Same delta semantics.
            let final_snap = self.results.read().clone();
            if final_snap.len() > sent_len {
                let delta: Vec<File> = final_snap[sent_len..].to_vec();
                let _ = tx.send(delta).await;
            }
            return Ok(final_snap);
        }

        Ok(self.results.read().clone())
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        // Wake whenever new results arrive (or the walker finishes).
        self.notifier.watch(Path::new("/")).await;
        Ok(())
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn redirect_target(&self, path: &Path) -> Option<VfsPath> {
        // Match by key (= relative path under search root). The path we
        // get is the in-vfs path; for entries it's `/<key>`.
        let key = path.strip_prefix("/").ok()?.to_string_lossy();
        if key.is_empty() {
            return None;
        }
        let key = key.into_owned();
        let results = self.results.read();
        results
            .iter()
            .find(|f| f.key() == key.as_str())
            .and_then(|f| f.source.clone())
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let key = path
            .strip_prefix("/")
            .map_err(|_| Error {
                kind: ErrorKind::NotFound,
                message: "not under search root".into(),
            })?
            .to_string_lossy()
            .into_owned();
        let results = self.results.read();
        results
            .iter()
            .find(|f| f.key() == key.as_str())
            .cloned()
            .ok_or_else(|| Error {
                kind: ErrorKind::NotFound,
                message: format!("no search hit for {}", key),
            })
    }
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

struct Walker {
    source_vfs: Arc<dyn Vfs>,
    file_reader: Arc<dyn FileReader>,
    search_root: VfsPath,
    params: SearchParams,
    results: Arc<RwLock<Vec<File>>>,
    status: Arc<AtomicU8>,
    notifier: VfsChangeNotifier,
    cancel: CancellationToken,
    hit_count: Arc<AtomicUsize>,
}

impl Walker {
    async fn run(self) {
        let final_status = match self.walk().await {
            Ok(_) => STATUS_DONE,
            Err(_) => STATUS_CANCELLED,
        };
        self.status.store(final_status, Ordering::Release);
        // Final wake so any subscribers can publish the closing snapshot.
        self.notifier.notify(Path::new("/"));
    }

    async fn walk(&self) -> Result<(), Error> {
        // Compile the name matcher once; bail loudly if the user gave us
        // garbage rather than silently matching everything.
        let name_glob = match self.params.name_pattern.as_deref() {
            Some(pat) if !pat.is_empty() => Some(compile_glob(pat, self.params.case_sensitive)?),
            _ => None,
        };

        // Stack-based iterative DFS so we don't have to recurse async.
        let mut stack: Vec<PathBuf> = vec![self.search_root.path.clone()];
        let vfs_id = self.search_root.vfs_id;

        while let Some(dir) = stack.pop() {
            if self.cancel.is_cancelled() {
                return Err(Error::cancelled());
            }

            let entries = match self.source_vfs.list_files(&dir, None).await {
                Ok(e) => e,
                Err(e) => {
                    // Log + skip — a single unreadable directory shouldn't
                    // kill the whole walk. (Permission-denied on `/proc/`
                    // is the canonical case.)
                    log::debug!("search walker: list_files {} failed: {}", dir.display(), e);
                    continue;
                }
            };

            for entry in entries {
                if self.cancel.is_cancelled() {
                    return Err(Error::cancelled());
                }
                if entry.name == ".." {
                    continue;
                }
                let entry_path = dir.join(&entry.name);

                // Recurse into directories. We only descend into entries
                // that the source VFS reports as directories — child
                // mount points (archives etc.) live in the registry,
                // not on the source VFS, and are therefore invisible
                // here, which is exactly what we want.
                if entry.is_dir
                    && !entry_path.starts_with("/proc")
                    && (self.params.follow_symlinks || !entry.is_symlink)
                {
                    stack.push(entry_path.clone());
                }

                // Name match.
                if let Some(ref glob) = name_glob
                    && !glob.is_match(&entry.name)
                {
                    continue;
                }

                // Content match: skip files that are too big to scan.
                // Directories never match content (they have no bytes to
                // scan); so when a content filter is set, dirs are
                // implicitly excluded.
                if entry.is_dir && self.params.content_pattern.is_some() {
                    continue;
                }
                if let Some(ref pattern) = self.params.content_pattern {
                    let cap = self.params.content_size_cap;
                    if cap != 0 && entry.size.unwrap_or(0) > cap {
                        continue;
                    }
                    let probe = self
                        .file_reader
                        .find_in_file(
                            VfsPath::new(vfs_id, entry_path.clone()),
                            0,
                            pattern.clone(),
                            if cap == 0 { u64::MAX } else { cap },
                        )
                        .await;
                    match probe {
                        Ok(Some(_)) => {}
                        Ok(None) => continue,
                        Err(e) => {
                            log::debug!(
                                "search walker: find_in_file {} failed: {}",
                                entry_path.display(),
                                e
                            );
                            continue;
                        }
                    }
                }

                // Hit. Build the synthetic File entry.
                let key = relative_key(&self.search_root.path, &entry_path);
                let source = VfsPath::new(vfs_id, entry_path);
                let mut hit = entry.clone();
                hit.key = Some(key);
                hit.source = Some(source);
                {
                    let mut results = self.results.write();
                    results.push(hit);
                    self.hit_count.store(results.len(), Ordering::Relaxed);
                }
                // Coarse-grained wake. The list_files forwarder snapshots
                // the full results vector on each wake, so this is fine
                // even if the walker is much faster than the consumer.
                self.notifier.notify(Path::new("/"));
            }
        }

        Ok(())
    }
}

/// Path under `root` rendered as a forward-slash key. For files at the
/// root itself this is just the basename; for nested matches this looks
/// like `subdir/leaf.rs`. Used as the entry's `File::key`.
fn relative_key(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        // Shouldn't happen — the walker only descends below `root` —
        // but if it does, fall back to the absolute path so the user
        // sees something rather than an empty string.
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

fn compile_glob(pattern: &str, case_sensitive: bool) -> Result<globset::GlobMatcher, Error> {
    let glob = globset::GlobBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .literal_separator(false)
        .build()
        .map_err(|e| Error::custom(format!("invalid glob '{}': {}", pattern, e)))?;
    Ok(glob.compile_matcher())
}

// ---------------------------------------------------------------------------
// Mount helper
// ---------------------------------------------------------------------------

/// Build a `SearchVfs` from a `MountRequest::Search`. Resolves the
/// source root via the registry to compute the human-readable label
/// stamped into `mount_meta`, then spawns the walker.
pub async fn mount(
    root: VfsPath,
    params: SearchParams,
    file_reader: Arc<dyn FileReader>,
    ctx: &crate::api::MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    let (src_vfs, src_path) = ctx.registry.resolve(&root)?;
    let src_meta = src_vfs.mount_meta();
    let display = src_vfs.descriptor().format_path(&src_path, &src_meta);
    let mount_meta = display.into_bytes();

    Ok(Arc::new(SearchVfs::new(
        src_vfs,
        file_reader,
        root,
        params,
        mount_meta,
    )))
}
