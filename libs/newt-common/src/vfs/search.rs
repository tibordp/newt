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
//! - **Rooted at the searched folder.** `origin` is the search root, so
//!   `..`/Backspace at `/` backs out of the results into the directory
//!   the search ran over (`OriginKind::Directory` — unlike an archive's
//!   `Entry` origin, there's nothing to pop past). A synthetic `..` row
//!   leads there too.
//! - **Refinable.** cmd+f inside a search reopens the dialog pre-filled
//!   from `mount_meta` (`VfsDescriptor::search_params`); submitting
//!   mounts a fresh search that *replaces* the pane's current history
//!   entry, so refinements don't stack.
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
//! deferred (archive/S3 native search, tree-view toggle, …).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::vfs::path::{Path, PathBuf};

use parking_lot::RwLock;
use tokio::sync::mpsc;

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

/// Parameters for a search. Captured at mount time and immutable for
/// the lifetime of the `SearchVfs` — refinement mounts a fresh search
/// (cmd+f inside a search reopens the dialog pre-filled from these).
/// Kept in the raw dialog form (not compiled matchers) so they can
/// round-trip through `mount_meta` back into the dialog.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SearchParams {
    /// Glob pattern for the basename (e.g. `*.rs`, `Cargo.*`). When
    /// `None`, matches any name.
    pub name_pattern: Option<String>,
    /// Optional content pattern — runs `FileReader::find_in_file` on
    /// every entry whose name matched. When `None`, name-match alone is
    /// sufficient. A substring unless `content_is_regex` is set.
    pub content_pattern: Option<String>,
    /// Whether `content_pattern` is a regular expression rather than a
    /// literal substring.
    pub content_is_regex: bool,
    /// Whether name and content matching are case-sensitive.
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
            content_is_regex: false,
            case_sensitive: false,
            follow_symlinks: false,
            content_size_cap: 10 * 1024 * 1024, // 10 MiB
        }
    }
}

// Walker status is tracked by the shared `BackgroundJob` (see
// `vfs::background_job`); callers query `SearchVfs::status()` which
// returns a `JobStatus`.

// ---------------------------------------------------------------------------
// SearchVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SearchVfsDescriptor;

/// `mount_meta` carries everything the descriptor needs to render its
/// label / breadcrumbs after the source VFS is unmounted: the search
/// root's pre-formatted display path, plus a one-line summary of the
/// search params (so the user can tell mounts apart at a glance).
/// Encoded as bincode so changes to the format don't accidentally
/// collide with anything path-shaped.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MountMeta {
    /// Source root rendered through the source VFS's `format_path` at
    /// mount time (e.g. `/home/user/projects` or `s3://bucket/prefix`).
    root_display: String,
    /// One-line params summary, e.g. `*.rs` or `*.rs · "TODO"`.
    params_summary: String,
    /// The raw params, so cmd+f inside the search can reopen the dialog
    /// pre-filled (`VfsDescriptor::search_params`).
    params: SearchParams,
}

fn encode_mount_meta(meta: &MountMeta) -> Vec<u8> {
    bincode::serialize(meta).unwrap_or_default()
}

fn decode_mount_meta(bytes: &[u8]) -> MountMeta {
    bincode::deserialize(bytes).unwrap_or_else(|_| MountMeta {
        root_display: String::new(),
        params_summary: String::new(),
        params: SearchParams::default(),
    })
}

fn mount_meta_label_full(meta: &MountMeta) -> String {
    if meta.params_summary.is_empty() {
        meta.root_display.clone()
    } else {
        format!("{} [{}]", meta.root_display, meta.params_summary)
    }
}

