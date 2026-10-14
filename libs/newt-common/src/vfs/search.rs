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
//! - **Walker.** The shared `walk::walk` over the source VFS, matching
//!   each entry against the compiled `Criteria`: exclusions, hidden and
//!   `.gitignore` prune whole subtrees, then name patterns and metadata
//!   filters, then content via the supplied `Filesystem::find_in_file`.
//!   Mounted child VFSes (archives etc.) are not descended into — they
//!   live in the `VfsRegistry`, not in the source VFS's listings.
//! - **Lifecycle.** A `BackgroundJob`: the walker starts with the first
//!   streaming `list_files` consumer and is cancelled when the last one
//!   goes away (or on unmount); a cancelled run stays partial. Progress
//!   goes through the mount's `ProgressReporter`.
//!
//! See `DESIGN_RECURSIVE_SEARCH.md` for rejected alternatives (pruned
//! tree, `File::Real`/`File::Alias` enum, etc.) and what's deliberately
//! deferred (archive/S3 native search, tree-view toggle, …).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::vfs::path::{Path, PathBuf};

use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::filesystem::Filesystem;
use crate::find::SearchPattern;
use crate::vfs::gitignore::GitIgnores;
use crate::vfs::walk;
use crate::vfs::{File, FsStats, ToUnix, UserGroup};
use crate::{Error, ErrorKind};

use super::{
    Breadcrumb, BreadcrumbDetail, BreadcrumbNote, DisplayPathMatch, RegisteredDescriptor, Vfs,
    VfsChangeNotifier, VfsDescriptor, VfsPath,
};

// ---------------------------------------------------------------------------
// SearchParams
// ---------------------------------------------------------------------------

/// Parameters for a search. Captured at mount time and immutable for
/// the lifetime of the `SearchVfs` — refinement mounts a fresh search
/// (cmd+f inside a search reopens the dialog pre-filled from these).
/// Patterns stay in their raw dialog form (not compiled matchers) so they
/// can round-trip through `mount_meta` back into the dialog.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SearchParams {
    /// `;`-separated name patterns, any of which matches. A fragment
    /// matches anywhere in the name; one with `*`, `?` or `[` is a glob
    /// over the whole name; one with `/` is a glob over the path below
    /// the search root. `None` matches every name.
    pub name_pattern: Option<String>,
    /// `;`-separated patterns of what is neither a result nor searched
    /// into: names, globs, or paths below the root as in `name_pattern`,
    /// but never fragments.
    pub exclude_pattern: Option<String>,
    /// Only files whose content matches are results. A substring unless
    /// `content_is_regex` is set.
    pub content_pattern: Option<String>,
    pub content_is_regex: bool,
    /// Whether name, exclude and content matching are case-sensitive.
    pub case_sensitive: bool,
    /// The content match must start and end at word boundaries.
    pub whole_word: bool,
    /// Files whose content does *not* match are the results instead.
    pub not_containing: bool,
    /// Whether the walker follows symlinks during traversal. Off by
    /// default — symlink loops and double-counting cause more pain than
    /// they save.
    pub follow_symlinks: bool,
    /// Hidden entries are neither results nor searched into.
    pub skip_hidden: bool,
    /// What `.gitignore` files ignore is neither a result nor searched
    /// into, and neither is `.git`.
    pub gitignore: bool,
    /// Mount points below the root are not searched into.
    pub one_file_system: bool,
    /// Packages (apps and other folders macOS presents as files, as their
    /// filesystem says) can be results, but aren't searched into.
    pub skip_packages: bool,
    pub filters: SearchFilters,
    /// Per-file size cap for content search. Files larger than this are
    /// skipped (matched-by-name still surfaces). 0 means unlimited.
    pub content_size_cap: u64,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            name_pattern: None,
            exclude_pattern: None,
            content_pattern: None,
            content_is_regex: false,
            case_sensitive: false,
            whole_word: false,
            not_containing: false,
            follow_symlinks: false,
            skip_hidden: false,
            gitignore: false,
            one_file_system: false,
            skip_packages: false,
            filters: SearchFilters::default(),
            content_size_cap: 10 * 1024 * 1024, // 10 MiB
        }
    }
}