/// One-line summary of the active search parameters. Goes into
/// `mount_meta` so the VFS selector / breadcrumbs can show what the
/// search is actually doing.
pub(crate) fn summarize_params(params: &SearchParams) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = params.name_pattern.as_deref().filter(|s| !s.is_empty()) {
        parts.push(name.to_string());
    }
    if let Some(content) = params.content_pattern.as_deref().filter(|s| !s.is_empty()) {
        parts.push(if params.content_is_regex {
            format!("/{}/", content)
        } else {
            format!("\"{}\"", content)
        });
    }
    if params.case_sensitive {
        parts.push("case-sensitive".to_string());
    }
    if params.follow_symlinks {
        parts.push("follow symlinks".to_string());
    }
    parts.join(" · ")
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
    // The origin is the searched folder itself — `..`/Backspace at "/"
    // backs out of the results into the directory the search ran over.
    fn origin_kind(&self) -> super::OriginKind {
        super::OriginKind::Directory
    }
    fn is_ephemeral(&self) -> bool {
        // Searches are scoped to one user action with params captured at
        // mount time — same lifecycle category as archive mounts. Auto-
        // unmounted when no pane (or pane history) references them.
        true
    }
    fn auto_refresh(&self) -> bool {
        false
    }
    fn can_revalidate(&self) -> bool {
        false
    }
    fn can_search(&self) -> bool {
        // Entries are aliases to files in the source VFS; stacking another
        // search on top produces duplicate keys and confuses op routing.
        // cmd+f instead reopens the dialog pre-filled (`search_params`)
        // to refine this search.
        false
    }
    fn search_params(&self, mount_meta: &[u8]) -> Option<SearchParams> {
        bincode::deserialize::<MountMeta>(mount_meta)
            .ok()
            .map(|m| m.params)
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

    fn format_path(&self, _path: &Path, mount_meta: &[u8]) -> String {
        // SearchVfs is flat — there are no meaningful sub-paths; the
        // path component is always "/". Render the root + params summary
        // as a single label. The "Search" prefix is omitted since the
        // VFS selector already shows that we're inside a search VFS.
        mount_meta_label_full(&decode_mount_meta(mount_meta))
    }

    fn breadcrumbs(&self, _path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        // A single non-navigable crumb showing the search root + params.
        // No "Search:" prefix here either — the VFS selector adjacent to
        // the breadcrumbs already conveys that.
        vec![Breadcrumb {
            label: mount_meta_label_full(&decode_mount_meta(mount_meta)),
            nav_path: "/".to_string(),
        }]
    }

    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        // SearchVfs paths don't round-trip — refuse all display-path
        // resolution so the navigate dialog never accidentally drops
        // the user back into a search.
        None
    }

    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        let s = mount_meta_label_full(&decode_mount_meta(mount_meta));
        if s.is_empty() { None } else { Some(s) }
    }
}

pub static SEARCH_VFS_DESCRIPTOR: SearchVfsDescriptor = SearchVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&SEARCH_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// SearchVfs
// ---------------------------------------------------------------------------

/// A search rooted at `search_root` with parameters captured at mount
/// time. The walker runs only while there's at least one streaming
/// `list_files` consumer to observe it — lazily spawned on first
/// consumer, cancelled when the last one goes away. Once the walker
/// finishes naturally (`Done`) the results are frozen and served to
/// any future consumers; the cancellation pathway never resumes a
/// stopped walker (results are partial in that case).
pub struct SearchVfs {
    mount_meta: Vec<u8>,
    /// Accumulated matches keyed by relative path under `search_root`.
    /// Push-only during the walker's lifetime; never mutated afterwards.
    results: Arc<RwLock<Vec<File>>>,
    /// Lifecycle of the walker task — lazy spawn, consumer counting,
    /// cancel-on-zero, status tracking. See `vfs::background_job`.
    job: super::BackgroundJob,
    notifier: VfsChangeNotifier,
    /// Best-effort match counter, refreshed atomically as results stream
    /// in. Used by the walker thread to throttle batch publishes.
    hit_count: Arc<AtomicUsize>,
    /// Walker construction inputs. Kept on `SearchVfs` so we can spawn
    /// the walker lazily — at construction time we don't yet know if
    /// anyone will ever observe this search (the navigation that
    /// triggered the mount could be cancelled before list_files
    /// runs).
    source_vfs: Arc<dyn Vfs>,
    file_reader: Arc<dyn FileReader>,
    search_root: VfsPath,
    params: SearchParams,
    reporter: Arc<dyn super::ProgressReporter>,
}

/// The synthetic `..` row leading back to the searched folder. Not a
/// hit — no `key`/`source` — so it's excluded from redirect/deref and
/// selection like every other VFS's `..` entry.
fn dotdot_entry() -> File {
    File {
        attributes: None,
        name: "..".to_string(),
        size: None,
        allocated_size: None,
        device_id: None,
        inode: None,
        hard_links: None,
        is_dir: true,
        is_hidden: false,
        is_symlink: false,
        symlink_target: None,
        user: None,
        group: None,
        mode: None,
        modified: None,
        accessed: None,
        created: None,
        key: None,
        source: None,
    }
}