/// What a result must be beyond its name and content. An entry missing
/// the metadata a filter looks at (an S3 object has no owner) passes it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SearchFilters {
    pub kind: EntryKind,
    /// Size bounds, inclusive, in bytes. Directories never match a
    /// search with either.
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified: TimeRange,
    pub accessed: TimeRange,
    pub created: TimeRange,
    /// Owner and group, by name or numeric id.
    pub user: Option<String>,
    pub group: Option<String>,
    pub mode: Option<ModeFilter>,
    /// Windows `FILE_ATTRIBUTE_*` bits a result must all have; 0 for any.
    pub attributes: u32,
    /// Levels below the root searched; 1 is the root's own entries.
    pub max_depth: Option<u32>,
    /// The search stops once it has this many results.
    pub limit: Option<u32>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    #[default]
    Any,
    File,
    Directory,
    /// Only seen as such when symlinks aren't followed; followed, a link
    /// is what it points to.
    Symlink,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TimeRange {
    pub newer_than: Option<TimeBound>,
    pub older_than: Option<TimeBound>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum TimeBound {
    /// That long before the search started.
    Ago { seconds: u64 },
    /// A calendar day as typed, and its start in the user's time zone
    /// in Unix milliseconds — resolved by the dialog, since the walk may
    /// run on a host in another zone.
    Date { date: String, at: i64 },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ModeFilter {
    pub how: ModeMatch,
    /// Permission bits (`0o7777` at most).
    pub bits: u32,
}

/// `find -perm`'s three ways of comparing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ModeMatch {
    Exactly,
    AllOf,
    AnyOf,
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
/// root's pre-formatted display path, plus the params themselves (for the
/// label's suffix, and so cmd+f inside the search can reopen the dialog
/// pre-filled — `VfsDescriptor::search_params`). Encoded as bincode so
/// changes to the format don't accidentally collide with anything
/// path-shaped.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct MountMeta {
    /// Source root rendered through the source VFS's `format_path` at
    /// mount time (e.g. `/home/user/projects` or `s3://bucket/prefix`).
    root_display: String,
    params: SearchParams,
}

fn encode_mount_meta(meta: &MountMeta) -> Vec<u8> {
    bincode::serialize(meta).unwrap_or_default()
}

fn decode_mount_meta(bytes: &[u8]) -> MountMeta {
    bincode::deserialize(bytes).unwrap_or_else(|_| MountMeta {
        root_display: String::new(),
        params: SearchParams::default(),
    })
}

fn mount_meta_label_full(meta: &MountMeta) -> String {
    if meta.root_display.is_empty() {
        return String::new();
    }
    format!("{} {}", meta.root_display, note(&meta.params).text)
}

/// The patterns of a `;`-separated list, trimmed.
fn split_patterns(raw: Option<&str>) -> impl Iterator<Item = &str> {
    raw.unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// The search's name patterns in brackets, marked `†` when it was asked
/// for anything more, which the details spell out.
pub(crate) fn note(params: &SearchParams) -> BreadcrumbNote {
    let names: Vec<&str> = split_patterns(params.name_pattern.as_deref()).collect();
    let names = if names.is_empty() {
        "*".to_string()
    } else {
        names.join("; ")
    };
    let mut details = vec![detail("Name", names.clone())];
    details.extend(more_details(params));
    let more = if details.len() > 1 { "†" } else { "" };
    BreadcrumbNote {
        text: format!("[{names}]{more}"),
        details,
    }
}

fn detail(label: &str, value: impl Into<String>) -> BreadcrumbDetail {
    BreadcrumbDetail {
        label: label.to_string(),
        value: value.into(),
    }
}

/// Everything but the name patterns that sets the search apart from one
/// with the defaults.
fn more_details(params: &SearchParams) -> Vec<BreadcrumbDetail> {
    let mut details = Vec::new();
    let excludes: Vec<&str> = split_patterns(params.exclude_pattern.as_deref()).collect();
    if !excludes.is_empty() {
        details.push(detail("Exclude", excludes.join("; ")));
    }
    if let Some(content) = params.content_pattern.as_deref().filter(|s| !s.is_empty()) {
        let mut value = if params.content_is_regex {
            format!("/{content}/")
        } else {
            format!("\"{content}\"")
        };
        if params.whole_word {
            value.push_str(", whole word");
        }
        if params.not_containing {
            value.push_str(", not containing");
        }
        details.push(detail("Content", value));
    }

    let f = &params.filters;
    match f.kind {
        EntryKind::Any => {}
        EntryKind::File => details.push(detail("Type", "files")),
        EntryKind::Directory => details.push(detail("Type", "folders")),
        EntryKind::Symlink => details.push(detail("Type", "symlinks")),
    }
    let size: Vec<String> = [
        f.min_size.map(|b| format!("at least {}", format_size(b))),
        f.max_size.map(|b| format!("at most {}", format_size(b))),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !size.is_empty() {
        details.push(detail("Size", size.join(", ")));
    }
    for (label, range) in [
        ("Modified", &f.modified),
        ("Accessed", &f.accessed),
        ("Created", &f.created),
    ] {
        let bounds: Vec<String> = [
            range.newer_than.as_ref().map(|b| match b {
                TimeBound::Ago { seconds } => format!("within {}", format_duration(*seconds)),
                TimeBound::Date { date, .. } => format!("since {date}"),
            }),
            range.older_than.as_ref().map(|b| match b {
                TimeBound::Ago { seconds } => format!("over {} ago", format_duration(*seconds)),
                TimeBound::Date { date, .. } => format!("before {date}"),
            }),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !bounds.is_empty() {
            details.push(detail(label, bounds.join(", ")));
        }
    }
    if let Some(user) = &f.user {
        details.push(detail("Owner", user.clone()));
    }
    if let Some(group) = &f.group {
        details.push(detail("Group", group.clone()));
    }
    if let Some(mode) = &f.mode {
        let how = match mode.how {
            ModeMatch::Exactly => "exactly",
            ModeMatch::AllOf => "all of",
            ModeMatch::AnyOf => "any of",
        };
        details.push(detail("Permissions", format!("{how} {:04o}", mode.bits)));
    }
    if f.attributes != 0 {
        let names: Vec<&str> = WINDOWS_ATTRIBUTES
            .iter()
            .filter(|(bit, _)| f.attributes & bit != 0)
            .map(|(_, name)| *name)
            .collect();
        details.push(detail("Attributes", names.join(", ")));
    }
    if let Some(depth) = f.max_depth {
        let levels = if depth == 1 { "level" } else { "levels" };
        details.push(detail("Depth", format!("{depth} {levels}")));
    }
    if let Some(limit) = f.limit {
        details.push(detail("Limit", format!("{limit} results")));
    }

    let options: Vec<&str> = [
        (params.case_sensitive, "case-sensitive"),
        (params.follow_symlinks, "follows symlinks"),
        (params.skip_hidden, "skips hidden"),
        (params.gitignore, "honours .gitignore"),
        (params.one_file_system, "one file system"),
        (params.skip_packages, "skips package contents"),
    ]
    .into_iter()
    .filter_map(|(on, label)| on.then_some(label))
    .collect();
    if !options.is_empty() {
        details.push(detail("Options", options.join(", ")));
    }
    details
}

/// The Windows attributes a search can ask for, by `FILE_ATTRIBUTE_*` bit.
const WINDOWS_ATTRIBUTES: [(u32, &str); 4] = [
    (0x1, "read-only"),
    (0x2, "hidden"),
    (0x4, "system"),
    (0x20, "archive"),
];

/// In the largest unit that's exact, as the dialog writes it.
fn format_size(bytes: u64) -> String {
    const UNITS: [(u64, &str); 8] = [
        (1_000_000_000_000, "TB"),
        (1 << 40, "TiB"),
        (1_000_000_000, "GB"),
        (1 << 30, "GiB"),
        (1_000_000, "MB"),
        (1 << 20, "MiB"),
        (1_000, "kB"),
        (1 << 10, "KiB"),
    ];
    UNITS
        .iter()
        .find(|(unit, _)| bytes >= *unit && bytes.is_multiple_of(*unit))
        .map_or_else(
            || format!("{bytes} B"),
            |(unit, name)| format!("{} {name}", bytes / unit),
        )
}

/// In the largest unit that's exact.
fn format_duration(seconds: u64) -> String {
    const UNITS: [(u64, &str); 7] = [
        (365 * 86_400, "year"),
        (30 * 86_400, "month"),
        (7 * 86_400, "week"),
        (86_400, "day"),
        (3_600, "hour"),
        (60, "minute"),
        (1, "second"),
    ];
    let (unit, name) = UNITS
        .iter()
        .find(|(unit, _)| seconds >= *unit && seconds.is_multiple_of(*unit))
        .unwrap_or(&(1, "second"));
    let n = seconds / unit;
    format!("{n} {name}{}", if n == 1 { "" } else { "s" })
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
    fn can_read(&self) -> bool {
        false
    }
    fn can_overwrite(&self) -> bool {
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
    fn can_write_range(&self) -> bool {
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
        // SearchVfs is flat — there are no meaningful sub-paths. The
        // searched folder is what Go to Path and Copy Path should see:
        // the patterns are the breadcrumb's note, not part of a path.
        decode_mount_meta(mount_meta).root_display
    }

    fn breadcrumbs(&self, _path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        // A single crumb: the search root, with the patterns as its note.
        // No "Search:" prefix — the VFS selector adjacent to the
        // breadcrumbs already conveys that.
        let meta = decode_mount_meta(mount_meta);
        vec![Breadcrumb {
            label: meta.root_display,
            nav_path: "/".to_string(),
            note: Some(note(&meta.params)),
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
    /// Why the results are incomplete, once the walker could not read
    /// something; the status bar's `(partial)` reason.
    incomplete: Arc<parking_lot::Mutex<Option<String>>>,
    /// Walker construction inputs. Kept on `SearchVfs` so we can spawn
    /// the walker lazily — at construction time we don't yet know if
    /// anyone will ever observe this search (the navigation that
    /// triggered the mount could be cancelled before list_files
    /// runs).
    source_vfs: Arc<dyn Vfs>,
    file_reader: Arc<dyn Filesystem>,
    search_root: VfsPath,
    params: SearchParams,
    reporter: Arc<dyn super::ProgressReporter>,
}

/// The synthetic `..` row leading back to the searched folder. Not a
/// hit — no `key`/`source` — so it's excluded from redirect/deref and
/// selection like every other VFS's `..` entry.
impl SearchVfs {
    /// Construct a `SearchVfs`. The walker is *not* spawned here — it
    /// starts on the first streaming `list_files` call. The search is
    /// `Running` until the walker finishes (`Done`), is cancelled by
    /// the last consumer going away (`Cancelled`), or is dropped
    /// (unmount).
    pub fn new(
        source_vfs: Arc<dyn Vfs>,
        file_reader: Arc<dyn Filesystem>,
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
            incomplete: Arc::new(parking_lot::Mutex::new(None)),
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

    fn partial(&self) -> Option<String> {
        if self.job.status() == super::JobStatus::Cancelled {
            return Some("cancelled".to_string());
        }
        self.incomplete.lock().clone()
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
                    incomplete: self.incomplete.clone(),
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
            let mut first_batch = vec![File::parent_dir()];
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
            let mut files = vec![File::parent_dir()];
            files.extend(final_snap);
            return Ok(super::VfsFileList {
                files,
                partial: self.partial(),
            });
        }

        let mut files = vec![File::parent_dir()];
        files.extend(self.results.read().iter().cloned());
        Ok(super::VfsFileList {
            files,
            partial: self.partial(),
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
    file_reader: Arc<dyn Filesystem>,
    search_root: VfsPath,
    params: SearchParams,
    results: Arc<RwLock<Vec<File>>>,
    notifier: VfsChangeNotifier,
    hit_count: Arc<AtomicUsize>,
    incomplete: Arc<parking_lot::Mutex<Option<String>>>,
    reporter: Arc<dyn super::ProgressReporter>,
    job: super::JobHandle,
}

/// How often the walker emits a progress report, regardless of how
/// many entries it's scanned in between. Pure-frontend cadence — the
/// numbers themselves are exact. Zero under test so the reporting is
/// exercised deterministically instead of by racing a wall clock.
#[cfg(not(test))]
const PROGRESS_THROTTLE: std::time::Duration = std::time::Duration::from_millis(200);
#[cfg(test)]
const PROGRESS_THROTTLE: std::time::Duration = std::time::Duration::ZERO;

impl Walker {
    async fn run(self) {
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

    async fn walk(&self) -> Result<(), Error> {
        // `mount` already validated the patterns, so failures here are
        // unreachable in practice.
        let criteria = Criteria::compile(&self.params)?;

        // Emit an initial zero-count report so the spinner is replaced
        // by a live status line immediately on mount, before the first
        // directory is scanned.
        self.emit_progress(0, None, 0);

        let ignores = if self.params.gitignore {
            Some(GitIgnores::above(&*self.source_vfs, &self.search_root.path).await)
        } else {
            None
        };
        let mut matcher = Matcher {
            walker: self,
            criteria,
            ignores,
            now: std::time::SystemTime::now().to_unix(),
            files_scanned: 0,
            skipped: 0,
            last_report: std::time::Instant::now(),
            root_prefix: self
                .search_root
                .path
                .file_name()
                .map_or(0, |name| name.len() + 1),
        };
        // The root is the directory the user is looking at, reached
        // through a link or not.
        let options = walk::WalkOptions {
            follow_symlinks: self.params.follow_symlinks,
            follow_root: true,
            one_file_system: self.params.one_file_system,
            excludes: vec![PathBuf::from_wire_str("/proc")],
        };
        walk::walk(
            &*self.source_vfs,
            std::slice::from_ref(&self.search_root.path),
            &options,
            &mut matcher,
        )
        .await
    }

    /// Build and push a progress snapshot. Cheap — clones the
    /// `Arc<dyn ProgressReporter>`'s `report` call into whatever sink
    /// is wired (no-op in tests, host-state mutation in local mode,
    /// RPC notify in remote mode).
    fn emit_progress(&self, files_scanned: u64, current_dir: Option<&Path>, skipped: u64) {
        let mut extra = std::collections::BTreeMap::new();
        if skipped > 0 {
            extra.insert("skipped".to_string(), skipped.to_string());
        }
        if let Some(path) = current_dir {
            // Relative to the search root, like the result rows: the root
            // itself is already named in the pane header, and an absolute
            // path would crowd everything else out of a one-line status
            // bar. The root directory has no relative form — it reports
            // the running count alone.
            let rel = relative_key(&self.search_root.path, path);
            if !rel.is_empty() {
                extra.insert("path".to_string(), rel);
            }
        }
        self.reporter.report(Some(super::VfsProgress {
            stage: "Searching".into(),
            processed: Some(files_scanned),
            total: None,
            extra,
        }));
    }
}

/// The walk's visitor: matches each entry against the search and
/// publishes the hits.
struct Matcher<'a> {
    walker: &'a Walker,
    criteria: Criteria,
    /// With `gitignore` set.
    ignores: Option<GitIgnores>,
    /// When the walk started, in Unix milliseconds: what relative time
    /// bounds count back from.
    now: i64,
    /// Entries looked at. The hit count is already visible to the user
    /// via the pane's normal file-count line, so progress reports the
    /// running scanned total and the directory being walked instead.
    files_scanned: u64,
    /// Directories and links the walk could not read and passed over.
    skipped: u64,
    last_report: std::time::Instant,
    /// Length of the root's own name plus `/` at the front of every
    /// `rel`; what follows is the hit's key.
    root_prefix: usize,
}

impl Matcher<'_> {
    /// Whether the entry is left out together with everything under it.
    fn pruned(&self, name: &str, key: &str, entry: &walk::Entry<'_>) -> bool {
        let params = &self.walker.params;
        (params.skip_hidden && entry.file.is_hidden)
            || self.criteria.exclude.is_match(name, key)
            || self.ignores.as_ref().is_some_and(|ignores| {
                name == ".git" || ignores.ignored(entry.path, entry.file.is_dir)
            })
    }

    /// The content verdict on a file that passed everything else.
    async fn content_matches(&self, entry: &walk::Entry<'_>, pattern: &SearchPattern) -> bool {
        let walker = self.walker;
        // Directories have no bytes to scan, and FIFOs and devices would
        // never stop giving them.
        if entry.file.is_dir || entry.file.is_special() {
            return false;
        }
        let cap = walker.params.content_size_cap;
        if cap != 0 && entry.file.size.unwrap_or(0) > cap {
            return false;
        }
        let probe = walker
            .file_reader
            .find_in_file(
                VfsPath::new(walker.search_root.vfs_id, entry.path.to_owned()),
                0,
                pattern.clone(),
                if cap == 0 { u64::MAX } else { cap },
            )
            .await;
        match probe {
            Ok(found) => found.is_some() != walker.params.not_containing,
            Err(e) => {
                log::debug!("search walker: find_in_file {:?} failed: {}", entry.path, e);
                false
            }
        }
    }
}

#[async_trait::async_trait]
impl walk::Visitor for Matcher<'_> {
    async fn entry(&mut self, entry: walk::Entry<'_>) -> Result<walk::Control, Error> {
        let walker = self.walker;
        if walker.job.is_cancelled() {
            return Err(Error::cancelled());
        }
        // The root is what is searched, not a result.
        if entry.depth == 0 {
            return Ok(walk::Control::Descend);
        }
        self.files_scanned += 1;
        // A tick on entering a directory as well as per entry: a search
        // through mostly-empty directories would otherwise rarely update.
        if self.last_report.elapsed() >= PROGRESS_THROTTLE {
            let dir = if entry.file.is_dir {
                entry.path
            } else {
                entry.path.parent().unwrap_or(entry.path)
            };
            walker.emit_progress(self.files_scanned, Some(dir), self.skipped);
            self.last_report = std::time::Instant::now();
        }

        // The name as traversed: a followed link's own name, not its
        // target's.
        let name = entry.rel.rsplit('/').next().unwrap_or(&entry.file.name);
        let key = &entry.rel[self.root_prefix..];
        if self.pruned(name, key, &entry) {
            return Ok(walk::Control::Skip);
        }
        let below = match self.criteria.filters.max_depth {
            Some(max) if entry.depth >= max as usize => walk::Control::Skip,
            _ if walker.params.skip_packages
                && entry.file.is_dir
                && walker
                    .source_vfs
                    .is_package(entry.path)
                    .await
                    .unwrap_or(false) =>
            {
                walk::Control::Skip
            }
            _ => walk::Control::Descend,
        };

        let names = &self.criteria.names;
        if !(names.is_empty() || names.is_match(name, key))
            || !self.criteria.filters_pass(entry.file, self.now)
        {
            return Ok(below);
        }
        if let Some(pattern) = &self.criteria.content
            && !self.content_matches(&entry, pattern).await
        {
            return Ok(below);
        }

        // Hit. Build the synthetic File entry.
        let mut hit = entry.file.clone();
        hit.name = name.to_string();
        hit.key = Some(key.to_string());
        hit.source = Some(VfsPath::new(
            walker.search_root.vfs_id,
            entry.path.to_owned(),
        ));
        let hits = {
            let mut results = walker.results.write();
            results.push(hit);
            walker.hit_count.store(results.len(), Ordering::Relaxed);
            results.len()
        };
        // Coarse-grained wake. The list_files forwarder snapshots
        // the full results vector on each wake, so this is fine
        // even if the walker is much faster than the consumer.
        walker.notifier.notify(&PathBuf::root());
        if let Some(limit) = self.criteria.filters.limit
            && hits >= limit as usize
        {
            *walker.incomplete.lock() = Some(format!("stopped at {limit} results"));
            return Ok(walk::Control::Stop);
        }
        Ok(below)
    }

    async fn listed(&mut self, path: &Path, children: &[File]) -> Result<(), Error> {
        if let Some(ignores) = &mut self.ignores {
            ignores
                .learn(&*self.walker.source_vfs, path, children)
                .await;
        }
        Ok(())
    }

    /// The root failing is the whole search failing, worded as such; a
    /// subtree failing is counted and passed over.
    async fn failed(
        &mut self,
        what: walk::Failed<'_>,
        error: Error,
    ) -> Result<walk::Resume, Error> {
        let walker = self.walker;
        let path = match what {
            walk::Failed::Listing(path) | walk::Failed::Following(path) => path,
        };
        let reason = if path.as_wire_str() == walker.search_root.path.as_wire_str() {
            format!("cannot list {path}: {error}")
        } else {
            self.skipped += 1;
            format!(
                "{} {} could not be read",
                self.skipped,
                if self.skipped == 1 {
                    "entry"
                } else {
                    "entries"
                }
            )
        };
        *walker.incomplete.lock() = Some(reason);
        Ok(walk::Resume::Skip)
    }
}

/// Segments under `root` rendered as a forward-slash key. For files at
/// the root itself this is just the basename; for nested matches this
/// looks like `subdir/leaf.rs`.
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

/// The search, compiled.
struct Criteria {
    names: Patterns,
    exclude: Patterns,
    content: Option<SearchPattern>,
    filters: SearchFilters,
}

impl Criteria {
    fn compile(params: &SearchParams) -> Result<Self, Error> {
        Ok(Self {
            names: Patterns::compile(params.name_pattern.as_deref(), params.case_sensitive, true)?,
            exclude: Patterns::compile(
                params.exclude_pattern.as_deref(),
                params.case_sensitive,
                false,
            )?,
            content: compile_content_pattern(params)?,
            filters: params.filters.clone(),
        })
    }

    /// Whether `file` passes the filters; what it lacks the metadata for
    /// passes.
    fn filters_pass(&self, file: &File, now: i64) -> bool {
        let f = &self.filters;
        let kind = match f.kind {
            EntryKind::Any => true,
            EntryKind::File => !file.is_dir && !file.is_symlink,
            EntryKind::Directory => file.is_dir,
            EntryKind::Symlink => file.is_symlink,
        };
        let size = (f.min_size.is_none() && f.max_size.is_none())
            || (!file.is_dir
                && file.size.is_none_or(|size| {
                    f.min_size.is_none_or(|min| size >= min)
                        && f.max_size.is_none_or(|max| size <= max)
                }));
        let times = [
            (&f.modified, file.modified),
            (&f.accessed, file.accessed),
            (&f.created, file.created),
        ]
        .into_iter()
        .all(|(range, time)| time.is_none_or(|time| in_range(range, time, now)));
        let owner = [(&f.user, &file.user), (&f.group, &file.group)]
            .into_iter()
            .all(|(wanted, has)| match (wanted, has) {
                (Some(wanted), Some(UserGroup::Name(name))) => name == wanted,
                (Some(wanted), Some(UserGroup::Id(id))) => wanted.parse() == Ok(*id),
                _ => true,
            });
        let mode = match (&f.mode, &file.mode) {
            (Some(filter), Some(mode)) => {
                let bits = mode.0 & 0o7777;
                match filter.how {
                    ModeMatch::Exactly => bits == filter.bits,
                    ModeMatch::AllOf => bits & filter.bits == filter.bits,
                    ModeMatch::AnyOf => filter.bits == 0 || bits & filter.bits != 0,
                }
            }
            _ => true,
        };
        let attributes = file
            .attributes
            .is_none_or(|has| has & f.attributes == f.attributes);
        kind && size && times && owner && mode && attributes
    }
}

fn in_range(range: &TimeRange, time: i64, now: i64) -> bool {
    let at = |bound: &TimeBound| match bound {
        TimeBound::Ago { seconds } => {
            now.saturating_sub(i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX))
        }
        TimeBound::Date { at, .. } => *at,
    };
    range.newer_than.as_ref().is_none_or(|b| time >= at(b))
        && range.older_than.as_ref().is_none_or(|b| time < at(b))
}

/// A `;`-separated pattern list, compiled: each pattern either over the
/// name or, when it has a `/`, over the path below the search root.
struct Patterns {
    names: globset::GlobSet,
    paths: globset::GlobSet,
}

impl Patterns {
    /// With `fragments`, a pattern without glob meta matches anywhere in
    /// the name (`*pat*`) — the "type a fragment, get matches" of every
    /// Find-in-files UI. Literal `*`, `?` and `[` in names then need
    /// escaping (`\?`), which is worth it.
    fn compile(raw: Option<&str>, case_sensitive: bool, fragments: bool) -> Result<Self, Error> {
        let mut names = globset::GlobSetBuilder::new();
        let mut paths = globset::GlobSetBuilder::new();
        for pattern in split_patterns(raw) {
            let path = pattern.contains('/');
            let effective = if path {
                pattern.trim_start_matches('/').to_string()
            } else if fragments && !pattern.contains(['*', '?', '[']) {
                format!("*{pattern}*")
            } else {
                pattern.to_string()
            };
            let glob = globset::GlobBuilder::new(&effective)
                .case_insensitive(!case_sensitive)
                .literal_separator(true)
                .build()
                .map_err(|e| Error::custom(format!("invalid pattern '{pattern}': {e}")))?;
            if path {
                paths.add(glob);
            } else {
                names.add(glob);
            }
        }
        let build = |set: globset::GlobSetBuilder| {
            set.build()
                .map_err(|e| Error::custom(format!("invalid pattern: {e}")))
        };
        Ok(Self {
            names: build(names)?,
            paths: build(paths)?,
        })
    }

    fn is_empty(&self) -> bool {
        self.names.is_empty() && self.paths.is_empty()
    }

    fn is_match(&self, name: &str, key: &str) -> bool {
        self.names.is_match(name) || self.paths.is_match(key)
    }
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
    if params.content_is_regex {
        regex::bytes::Regex::new(raw)
            .map_err(|e| Error::custom(format!("invalid regex '{raw}': {e}")))?;
    }
    if params.case_sensitive && !params.content_is_regex && !params.whole_word {
        return Ok(Some(SearchPattern::Literal(raw.as_bytes().to_vec())));
    }
    let mut pattern = if params.content_is_regex {
        raw.to_string()
    } else {
        regex::escape(raw)
    };
    if params.whole_word {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    if !params.case_sensitive {
        pattern = format!("(?i){pattern}");
    }
    Ok(Some(SearchPattern::Regex(pattern)))
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
    file_reader: Arc<dyn Filesystem>,
    ctx: &crate::vfs::mount::MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    // Validate the patterns up front so garbage fails the mount — and
    // surfaces in the search dialog — rather than erroring inside the
    // walker after the pane has already navigated.
    Criteria::compile(&params)?;

    let (src_vfs, _) = ctx.registry.resolve(&root)?;
    let src_meta = src_vfs.mount_meta();
    let root_display = src_vfs.descriptor().format_path(&root.path, &src_meta);
    let mount_meta = encode_mount_meta(&MountMeta {
        root_display,
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

/// The walker is the only thing that knows a long search is alive: until
/// the first hit there is nothing to put in the pane. These cover the
/// signal it emits meanwhile.
#[cfg(test)]
mod progress_tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use crate::test_support::{FailureSpec, MockVfs, MockVfsBuilder};
    use crate::vfs::path::PathBuf;
    use crate::vfs::search::{SearchParams, SearchVfs};
    use crate::vfs::{ProgressReporter, Vfs, VfsId, VfsPath, VfsProgress, VfsRegistry};

    #[derive(Default)]
    struct Capture(Mutex<Vec<Option<VfsProgress>>>);

    impl ProgressReporter for Capture {
        fn report(&self, progress: Option<VfsProgress>) {
            self.0.lock().push(progress);
        }
    }

    /// Run a search that matches nothing over a small tree, and return
    /// every progress report it emitted.
    async fn reports_for_a_fruitless_search() -> Vec<Option<VfsProgress>> {
        let mut b = MockVfs::builder();
        for d in 0..3 {
            b = b.dir(&format!("/dir{d}"));
            for f in 0..3 {
                b = b.file(&format!("/dir{d}/f{f}.txt"), b"content");
            }
        }
        let source = b.build();
        let registry = Arc::new(VfsRegistry::with_root(source.clone()));
        let reader = Arc::new(crate::vfs::VfsRegistryFs::new(registry));
        let capture = Arc::new(Capture::default());

        let vfs = SearchVfs::new(
            source,
            reader,
            VfsPath::new(VfsId::ROOT, PathBuf::root()),
            SearchParams {
                name_pattern: Some("*matches-nothing*".into()),
                ..Default::default()
            },
            Vec::new(),
            capture.clone(),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let _ = vfs.list_files(&PathBuf::root(), Some(tx)).await;
        let _ = drain.await;

        capture.0.lock().clone()
    }

    #[tokio::test]
    async fn a_search_that_finds_nothing_still_reports_where_it_is() {
        let reports = reports_for_a_fruitless_search().await;
        let with_path: Vec<&std::collections::BTreeMap<String, String>> = reports
            .iter()
            .flatten()
            .map(|p| &p.extra)
            .filter(|e| e.contains_key("path"))
            .collect();

        assert!(
            !with_path.is_empty(),
            "the walker never said which directory it was in — a long search \
             with no hits looks hung"
        );
        for extra in with_path {
            let path = &extra["path"];
            // Relative to the search root: the root is already named in the
            // pane header, and an absolute path crowds out the status bar.
            assert!(
                !path.starts_with('/'),
                "progress path should be relative to the search root, got {path}"
            );
            assert!(path.starts_with("dir"), "unexpected progress path {path}");
        }
    }

    #[tokio::test]
    async fn progress_carries_a_running_count_and_is_cleared_at_the_end() {
        let reports = reports_for_a_fruitless_search().await;

        assert!(
            reports.iter().flatten().all(|p| p.stage == "Searching"),
            "every report should name its stage"
        );
        // `processed` must always be set: a counter-less report is treated
        // as a mount-log line by the host sink, and a search would spam it.
        assert!(reports.iter().flatten().all(|p| p.processed.is_some()));

        let scanned: Vec<u64> = reports
            .iter()
            .flatten()
            .filter_map(|p| p.processed)
            .collect();
        assert!(
            scanned.windows(2).all(|w| w[1] >= w[0]),
            "the scanned count should never go backwards: {scanned:?}"
        );
        assert!(
            matches!(reports.last(), Some(None)),
            "the walker must clear its progress when it finishes"
        );
    }

    fn unreadable(path: &str) -> FailureSpec {
        FailureSpec {
            path: PathBuf::from_wire_str(path),
            operation: "list_files",
            error: crate::Error::custom("denied"),
            remaining: None,
        }
    }

    /// Search everything under `root`, let the walker finish, and return
    /// the final listing.
    async fn search_all(builder: MockVfsBuilder, root: &str) -> crate::vfs::VfsFileList {
        let source = builder.build();
        let registry = Arc::new(VfsRegistry::with_root(source.clone()));
        let reader = Arc::new(crate::vfs::VfsRegistryFs::new(registry));
        let vfs = SearchVfs::new(
            source,
            reader,
            VfsPath::new(VfsId::ROOT, PathBuf::from_wire_str(root)),
            SearchParams::default(),
            Vec::new(),
            Arc::new(Capture::default()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let _ = vfs.list_files(&PathBuf::root(), Some(tx)).await;
        let _ = drain.await;
        vfs.list_files(&PathBuf::root(), None).await.unwrap()
    }

    #[tokio::test]
    async fn an_unreadable_root_is_an_empty_listing_that_says_why() {
        let list = search_all(
            MockVfs::builder()
                .file("/dir/a.txt", b"")
                .failure(unreadable("/dir")),
            "/dir",
        )
        .await;
        let names: Vec<&str> = list.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, [".."]);
        assert_eq!(list.partial.as_deref(), Some("cannot list /dir: denied"));
    }

    #[tokio::test]
    async fn unreadable_subtrees_are_passed_over_and_counted() {
        let list = search_all(
            MockVfs::builder()
                .file("/dir/a.txt", b"")
                .file("/dir/bad/x", b"")
                .file("/dir/worse/y", b"")
                .file("/dir/ok/z", b"")
                .failure(unreadable("/dir/bad"))
                .failure(unreadable("/dir/worse")),
            "/dir",
        )
        .await;
        let names: Vec<&str> = list.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["..", "a.txt", "bad", "ok", "z", "worse"]);
        assert_eq!(list.partial.as_deref(), Some("2 entries could not be read"));

        let list = search_all(MockVfs::builder().file("/dir/a.txt", b""), "/dir").await;
        assert_eq!(list.partial, None);
    }
}

#[cfg(all(test, unix))]
mod root_tests {
    use std::sync::Arc;

    use crate::vfs::path::PathBuf;
    use crate::vfs::search::{SearchParams, SearchVfs};
    use crate::vfs::{LocalVfs, Vfs, VfsId, VfsPath, VfsRegistry};

    /// A pane inside a directory reached through a link searches that
    /// directory, and the hits are keyed below it.
    #[tokio::test]
    async fn a_linked_root_is_searched() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        std::fs::write(real.join("sub/needle.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();

        let source: Arc<dyn Vfs> = Arc::new(LocalVfs::new());
        let registry = Arc::new(VfsRegistry::with_root(source.clone()));
        let reader = Arc::new(crate::vfs::VfsRegistryFs::new(registry));
        let root = PathBuf::from_native(&dir.path().join("link"));
        let vfs = SearchVfs::new(
            source,
            reader,
            VfsPath::new(VfsId::ROOT, root),
            SearchParams {
                name_pattern: Some("needle".into()),
                ..Default::default()
            },
            Vec::new(),
            Arc::new(crate::vfs::ScopedReporter::new(
                Arc::new(crate::vfs::NoopProgressSink),
                VfsId(1),
            )),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let listing = vfs.list_files(&PathBuf::root(), Some(tx)).await.unwrap();
        drain.await.unwrap();
        let keys: Vec<_> = listing
            .files
            .iter()
            .filter_map(|f| f.key.as_deref())
            .collect();
        assert_eq!(keys, ["sub/needle.txt"]);
    }
}

#[cfg(test)]
mod criteria_tests {
    use std::sync::Arc;

    use crate::test_support::{MockVfs, MockVfsBuilder};
    use crate::vfs::path::PathBuf;
    use crate::vfs::search::{
        Criteria, EntryKind, ModeFilter, ModeMatch, SearchFilters, SearchParams, SearchVfs,
        TimeBound, TimeRange,
    };
    use crate::vfs::{File, NoopProgressSink, ScopedReporter, ToUnix, UserGroup, Vfs, VfsId};
    use crate::vfs::{VfsPath, VfsRegistry};

    /// The keys of what a search of `/root` finds, sorted.
    async fn hits(builder: MockVfsBuilder, params: SearchParams) -> Vec<String> {
        let source = builder.build();
        let registry = Arc::new(VfsRegistry::with_root(source.clone()));
        let reader = Arc::new(crate::vfs::VfsRegistryFs::new(registry));
        let vfs = SearchVfs::new(
            source,
            reader,
            VfsPath::new(VfsId::ROOT, PathBuf::from_wire_str("/root")),
            params,
            Vec::new(),
            Arc::new(ScopedReporter::new(Arc::new(NoopProgressSink), VfsId(1))),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let listing = vfs.list_files(&PathBuf::root(), Some(tx)).await.unwrap();
        drain.await.unwrap();
        let mut keys: Vec<String> = listing.files.into_iter().filter_map(|f| f.key).collect();
        keys.sort();
        keys
    }

    fn names(pattern: &str) -> SearchParams {
        SearchParams {
            name_pattern: Some(pattern.into()),
            ..Default::default()
        }
    }

    fn tree() -> MockVfsBuilder {
        MockVfs::builder()
            .file("/root/main.rs", b"fn main() { todo!() }")
            .file("/root/Cargo.toml", b"[package]")
            .file("/root/notes.txt", b"TODO: everything")
            .file("/root/latest.rs", b"")
            .file("/root/src/gen/out.rs", b"")
            .file("/root/test/case.rs", b"")
            .file("/root/node_modules/dep/index.rs", b"")
    }

    #[tokio::test]
    async fn any_of_several_patterns_matches_and_a_slash_matches_the_path() {
        let found = hits(tree(), names("*.toml; main")).await;
        assert_eq!(found, ["Cargo.toml", "main.rs"]);

        let found = hits(tree(), names("src/**/*.rs")).await;
        assert_eq!(found, ["src/gen/out.rs"]);
    }

    #[tokio::test]
    async fn exclusions_are_whole_names_and_are_not_searched_into() {
        let params = SearchParams {
            exclude_pattern: Some("node_modules; test; src/gen".into()),
            ..names("*.rs")
        };
        // `latest.rs` survives `test`: an exclusion is never a fragment.
        assert_eq!(hits(tree(), params).await, ["latest.rs", "main.rs"]);
    }

    #[tokio::test]
    async fn a_size_bound_leaves_out_folders() {
        let params = SearchParams {
            filters: SearchFilters {
                min_size: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(hits(tree(), params).await, ["main.rs", "notes.txt"]);
    }

    #[tokio::test]
    async fn kind_depth_and_hidden() {
        let builder = || {
            tree()
                .file("/root/.env", b"")
                .file("/root/.cache/blob.rs", b"")
        };
        let params = SearchParams {
            skip_hidden: true,
            filters: SearchFilters {
                kind: EntryKind::Directory,
                max_depth: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            hits(builder(), params).await,
            ["node_modules", "src", "test"]
        );

        let params = SearchParams {
            filters: SearchFilters {
                kind: EntryKind::File,
                max_depth: Some(1),
                ..Default::default()
            },
            ..names(".*")
        };
        assert_eq!(hits(builder(), params).await, [".env"]);
    }

    #[tokio::test]
    async fn package_contents_are_skipped_when_asked() {
        let builder = || {
            MockVfs::builder()
                .file("/root/Tool.app/Contents/tool.rs", b"")
                .file("/root/main.rs", b"")
                .package("/root/Tool.app")
        };
        assert_eq!(
            hits(builder(), names("*.rs")).await,
            ["Tool.app/Contents/tool.rs", "main.rs"]
        );
        let params = SearchParams {
            skip_packages: true,
            ..names("*.rs; *.app")
        };
        assert_eq!(hits(builder(), params).await, ["Tool.app", "main.rs"]);
    }

    #[tokio::test]
    async fn the_limit_stops_the_search_and_says_so() {
        let source = tree().build();
        let registry = Arc::new(VfsRegistry::with_root(source.clone()));
        let reader = Arc::new(crate::vfs::VfsRegistryFs::new(registry));
        let vfs = SearchVfs::new(
            source,
            reader,
            VfsPath::new(VfsId::ROOT, PathBuf::from_wire_str("/root")),
            SearchParams {
                filters: SearchFilters {
                    limit: Some(2),
                    ..Default::default()
                },
                ..names("*.rs")
            },
            Vec::new(),
            Arc::new(ScopedReporter::new(Arc::new(NoopProgressSink), VfsId(1))),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let listing = vfs.list_files(&PathBuf::root(), Some(tx)).await.unwrap();
        drain.await.unwrap();
        assert_eq!(listing.files.iter().filter(|f| f.key.is_some()).count(), 2);
        assert_eq!(listing.partial.as_deref(), Some("stopped at 2 results"));
    }

    #[tokio::test]
    async fn gitignored_entries_and_git_itself_are_left_out() {
        let builder = MockVfs::builder()
            .file("/root/.gitignore", b"target/\n*.log\n")
            .file("/root/.git/config", b"")
            .file("/root/src/lib.rs", b"")
            .file("/root/src/debug.log", b"")
            .file("/root/target/debug/out.rs", b"");
        let params = SearchParams {
            gitignore: true,
            filters: SearchFilters {
                kind: EntryKind::File,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(hits(builder, params).await, [".gitignore", "src/lib.rs"]);
    }

    #[tokio::test]
    async fn whole_words_and_files_not_containing() {
        let params = SearchParams {
            content_pattern: Some("todo".into()),
            whole_word: true,
            ..Default::default()
        };
        // `todo!` is a word; `TODO:` too, case-insensitively.
        assert_eq!(hits(tree(), params).await, ["main.rs", "notes.txt"]);

        let params = SearchParams {
            content_pattern: Some("TODO".into()),
            case_sensitive: true,
            not_containing: true,
            ..names("*.rs; *.toml")
        };
        assert_eq!(
            hits(tree(), params).await,
            [
                "Cargo.toml",
                "latest.rs",
                "main.rs",
                "node_modules/dep/index.rs",
                "src/gen/out.rs",
                "test/case.rs"
            ]
        );
    }

    #[tokio::test]
    async fn modified_within_counts_back_from_the_start_of_the_search() {
        let now = std::time::SystemTime::now().to_unix();
        let day = 86_400_000;
        let builder = || {
            MockVfs::builder()
                .file("/root/fresh", b"")
                .file("/root/stale", b"")
                .modified("/root/fresh", now - day)
                .modified("/root/stale", now - 30 * day)
        };
        let within = |newer_than, older_than| SearchParams {
            filters: SearchFilters {
                kind: EntryKind::File,
                modified: TimeRange {
                    newer_than,
                    older_than,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let week = || TimeBound::Ago {
            seconds: 7 * 86_400,
        };
        assert_eq!(hits(builder(), within(Some(week()), None)).await, ["fresh"]);
        assert_eq!(hits(builder(), within(None, Some(week()))).await, ["stale"]);
    }

    #[test]
    fn the_note_shows_the_name_patterns_and_marks_anything_more() {
        let plain = super::note(&names(" *.rs ;; *.toml "));
        assert_eq!(plain.text, "[*.rs; *.toml]");
        assert_eq!(plain.details.len(), 1);

        let more = super::note(&SearchParams {
            gitignore: true,
            filters: SearchFilters {
                min_size: Some(10_000_000),
                modified: TimeRange {
                    newer_than: Some(TimeBound::Ago {
                        seconds: 14 * 86_400,
                    }),
                    older_than: None,
                },
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(more.text, "[*]†");
        let lines: Vec<String> = more
            .details
            .iter()
            .map(|d| format!("{}: {}", d.label, d.value))
            .collect();
        assert_eq!(
            lines,
            [
                "Name: *",
                "Size: at least 10 MB",
                "Modified: within 2 weeks",
                "Options: honours .gitignore"
            ]
        );
    }

    #[test]
    fn owner_and_mode_compare_what_there_is_and_pass_what_there_isnt() {
        let criteria = |filters: SearchFilters| {
            Criteria::compile(&SearchParams {
                filters,
                ..Default::default()
            })
            .unwrap()
        };
        let file = File {
            user: Some(UserGroup::Name("root".into())),
            group: Some(UserGroup::Id(20)),
            mode: Some(crate::vfs::Mode(0o104755)),
            ..Default::default()
        };
        let by_owner = criteria(SearchFilters {
            user: Some("root".into()),
            group: Some("20".into()),
            ..Default::default()
        });
        assert!(by_owner.filters_pass(&file, 0));
        assert!(by_owner.filters_pass(&File::default(), 0));
        let not_root = criteria(SearchFilters {
            user: Some("demo".into()),
            ..Default::default()
        });
        assert!(!not_root.filters_pass(&file, 0));

        let mode = |how, bits| {
            criteria(SearchFilters {
                mode: Some(ModeFilter { how, bits }),
                ..Default::default()
            })
            .filters_pass(&file, 0)
        };
        assert!(mode(ModeMatch::AnyOf, 0o6000));
        assert!(mode(ModeMatch::AllOf, 0o4111));
        assert!(!mode(ModeMatch::AllOf, 0o6000));
        assert!(mode(ModeMatch::Exactly, 0o4755));
        assert!(!mode(ModeMatch::Exactly, 0o755));
    }
}