impl SearchVfs {
    /// Construct a `SearchVfs`. The walker is *not* spawned here — it
    /// starts on the first streaming `list_files` call. The search is
    /// `Running` until the walker finishes (`Done`), is cancelled by
    /// the last consumer going away (`Cancelled`), or is dropped
    /// (unmount).
    pub fn new(
        source_vfs: Arc<dyn Vfs>,
        file_reader: Arc<dyn FileReader>,
        search_root: VfsPath,
        params: SearchParams,
        mount_meta: Vec<u8>,
        reporter: Arc<dyn super::ProgressReporter>,
    ) -> Self {
        Self {
            mount_meta,
            results: Arc::new(RwLock::new(Vec::new())),
            job: super::BackgroundJob::new(super::RestartPolicy::Sticky),
            notifier: VfsChangeNotifier::new(),
            hit_count: Arc::new(AtomicUsize::new(0)),
            source_vfs,
            file_reader,
            search_root,
            params,
            reporter,
        }
    }

    pub fn status(&self) -> super::JobStatus {
        self.job.status()
    }

    pub fn hit_count(&self) -> usize {
        self.hit_count.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl Vfs for SearchVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &SEARCH_VFS_DESCRIPTOR
    }

    fn origin(&self) -> Option<&VfsPath> {
        Some(&self.search_root)
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.mount_meta.clone()
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::VfsFileList, Error> {
        // SearchVfs is flat: only "/" is a valid listing target. Any
        // sub-path is either a synthetic alias the registry already
        // dereferenced past, or junk.
        if !path.is_root() {
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
            // Acquire a consumer slot. The closure spawns the walker
            // (at most once for this run, per `BackgroundJob`'s lazy
            // semantics). When the returned `ConsumerGuard` drops and
            // we were the last consumer, the walker is cancelled.
            let cancel = self.job.cancel_token();
            let _consumer = self.job.acquire(|handle| {
                let walker = Walker {
                    source_vfs: self.source_vfs.clone(),
                    file_reader: self.file_reader.clone(),
                    search_root: self.search_root.clone(),
                    params: self.params.clone(),
                    results: self.results.clone(),
                    notifier: self.notifier.clone(),
                    hit_count: self.hit_count.clone(),
                    reporter: self.reporter.clone(),
                    job: handle,
                };
                tokio::spawn(walker.run());
            });

            // The navigation layer accumulates batches by *appending* —
            // see VfsRegistryFs::list_files's caller — so we must emit
            // only the new entries since the last batch, never the full
            // running snapshot. Keep a high-water mark and slice off the
            // tail each time.
            let initial: Vec<File> = self.results.read().clone();
            let mut sent_len = initial.len();
            // First batch (possibly just `..`) so the navigation layer can
            // clear pending_path and show "loading…" / partial results.
            let mut first_batch = vec![dotdot_entry()];
            first_batch.extend(initial);
            let _ = tx.send(first_batch).await;

            // Drain results until the walker finishes. Order matters:
            // peek the current snapshot first, *then* arm the notifier.
            // Otherwise a notify that fires between our last read and
            // the next watch() is lost and the loop stalls.
            let watch_root = PathBuf::root();
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
                if self.job.status() != super::JobStatus::Running {
                    break;
                }
                // 200ms periodic recheck guards against a race in
                // VfsChangeNotifier: a notify that fires between our
                // last read and re-registering the watcher otherwise
                // gets lost and the loop stalls.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = self.notifier.watch(&watch_root) => {}
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
            let mut files = vec![dotdot_entry()];
            files.extend(final_snap);
            return Ok(super::VfsFileList {
                files,
                partial: self.job.status() == super::JobStatus::Cancelled,
            });
        }

        let mut files = vec![dotdot_entry()];
        files.extend(self.results.read().iter().cloned());
        Ok(super::VfsFileList {
            files,
            partial: self.job.status() == super::JobStatus::Cancelled,
        })
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        // Wake whenever new results arrive (or the walker finishes).
        self.notifier.watch(&PathBuf::root()).await;
        Ok(())
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn redirect_target(&self, path: &Path) -> Option<VfsPath> {
        // Match by key (= relative path under search root). The path we
        // get is the in-vfs path; for entries it's `/<key>`.
        let key = path.strip_prefix(&PathBuf::root())?;
        if key.is_empty() {
            return None;
        }
        let results = self.results.read();
        results
            .iter()
            .find(|f| f.key() == key)
            .and_then(|f| f.source.clone())
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let key = path.strip_prefix(&PathBuf::root()).ok_or_else(|| Error {
            kind: ErrorKind::NotFound,
            message: "not under search root".into(),
        })?;
        let results = self.results.read();
        results
            .iter()
            .find(|f| f.key() == key)
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
    notifier: VfsChangeNotifier,
    hit_count: Arc<AtomicUsize>,
    reporter: Arc<dyn super::ProgressReporter>,
    job: super::JobHandle,
}

/// How often the walker emits a progress report, regardless of how
/// many entries it's scanned in between. Pure-frontend cadence — the
/// numbers themselves are exact.
const PROGRESS_THROTTLE: std::time::Duration = std::time::Duration::from_millis(200);

impl Walker {
    async fn run(mut self) {
        if self.walk().await.is_ok() {
            // Natural completion — flip the job status to `Done`.
            // Cancellation flows through `BackgroundJob` already; we
            // don't need to set anything in the Err arm.
            self.job.mark_done();
        }
        // Final wake so any subscribers can publish the closing snapshot.
        self.notifier.notify(&PathBuf::root());
        // Clear any lingering progress entry — frontend sees us as
        // done.
        self.reporter.report(None);
    }

    async fn walk(&mut self) -> Result<(), Error> {
        // Compile both matchers once; bail loudly if the user gave us
        // garbage rather than silently matching everything. `mount`
        // already validated these, so failures here are unreachable in
        // practice.
        let name_glob = match self.params.name_pattern.as_deref() {
            Some(pat) if !pat.is_empty() => Some(compile_glob(pat, self.params.case_sensitive)?),
            _ => None,
        };
        let content_pattern = compile_content_pattern(&self.params)?;

        // Stack-based iterative DFS so we don't have to recurse async.
        // VFS `PathBuf`s are always `/`-separated regardless of host OS,
        // so there's no separator-mangling concern.
        let mut stack: Vec<PathBuf> = vec![self.search_root.path.clone()];
        let vfs_id = self.search_root.vfs_id;

        // Progress accounting. `files_scanned` is the running total of
        // entries we've actually looked at. The hit count is already
        // visible to the user via the pane's normal file-count line, so
        // we don't duplicate it here — we report the running scanned
        // total and the current directory being walked, which is the
        // useful "what's it busy with" signal.
        let mut files_scanned: u64 = 0;
        let mut last_report = std::time::Instant::now();
        // Emit an initial zero-count report so the spinner is replaced
        // by a live status line immediately on mount, before the first
        // directory is scanned.
        self.emit_progress(files_scanned, None);

        while let Some(dir_path) = stack.pop() {
            if self.job.is_cancelled() {
                return Err(Error::cancelled());
            }

            // Emit a progress tick whenever we *enter* a new directory,
            // not only after a fixed number of entries — for searches
            // that traverse mostly-empty directories the per-entry path
            // would otherwise rarely update.
            if last_report.elapsed() >= PROGRESS_THROTTLE {
                self.emit_progress(files_scanned, Some(&dir_path));
                last_report = std::time::Instant::now();
            }

            let entries = match self.source_vfs.list_files(&dir_path, None).await {
                Ok(e) => e,
                Err(e) => {
                    // Log + skip — a single unreadable directory shouldn't
                    // kill the whole walk. (Permission-denied on `/proc/`
                    // is the canonical case.)
                    log::debug!("search walker: list_files {} failed: {}", dir_path, e);
                    continue;
                }
            };

            for entry in entries.files {
                if self.job.is_cancelled() {
                    return Err(Error::cancelled());
                }
                if entry.name == ".." {
                    continue;
                }
                files_scanned += 1;
                if last_report.elapsed() >= PROGRESS_THROTTLE {
                    self.emit_progress(files_scanned, Some(&dir_path));
                    last_report = std::time::Instant::now();
                }
                let entry_path = dir_path.join(&entry.name);

                // Recurse into directories. We only descend into entries
                // that the source VFS reports as directories — child
                // mount points (archives etc.) live in the registry,
                // not on the source VFS, and are therefore invisible
                // here, which is exactly what we want.
                let is_proc = entry_path.components().next() == Some("proc");
                if entry.is_dir && !is_proc && (self.params.follow_symlinks || !entry.is_symlink) {
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
                if entry.is_dir && content_pattern.is_some() {
                    continue;
                }
                if let Some(ref pattern) = content_pattern {
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
                                "search walker: find_in_file {:?} failed: {}",
                                entry_path,
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
                self.notifier.notify(&PathBuf::root());
            }
        }

        Ok(())
    }

    /// Build and push a progress snapshot. Cheap — clones the
    /// `Arc<dyn ProgressReporter>`'s `report` call into whatever sink
    /// is wired (no-op in tests, host-state mutation in local mode,
    /// RPC notify in remote mode).
    fn emit_progress(&self, files_scanned: u64, current_dir: Option<&Path>) {
        let mut extra = std::collections::BTreeMap::new();
        if let Some(path) = current_dir {
            // Render through the source VFS's descriptor so what the
            // user sees matches the rest of the app's path formatting
            // (e.g. `s3://bucket/foo` rather than `/foo`).
            let desc = self.source_vfs.descriptor();
            let meta = self.source_vfs.mount_meta();
            extra.insert("path".to_string(), desc.format_path(path, &meta));
        }
        self.reporter.report(Some(super::VfsProgress {
            stage: "Searching".into(),
            processed: Some(files_scanned),
            total: None,
            extra,
        }));
    }
}

/// Segments under `root` rendered as a forward-slash key. For files at
/// the root itself this is just the basename; for nested matches this
/// looks like `subdir/leaf.rs`. Used as the entry's `File::key`.
fn relative_key(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(str::to_string)
        .unwrap_or_else(|| {
            // Shouldn't happen — the walker only descends below `root` —
            // but if it does, fall back to the full path so the user sees
            // something rather than an empty string.
            path.as_wire_str().trim_start_matches('/').to_string()
        })
}

/// Compile the name matcher. A pattern containing any glob meta
/// (`*`, `?`, `[`) is treated as a glob and must match the *whole*
/// basename; anything else is treated as a substring and auto-wrapped
/// to `*pat*`. That matches the "type a fragment, get matches"
/// expectation set by every Find-in-files UI in the wild, while still
/// giving full glob power when the user asks for it.
///
/// Trade-off: literal `?` / `*` / `[` characters in filenames are no
/// longer searchable as substrings without escaping (`\?` etc.); we
/// accept that because filenames containing those characters are
/// vanishingly rare and the substring ergonomics are worth far more.
fn compile_glob(pattern: &str, case_sensitive: bool) -> Result<globset::GlobMatcher, Error> {
    let has_meta = pattern.contains(['*', '?', '[']);
    let effective = if has_meta {
        pattern.to_string()
    } else {
        // Escape any leftover glob-sensitive bytes (shouldn't be any
        // after the `has_meta` check, but cheap insurance) and wrap.
        format!("*{}*", pattern)
    };
    let glob = globset::GlobBuilder::new(&effective)
        .case_insensitive(!case_sensitive)
        .literal_separator(false)
        .build()
        .map_err(|e| Error::custom(format!("invalid glob '{}': {}", pattern, e)))?;
    Ok(glob.compile_matcher())
}

/// Build the `find_in_file` probe pattern from the raw params.
/// `SearchPattern` has no case-sensitivity flag, so insensitivity is
/// encoded inline: `(?i)` on a user regex, or an escaped `(?i)` regex
/// for a case-insensitive literal. Validates user regexes with the same
/// engine `find_in_file` uses (`regex::bytes`).
fn compile_content_pattern(params: &SearchParams) -> Result<Option<SearchPattern>, Error> {
    let Some(raw) = params.content_pattern.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let pattern = if params.content_is_regex {
        let s = if params.case_sensitive {
            raw.to_string()
        } else {
            format!("(?i){}", raw)
        };
        regex::bytes::Regex::new(&s)
            .map_err(|e| Error::custom(format!("invalid regex '{}': {}", raw, e)))?;
        SearchPattern::Regex(s)
    } else if params.case_sensitive {
        SearchPattern::Literal(raw.as_bytes().to_vec())
    } else {
        SearchPattern::Regex(format!("(?i){}", regex::escape(raw)))
    };
    Ok(Some(pattern))
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
    // Validate the patterns up front so garbage fails the mount — and
    // surfaces in the search dialog — rather than erroring inside the
    // walker after the pane has already navigated.
    if let Some(pat) = params.name_pattern.as_deref().filter(|s| !s.is_empty()) {
        compile_glob(pat, params.case_sensitive)?;
    }
    compile_content_pattern(&params)?;

    let (src_vfs, _) = ctx.registry.resolve(&root)?;
    let src_meta = src_vfs.mount_meta();
    let root_display = src_vfs.descriptor().format_path(&root.path, &src_meta);
    let params_summary = summarize_params(&params);
    let mount_meta = encode_mount_meta(&MountMeta {
        root_display,
        params_summary,
        params: params.clone(),
    });

    Ok(Arc::new(SearchVfs::new(
        src_vfs,
        file_reader,
        root,
        params,
        mount_meta,
        ctx.progress_reporter.clone(),
    )))
}
