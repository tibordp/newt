use log::debug;
use log::info;
use log::warn;
use newt_common::enrich::{
    Annotation, ContextBadge, EnrichScope, EnricherClient, EnrichmentBatch, EnrichmentEvent,
};
use newt_common::filesystem::File;
use newt_common::filesystem::FileList;
use newt_common::filesystem::Filesystem;
use newt_common::filesystem::FsStats;
use newt_common::filesystem::ListFilesOptions;
use newt_common::vfs::{Breadcrumb, OriginKind, VfsId, VfsPath};
use parking_lot::Mutex;
use parking_lot::RwLock;
use parking_lot::RwLockReadGuard;
use parking_lot::RwLockWriteGuard;
use tokio_util::sync::CancellationToken;

use crate::common::Error;
use crate::common::UpdatePublisher;
use crate::main_window::session::VfsInfo;
use std::collections::HashMap;
use std::collections::HashSet;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::DisplayOptions;
use super::DisplayOptionsInner;
use super::MainWindowState;

/// Entry key of the `..` pseudo-entry.
///
/// `..` is a *navigation* affordance, never an operation target: Enter on it
/// must go up, Delete on it must do nothing at all. Navigation asks via
/// `get_focused_*` and legitimately sees it; actions ask via
/// [`Pane::effective_keys`] and never do.
pub const PARENT_KEY: &str = "..";

#[derive(Default, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum SortingKey {
    #[default]
    Name,
    Extension,
    Size,
    User,
    Mode,
    Group,
    Modified,
    Accessed,
    Created,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct Sorting {
    pub key: SortingKey,
    pub asc: bool,
}

impl Default for Sorting {
    fn default() -> Self {
        Self {
            key: SortingKey::default(),
            asc: true,
        }
    }
}

#[derive(Default, Clone, serde::Serialize, specta::Type)]
pub struct PaneStats {
    pub file_count: usize,
    pub dir_count: usize,
    pub bytes: u64,
    pub selected_file_count: usize,
    pub selected_dir_count: usize,
    pub selected_bytes: u64,
    pub total_count: Option<usize>,
    /// Entries filtered out because hidden files are not shown. Always 0
    /// while `show_hidden` is on.
    pub hidden_count: usize,
}

#[derive(Clone)]
struct HistoryEntry {
    path: VfsPath,
    focused: Option<String>,
    /// Pre-formatted human-readable path (e.g. "s3://bucket/foo"), captured
    /// at push time so we can still display the entry meaningfully if the
    /// VFS is later unmounted (the descriptor needed to format `path` would
    /// be gone by then).
    display_path: String,
    /// Pre-resolved VFS display name, captured at push time for the same
    /// reason as `display_path`.
    vfs_display_name: String,
    /// When the user originally arrived at this path. Preserved across
    /// re-visits via back/forward — we don't bump it when popping a snapshot
    /// off a stack and landing on it again. This makes the visual order of
    /// the combined history monotonic in time.
    arrived_at: std::time::SystemTime,
    /// The enrichment overlay as it stood when this view was left,
    /// restored stale-while-revalidate on back/forward/jump: computed
    /// du sizes reappear as they were (the user explicitly returned to
    /// a past view, so staleness is accepted), and automatic enrichers
    /// supersede their part when the landing rerun's results arrive.
    /// Lifetime rides history retention like everything else here.
    enrichments: Option<Arc<EnrichmentSnapshot>>,
}

/// Immutable capture of a pane's enrichment overlay (annotations +
/// badges, not activity), `Arc`-shared so history-stack clones stay
/// cheap.
struct EnrichmentSnapshot {
    annotations: std::collections::BTreeMap<String, HashMap<String, Annotation>>,
    badges: std::collections::BTreeMap<String, Vec<ContextBadge>>,
}

#[derive(Default)]
struct NavigationHistory {
    back: Vec<HistoryEntry>,
    forward: Vec<HistoryEntry>,
}

/// Frontend-visible view of a single history entry. Sent via the
/// HistoryNavigator modal. `is_alive` reflects whether the entry's VFS is
/// currently mounted; the overlay uses this to grey out & skip dead entries.
/// `arrived_at` is the user's original arrival time at this path expressed
/// as Unix milliseconds; the overlay groups adjacent entries into time
/// buckets using these values.
#[derive(Clone, serde::Serialize, specta::Type)]
pub struct HistoryEntryView {
    pub path: VfsPath,
    pub vfs_display_name: String,
    pub display_path: String,
    pub is_alive: bool,
    pub arrived_at: i64,
}

#[derive(Clone, Copy, Debug)]
enum NavigationKind {
    /// Silent in-place reload. No history mutation, no view churn.
    Refresh,
    /// Fresh user-initiated navigation. On landing: push old snapshot to back, clear forward.
    Fresh,
    /// Fresh navigation that takes over the current history slot (search
    /// refinement). On landing: drop the old snapshot, leave both stacks alone.
    Replace,
    /// Back navigation. Caller already popped from `back`. On landing: push old snapshot to forward.
    Back,
    /// Forward navigation. Caller already popped from `forward`. On landing: push old snapshot to back.
    Forward,
    /// History overlay jump. Caller pre-arranged the stacks to land at the target;
    /// commit_history does nothing on landing (and the caller restores the stacks
    /// on NotLanded/Err).
    HistoryJump,
}

#[must_use = "callers may need to know whether the displayed path actually changed"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavigationOutcome {
    /// The pane's displayed path changed to the new target (at least partially).
    /// History was mutated to reflect this.
    Landed,
    /// The pane's displayed path never changed (cancelled or errored before any
    /// batch landed). History was not mutated.
    NotLanded,
}

/// Signals from navigation/cancel entry points to the pane's
/// `enrichment_loop`.
enum EnrichSignal {
    /// A listing landed (navigation or refresh) — (re)run the automatic
    /// enrichers for `path`, superseding any run in flight.
    Start { path: VfsPath },
    /// Cancel the run in flight (Esc, navigate-away). Already-applied
    /// annotations stay; only the computation stops.
    Cancel,
}

pub struct Pane {
    fs: Arc<dyn Filesystem>,
    nav_changes_rx: tokio::sync::watch::Receiver<()>,
    navigation_mutex: tokio::sync::Mutex<tokio::sync::watch::Sender<()>>,
    refresh_queue: AtomicUsize,
    file_list: RwLock<Arc<FileList>>,
    view_state: RwLock<PaneViewState>,
    display_options: DisplayOptions,
    preferences: crate::preferences::PreferencesHandle,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
    cancellation_token: Mutex<Option<CancellationToken>>,
    vfs_info: Arc<dyn VfsInfo>,
    enricher_client: Arc<dyn EnricherClient>,
    enrich_tx: mpsc::UnboundedSender<EnrichSignal>,
    /// Receiver half for `enrichment_loop`, taken once at spawn.
    enrich_rx: Mutex<Option<mpsc::UnboundedReceiver<EnrichSignal>>>,
    /// Cancellation for the in-flight manual enrichment run (du), if
    /// any. Fired by Esc/navigation (`cancel`) and by a newer trigger.
    manual_enrich: Mutex<Option<CancellationToken>>,
    history: Mutex<NavigationHistory>,
    /// When the user arrived at the currently-displayed path. Captured into
    /// each HistoryEntry pushed onto a stack so the overlay can group entries
    /// by time bucket. Reset to `now()` whenever a navigation lands.
    current_arrived_at: Mutex<std::time::SystemTime>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<super::MainWindowEvent>>,
}

impl Pane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fs: Arc<dyn Filesystem>,
        path: VfsPath,
        display_options: DisplayOptions,
        preferences: crate::preferences::PreferencesHandle,
        publisher: Arc<UpdatePublisher<MainWindowState>>,
        vfs_info: Arc<dyn VfsInfo>,
        enricher_client: Arc<dyn EnricherClient>,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<super::MainWindowEvent>>,
    ) -> Self {
        let (tx, rx) = tokio::sync::watch::channel(());
        let (enrich_tx, enrich_rx) = mpsc::unbounded_channel();

        Self {
            fs,
            navigation_mutex: tokio::sync::Mutex::new(tx),
            file_list: RwLock::new(Arc::new(FileList::new(path, vec![], None))),
            refresh_queue: AtomicUsize::new(0),
            view_state: RwLock::new({
                let prefs = preferences.load();
                PaneViewState {
                    vfs_info: Some(vfs_info.clone()),
                    sorting: Sorting {
                        key: match prefs.behavior.default_sort.key {
                            crate::preferences::schema::DefaultSortKey::Name => SortingKey::Name,
                            crate::preferences::schema::DefaultSortKey::Extension => {
                                SortingKey::Extension
                            }
                            crate::preferences::schema::DefaultSortKey::Size => SortingKey::Size,
                            crate::preferences::schema::DefaultSortKey::Modified => {
                                SortingKey::Modified
                            }
                            crate::preferences::schema::DefaultSortKey::Accessed => {
                                SortingKey::Accessed
                            }
                            crate::preferences::schema::DefaultSortKey::Created => {
                                SortingKey::Created
                            }
                        },
                        asc: prefs.behavior.default_sort.ascending,
                    },
                    ..Default::default()
                }
            }),
            nav_changes_rx: rx,
            display_options,
            preferences,
            publisher,
            cancellation_token: Mutex::new(None),
            event_tx,
            vfs_info,
            enricher_client,
            enrich_tx,
            enrich_rx: Mutex::new(Some(enrich_rx)),
            manual_enrich: Mutex::new(None),
            history: Mutex::new(NavigationHistory::default()),
            current_arrived_at: Mutex::new(std::time::SystemTime::now()),
        }
    }

    pub async fn watch_changes(self: Arc<Self>) {
        let mut rx = self.nav_changes_rx.clone();
        let mut prefs_rx = self.preferences.subscribe();
        loop {
            let vfs_path = self.path();
            tokio::select! {
                ret = self.fs.poll_changes(vfs_path.clone()) => {
                    match ret {
                        Ok(()) => {
                            info!("changes detected")
                        }
                        Err(e) => {
                            warn!("failed to watch, the folder was probably removed: {}", e);
                            // We wait until the next navigation before restarting the watch
                            let _ = rx.changed().await;
                            continue;
                        }
                    }
                }
                _ = rx.changed() =>  {
                    continue;
                }
                _ = prefs_rx.changed() => {
                    self.update_view_state();
                    let _ = self.publisher.publish();
                    // Re-run automatic enrichers so preference toggles
                    // (e.g. git status off) take effect immediately.
                    let _ = self.enrich_tx.send(EnrichSignal::Start { path: vfs_path });
                    continue;
                }
            };

            let cloned = self.clone();
            tauri::async_runtime::spawn(async move {
                match cloned.refresh(Some(vfs_path), true).await {
                    Ok(()) => cloned.publisher.publish().unwrap(),
                    Err(e) => warn!("failed to refresh pane: {}", e),
                }
            });
        }
    }

    pub fn path(&self) -> VfsPath {
        self.file_list.read().path().clone()
    }

    pub fn file_list(&self) -> Arc<FileList> {
        self.file_list.read().clone()
    }

    /// Mutate the navigation history stacks at the moment the user lands on
    /// the new path. `old_snapshot` is the pre-navigation view state captured
    /// before any mutation. `new_path` is what the user is now seeing.
    ///
    /// This must run *before* acquiring `view_state` to maintain
    /// history → view_state lock ordering.
    fn commit_history(
        &self,
        kind: NavigationKind,
        old_snapshot: &HistoryEntry,
        new_path: &VfsPath,
    ) {
        if old_snapshot.path == *new_path {
            return;
        }
        let mut history = self.history.lock();
        match kind {
            NavigationKind::Refresh | NavigationKind::HistoryJump | NavigationKind::Replace => {}
            NavigationKind::Fresh => {
                history.back.push(old_snapshot.clone());
                history.forward.clear();
            }
            NavigationKind::Back => {
                history.forward.push(old_snapshot.clone());
            }
            NavigationKind::Forward => {
                history.back.push(old_snapshot.clone());
            }
        }
        // Enforce history retention. The combined view is back + current +
        // forward, so cap that total. 0 = unlimited.
        let limit = self.preferences.load().behavior.history_retention as usize;
        if limit > 0 {
            // current is implicit (not in either stack), so we account for it.
            while history.back.len() + history.forward.len() + 1 > limit {
                if !history.back.is_empty() {
                    history.back.remove(0);
                } else if !history.forward.is_empty() {
                    history.forward.remove(0);
                } else {
                    break;
                }
            }
        }
    }

    async fn navigate_impl(
        &self,
        target: VfsPath,
        kind: NavigationKind,
        restore: Option<Arc<EnrichmentSnapshot>>,
        changes_sender: &mut tokio::sync::watch::Sender<()>,
    ) -> Result<NavigationOutcome, Error> {
        let silent = matches!(kind, NavigationKind::Refresh);
        debug!("navigate_impl: target={:?} kind={:?}", target, kind);

        let mut old_snapshot = self.snapshot();
        if !silent {
            // Capture the overlay being left behind so a later history
            // navigation back to this view can restore it. Skipped for
            // refreshes (no history mutation, and the clone isn't free).
            old_snapshot.enrichments = self.view_state().enrichment_snapshot();
        }

        // If the navigation crosses a VFS boundary into the target, give
        // that VFS a chance to revalidate cached external state (e.g. an
        // archive's central directory if the underlying file changed
        // externally). Skipped for same-VFS navigation, refresh, and any
        // VFS whose descriptor doesn't advertise revalidation — that
        // covers the local FS (the common case) without paying for an
        // RPC round-trip in remote sessions.
        if !matches!(kind, NavigationKind::Refresh)
            && target.vfs_id != old_snapshot.path.vfs_id
            && self
                .vfs_info
                .descriptor(target.vfs_id)
                .is_some_and(|(d, _)| d.can_revalidate())
        {
            match self.fs.revalidate(target.vfs_id).await {
                Ok(_) => {}
                Err(e) => {
                    warn!(
                        "navigate_impl: revalidate({}) failed: {}; aborting navigation",
                        target.vfs_id, e
                    );
                    return Err(e.into());
                }
            }
        }

        let prefs = self.preferences.load();
        let old_file_list = {
            // Temporarily push the new navigation state. This is mostly so people can backspace out of
            // a directory that is taking a long time to load (and not just Esc) - but eventually this
            // is also to support gradual loading of directories. Right now - after this block, the
            // view state is out of sync with the navigation state (still shows the old files)
            let mut file_list = self.file_list.write();
            let old_file_list = std::mem::replace(
                &mut *file_list,
                Arc::new(FileList::new(target.clone(), Vec::new(), None)),
            );

            if !silent {
                let mut ws = self.view_state_mut();
                ws.pending_path = Some(target.clone());
                self.update_display(&mut ws);
            }

            old_file_list
        };

        if !silent {
            let _ = self.publisher.publish();
        }

        // Set up cancellation
        let token = CancellationToken::new();
        if let Some(previous) = self.cancellation_token.lock().replace(token.clone()) {
            previous.cancel();
        }

        // Create batch channel and start streaming.
        // Skip batches when refreshing the same path — keep the current view
        // intact and swap in the final result atomically.
        let same_path = *old_file_list.path() == target;
        debug!(
            "navigate_impl: same_path={} silent={} streaming={}",
            same_path,
            silent,
            !silent && !same_path
        );
        let (batch_tx, mut batch_rx) =
            mpsc::channel::<FileList>(newt_common::filesystem::LIST_BATCH_CHANNEL_CAPACITY);
        let streaming_fut = self.fs.list_files(
            target.clone(),
            ListFilesOptions { strict: !silent },
            (!silent && !same_path).then_some(batch_tx),
        );
        tokio::pin!(streaming_fut);

        let mut accumulated = Vec::new();
        let mut batch_path: Option<VfsPath> = None;
        let mut batch_fs_stats: Option<FsStats>;
        let mut first_batch = true;
        let mut last_publish = Instant::now();
        let mut dirty = false;
        let mut landed = false;
        let throttle = Duration::from_millis(100);

        let result = loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    debug!("navigate_impl: cancelled");
                    break Err(Error::Cancelled);
                }
                result = &mut streaming_fut => {
                    debug!("navigate_impl: streaming_fut completed, ok={}", result.is_ok());
                    break result.map_err(Error::from);
                }
                Some(file_list) = batch_rx.recv() => {
                    let incoming_path = file_list.path().clone();
                    debug!(
                        "navigate_impl: batch received, {} files, path={:?}, path_changed={}",
                        file_list.files().len(),
                        incoming_path,
                        batch_path.as_ref() != Some(&incoming_path)
                    );
                    if batch_path.as_ref() != Some(&incoming_path) {
                        accumulated.clear();
                        batch_path = Some(incoming_path);
                    }
                    batch_fs_stats = file_list.fs_stats().cloned();
                    accumulated.extend(file_list.files().iter().cloned());
                    if !silent {
                        // On first batch: clear pending_path, set loading=true,
                        // and reset filter/selection/focus for the new directory.
                        // This is the landing point — commit history here.
                        if first_batch {
                            if !landed
                                && let Some(bp) = batch_path.as_ref()
                            {
                                self.commit_history(kind, &old_snapshot, bp);
                                landed = true;
                            }
                            let mut ws = self.view_state_mut();
                            ws.pending_path = batch_path.clone();
                            ws.loading = true;
                            ws.set_filter(None);
                            ws.all_selected.clear();
                            ws.focused = None;
                            // Annotations are anchored to the history
                            // cursor — moving it drops them; a history
                            // navigation restores the target view's
                            // captured overlay (stale-while-revalidate:
                            // the Start signal below hasn't fired yet,
                            // so automatic reruns supersede it).
                            ws.clear_enrichments();
                            // Only restore onto the view the snapshot
                            // belongs to — a failed listing can walk up
                            // and land on a parent, where the entry's
                            // keys would decorate the wrong rows.
                            if let Some(ref snapshot) = restore
                                && batch_path.as_ref() == Some(&target)
                            {
                                ws.restore_enrichments(snapshot);
                            }
                            self.update_display(&mut ws);
                            first_batch = false;
                        }
                        // Throttled intermediate publish
                        if first_batch || last_publish.elapsed() >= throttle {
                            let display_options = self.display_options.0.read().clone();
                            let interim = FileList::new(
                                batch_path.clone().unwrap_or_else(|| target.clone()),
                                accumulated.clone(),
                                batch_fs_stats.clone(),
                            );
                            self.view_state_mut().update(display_options, &prefs, &interim);
                            let _ = self.publisher.publish();
                            last_publish = Instant::now();
                            dirty = true;
                        }
                    }
                }
            }
        };

        debug!(
            "navigate_impl: loop exited, accumulated={} files, dirty={}",
            accumulated.len(),
            dirty
        );

        let new_file_list = match result {
            Ok(ret) => {
                debug!(
                    "navigate_impl: success, {} files at {:?}",
                    ret.files().len(),
                    ret.path()
                );
                Arc::new(ret)
            }
            Err(e) => {
                debug!("navigate_impl: failed: {}", e);

                if !dirty {
                    // Restore the old navigation state, but only if we haven't already published a partial update
                    // The user may have already navigated somewhere else, so we shouldn't override that
                    *self.file_list.write() = old_file_list;
                }

                let intrinsic_partial = self.file_list.read().is_partial();
                let mut ws = self.view_state_mut();
                ws.pending_path = None;
                ws.loading = false;
                // OR the consumer-side "we got cut off" signal with the
                // VFS-intrinsic flag (e.g. SearchVfs Cancelled walker).
                ws.partial = dirty || intrinsic_partial;
                self.update_display(&mut ws);

                let outcome = if landed {
                    NavigationOutcome::Landed
                } else {
                    NavigationOutcome::NotLanded
                };
                return match e {
                    Error::Cancelled => Ok(outcome),
                    e => Err(e),
                };
            }
        };

        let has_path_changed = old_file_list.path() != new_file_list.path();
        debug!(
            "navigate_impl: finalizing, old={:?} new={:?} path_changed={}",
            old_file_list.path(),
            new_file_list.path(),
            has_path_changed
        );

        // Final-swap landing point: if streaming never landed (no batches), commit
        // history now if the path changed. Must run before taking view_state to
        // preserve history → view_state lock ordering.
        if has_path_changed && !landed {
            self.commit_history(kind, &old_snapshot, new_file_list.path());
            landed = true;
        }

        let mut ws = self.view_state_mut();

        let mut file_list = self.file_list.write();
        *file_list = new_file_list.clone();

        let display_options = self.display_options.0.read().clone();

        ws.pending_path = None;
        ws.loading = false;
        // VFS-intrinsic partial flag (SearchVfs whose walker was
        // cancelled, …) takes precedence over the consumer-side
        // "we navigated away mid-stream" flavor — both render the
        // same `(partial)` badge.
        ws.partial = new_file_list.is_partial();
        if has_path_changed {
            let _ = changes_sender.send(());
            if let Some(tx) = &self.event_tx {
                let _ = tx.send(super::MainWindowEvent::PaneNavigated);
            }
            // Only clear if we didn't already do it on first batch
            if first_batch {
                ws.set_filter(None);
                ws.all_selected.clear();
                ws.focused = None;
                ws.clear_enrichments();
                // See the first-batch landing: no restore onto a
                // walked-up landing path.
                if let Some(ref snapshot) = restore
                    && *new_file_list.path() == target
                {
                    ws.restore_enrichments(snapshot);
                }
            }
        }

        ws.update(display_options, &prefs, &new_file_list);
        self.update_display(&mut ws);

        if has_path_changed {
            if old_file_list.path().vfs_id != new_file_list.path().vfs_id {
                // VFS boundary crossed (e.g. exiting an archive) — focus the
                // origin filename. Only for Entry origins: exiting a search
                // lands *inside* its Directory origin, where the origin's
                // own name isn't an entry.
                if let Some((desc, _)) = self.vfs_info.descriptor(old_file_list.path().vfs_id)
                    && desc.origin_kind() == OriginKind::Entry
                    && let Some(origin) = self.vfs_info.origin(old_file_list.path().vfs_id)
                    && let Some(name) = origin.file_name()
                {
                    ws.focus(name.to_string());
                }
            } else if target == *new_file_list.path() {
                ws.focus_descendant(old_file_list.path());
            } else {
                ws.focus_descendant(&target);
            }
        }

        // The listing is in place (fresh navigation or refresh) — kick
        // off the automatic enrichers for it.
        let _ = self.enrich_tx.send(EnrichSignal::Start {
            path: new_file_list.path().clone(),
        });

        Ok(if landed {
            NavigationOutcome::Landed
        } else {
            NavigationOutcome::NotLanded
        })
    }

    pub fn cancel(&self) {
        if let Some(token) = self.cancellation_token.lock().take() {
            token.cancel();
        }
        let _ = self.enrich_tx.send(EnrichSignal::Cancel);
        if let Some(token) = self.manual_enrich.lock().take() {
            token.cancel();
        }
    }

    /// Run a manually-triggered enricher (du keybinds) for the pane's
    /// current directory. Runs on its own lane so it isn't superseded
    /// by the automatic enrichment that refreshes restart — only Esc,
    /// navigation (both via [`cancel`](Self::cancel)) or a newer manual
    /// trigger stop it.
    pub async fn run_manual_enrichment(
        self: Arc<Self>,
        enrichers: Vec<String>,
        scope: EnrichScope,
    ) {
        let path = self.path();
        let token = CancellationToken::new();
        if let Some(previous) = self.manual_enrich.lock().replace(token.clone()) {
            previous.cancel();
        }

        let (tx, mut ev_rx) = mpsc::channel(16);
        let run_path = path.clone();
        let fut = self
            .enricher_client
            .enrich(path, scope, enrichers.clone(), tx);
        tokio::pin!(fut);
        let mut fut_done = false;
        let mut events_done = false;
        while !(fut_done && events_done) {
            tokio::select! {
                biased;
                _ = token.cancelled() => break,
                ev = ev_rx.recv(), if !events_done => match ev {
                    Some(ev) => self.apply_enrichment_event(&run_path, ev),
                    None => events_done = true,
                },
                res = &mut fut, if !fut_done => {
                    if let Err(e) = res {
                        debug!("manual enrichment failed: {}", e);
                    }
                    fut_done = true;
                }
            }
        }
        // A cancelled run never sent its Finished events — drop this
        // run's activity labels (and only this run's).
        if self.view_state_mut().clear_activity_for(&enrichers) {
            let _ = self.publisher.publish();
        }
    }

    /// Drive automatic enrichment for this pane: waits for `Start`
    /// signals (sent when a listing lands), runs the enricher client,
    /// and applies streamed events to the view state. One run at a
    /// time; a new signal supersedes the run in flight by dropping its
    /// future (which cancels it locally, or via transport-level
    /// `InvokeCancel` in remote sessions).
    pub async fn enrichment_loop(self: Arc<Self>) {
        let mut rx = self
            .enrich_rx
            .lock()
            .take()
            .expect("enrichment_loop started twice");
        let mut pending: Option<VfsPath> = None;
        loop {
            let path = match pending.take() {
                Some(p) => p,
                None => match rx.recv().await {
                    Some(EnrichSignal::Start { path }) => path,
                    Some(EnrichSignal::Cancel) => continue,
                    None => return,
                },
            };

            let disabled = self.preferences.load().enrichers.disabled_enrichers();
            // A preference toggled off mid-visit: purge that enricher's
            // leftovers — its run won't happen, so no reset batch will.
            if !disabled.is_empty() {
                self.view_state_mut().clear_enrichments_for(&disabled);
                let _ = self.publisher.publish();
            }

            // Select enrichers host-side: static descriptor gate against
            // the pane's VFS × preference gate. Nothing applicable →
            // no request at all (no RPC round-trip on e.g. S3 panes).
            let enrichers: Vec<String> = match self.vfs_info.descriptor(path.vfs_id) {
                Some((vfs_descriptor, _)) => newt_common::enrich::all_enricher_descriptors()
                    .filter(|d| {
                        d.automatic()
                            && d.applies_to_vfs(vfs_descriptor)
                            && !disabled.iter().any(|id| id == d.id())
                    })
                    .map(|d| d.id().to_string())
                    .collect(),
                None => Vec::new(),
            };
            if enrichers.is_empty() {
                continue;
            }

            let (tx, mut ev_rx) = mpsc::channel(16);
            let run_path = path.clone();
            let fut =
                self.enricher_client
                    .enrich(path, EnrichScope::AllEntries, enrichers.clone(), tx);
            tokio::pin!(fut);
            let mut fut_done = false;
            let mut events_done = false;
            while !(fut_done && events_done) {
                tokio::select! {
                    biased;
                    sig = rx.recv() => {
                        match sig {
                            Some(EnrichSignal::Start { path }) => pending = Some(path),
                            Some(EnrichSignal::Cancel) => {}
                            None => {}
                        }
                        break;
                    }
                    ev = ev_rx.recv(), if !events_done => match ev {
                        Some(ev) => self.apply_enrichment_event(&run_path, ev),
                        None => events_done = true,
                    },
                    res = &mut fut, if !fut_done => {
                        if let Err(e) = res {
                            debug!("enrichment failed: {}", e);
                        }
                        fut_done = true;
                    }
                }
            }
            // However the run ended, none of *its* enrichers are
            // running anymore — a cancelled run never sent its Finished
            // events, so drop this run's activity labels (a concurrent
            // manual run's label must survive).
            if self.view_state_mut().clear_activity_for(&enrichers) {
                let _ = self.publisher.publish();
            }
        }
    }

    fn apply_enrichment_event(&self, run_path: &VfsPath, event: EnrichmentEvent) {
        // A run's results are only valid for the view it was requested
        // for. `file_list` flips to the target at navigation *start*,
        // so a straggler event from the previous view's run (already
        // dequeued when the cancel signal landed) is rejected here —
        // annotation keys collide across directories, so letting it
        // through would paint the wrong view. The sliver between this
        // check and the write below is covered by the landing-time
        // overlay clear. (Read fully before taking `view_state`:
        // navigate_impl holds view_state → file_list in that order.)
        if *self.file_list.read().path() != *run_path {
            return;
        }
        {
            let mut ws = self.view_state_mut();
            match event {
                EnrichmentEvent::Started { enricher, activity } => {
                    ws.set_enrichment_activity(enricher, Some(activity));
                }
                EnrichmentEvent::Batch(batch) => ws.apply_enrichment_batch(batch),
                EnrichmentEvent::Finished { enricher } => {
                    ws.set_enrichment_activity(enricher, None);
                }
            }
        }
        let _ = self.publisher.publish();
    }

    pub fn view_state(&self) -> RwLockReadGuard<'_, PaneViewState> {
        self.view_state.read()
    }

    pub fn view_state_mut(&self) -> RwLockWriteGuard<'_, PaneViewState> {
        self.view_state.write()
    }

    pub fn update_view_state(&self) {
        let display_options = self.display_options.0.read().clone();
        let prefs = self.preferences.load();
        let file_list = self.file_list.read();
        let mut view_state: parking_lot::lock_api::RwLockWriteGuard<
            parking_lot::RawRwLock,
            PaneViewState,
        > = self.view_state.write();

        view_state.update(display_options, &prefs, &file_list);
        self.update_display(&mut view_state);
    }

    fn update_display(&self, ws: &mut PaneViewState) {
        if let Some((desc, meta)) = self.vfs_info.descriptor(ws.path.vfs_id) {
            ws.display_path = desc.format_path(&ws.path.path, &meta);
            ws.vfs_display_name = self
                .vfs_info
                .display_name(ws.path.vfs_id)
                .unwrap_or_default();
        } else {
            ws.display_path = ws.path.to_string();
            ws.vfs_display_name = String::new();
        }
        ws.is_host_local = self.vfs_info.is_host_local(ws.path.vfs_id);
        let shown_path = ws.pending_path.as_ref().unwrap_or(&ws.path);
        if let Some((shown_desc, shown_meta)) = self.vfs_info.descriptor(shown_path.vfs_id) {
            ws.breadcrumbs = shown_desc.breadcrumbs(&shown_path.path, &shown_meta);
        }
    }

    pub async fn refresh(&self, expected_path: Option<VfsPath>, force: bool) -> Result<(), Error> {
        // Watcher-driven refreshes pass an `expected_path` they captured
        // before the event arrived; if the user has since navigated away we
        // skip below via the `self.path() == expected_path` check. Caller-
        // driven "refresh whatever you're on" (e.g. window focus) passes
        // None — fall back to current path so the auto_refresh check below
        // still applies.
        let expected_path = expected_path.unwrap_or_else(|| self.path());

        // Skip auto-refresh for VFS types that don't support it (e.g. S3, SFTP, archives)
        if !force
            && let Some((desc, _)) = self.vfs_info.descriptor(expected_path.vfs_id)
            && !desc.auto_refresh()
        {
            return Ok(());
        }

        self.refresh_queue.fetch_add(1, Ordering::SeqCst);

        let mut changes_sender = self.navigation_mutex.lock().await;
        if self.refresh_queue.fetch_sub(1, Ordering::SeqCst) == 1 && self.path() == expected_path {
            self.navigate_impl(
                expected_path,
                NavigationKind::Refresh,
                None,
                &mut changes_sender,
            )
            .await
            .map(|_| ())?;
        }

        Ok(())
    }

    fn snapshot(&self) -> HistoryEntry {
        let view_state = self.view_state.read();
        HistoryEntry {
            path: view_state.path.clone(),
            focused: view_state.focused.clone(),
            display_path: view_state.display_path.clone(),
            vfs_display_name: view_state.vfs_display_name.clone(),
            arrived_at: *self.current_arrived_at.lock(),
            // Filled at the sites that actually push into history
            // (navigate_impl, rearrange_history_to_index) — the clone
            // isn't free and snapshot() also serves display paths.
            enrichments: None,
        }
    }

    pub async fn navigate(&self, rel: &str) -> Result<(), Error> {
        let current = self.path();
        let target = self.resolve_relative(&current, rel);

        // Cancel any pending navigation
        self.cancel();

        let mut changes_sender = self.navigation_mutex.lock().await;
        let outcome = self
            .navigate_impl(target, NavigationKind::Fresh, None, &mut changes_sender)
            .await?;
        if outcome == NavigationOutcome::Landed {
            *self.current_arrived_at.lock() = std::time::SystemTime::now();
        }
        Ok(())
    }

    /// Resolve a relative path *expression* against a VfsPath, crossing
    /// VFS boundaries when `..` escapes above root on a VFS that has an
    /// origin.
    ///
    /// `rel` is a relative fragment (`..`, `a/b`) — never an absolute or
    /// native OS path. Absolute inputs (breadcrumb display paths, typed
    /// absolute paths) are decoded into a `VfsPath` at the navigate
    /// boundary (see `cmd::pane::navigate`) and routed through
    /// `navigate_to`, so they never reach here. We split on `/` and `\`
    /// ourselves rather than going through `std::path::Component`, whose
    /// model would — on Windows — fabricate a drive `Prefix` and silently
    /// corrupt the path. The VFS path domain has no drive/UNC concept.
    fn resolve_relative(&self, base: &VfsPath, rel: &str) -> VfsPath {
        let mut vfs_id = base.vfs_id;
        let mut path = if rel.starts_with(['/', '\\']) {
            // Defensive: an absolute fragment resets to the VFS root.
            newt_common::vfs::path::PathBuf::root()
        } else {
            base.path.clone()
        };

        for seg in rel.split(['/', '\\']) {
            match seg {
                "" | "." => {}
                ".." => {
                    // Ask the descriptor what "up" means — most just pop
                    // one segment, but `LocalVfs` on Windows refuses to
                    // go above a drive/share root.
                    let popped = self
                        .vfs_info
                        .descriptor(vfs_id)
                        .and_then(|(desc, meta)| desc.navigable_parent(&path, &meta));
                    match popped {
                        Some(parent) => path = parent,
                        None => {
                            // At root — try to escape to origin VFS.
                            if let Some((desc, _)) = self.vfs_info.descriptor(vfs_id)
                                && desc.origin_kind() != OriginKind::None
                                && let Some(origin) = self.vfs_info.origin(vfs_id)
                            {
                                vfs_id = origin.vfs_id;
                                path = origin.path.clone();
                                // An Entry origin (archive file) gets popped
                                // by the `..`; a Directory origin (search
                                // root) is itself the landing spot.
                                if desc.origin_kind() == OriginKind::Entry {
                                    path.pop();
                                }
                            }
                            // No origin — clamp at root.
                        }
                    }
                }
                seg => path.push(seg),
            }
        }

        VfsPath::new(vfs_id, path)
    }

    pub async fn navigate_to(&self, target: VfsPath) -> Result<(), Error> {
        // Cancel any pending navigation
        self.cancel();

        let mut changes_sender = self.navigation_mutex.lock().await;
        let outcome = self
            .navigate_impl(target, NavigationKind::Fresh, None, &mut changes_sender)
            .await?;
        if outcome == NavigationOutcome::Landed {
            *self.current_arrived_at.lock() = std::time::SystemTime::now();
        }
        Ok(())
    }

    /// Like [`navigate_to`](Self::navigate_to), but the new path takes over
    /// the current history entry instead of pushing it onto the back stack.
    /// Used when the current entry is superseded rather than left (search
    /// refinement re-mounts).
    pub async fn navigate_to_replace(&self, target: VfsPath) -> Result<(), Error> {
        self.cancel();

        let mut changes_sender = self.navigation_mutex.lock().await;
        let outcome = self
            .navigate_impl(target, NavigationKind::Replace, None, &mut changes_sender)
            .await?;
        if outcome == NavigationOutcome::Landed {
            *self.current_arrived_at.lock() = std::time::SystemTime::now();
        }
        Ok(())
    }

    /// Return the set of VFS ids referenced by any entry in this pane's
    /// history (back + forward stacks). The currently-displayed path is
    /// *not* included — callers that also want the current VFS id can
    /// fold `self.path().vfs_id` in. Used by archive auto-unmount to
    /// keep mounts alive as long as they remain reachable via back/forward.
    pub fn history_vfs_ids(&self) -> Vec<VfsId> {
        let history = self.history.lock();
        history
            .back
            .iter()
            .chain(history.forward.iter())
            .map(|e| e.path.vfs_id)
            .collect()
    }

    /// Remove the history entry at `target_index` in the combined view.
    /// The current entry cannot be removed; this is a no-op for the
    /// current index.
    pub fn delete_history_entry(&self, target_index: usize) {
        let mut history = self.history.lock();
        let current_index = history.forward.len();
        if target_index == current_index {
            return;
        }
        if target_index < current_index {
            // Forward section. forward[0] is at list[0]; forward.last() is
            // at list[current_index - 1]. So forward index = (current_index - 1) - target_index.
            let fi = current_index - 1 - target_index;
            if fi < history.forward.len() {
                history.forward.remove(fi);
            }
        } else {
            // Back section. back.last() is at list[current_index + 1];
            // back[0] is at list[current_index + back.len()]. So back index
            // = back.len() - (target_index - current_index).
            let offset = target_index - current_index;
            if offset > 0 && offset <= history.back.len() {
                let bi = history.back.len() - offset;
                history.back.remove(bi);
            }
        }
    }

    /// Build a flat view of the history for the overlay UI. Forward (redo)
    /// entries on top, back (undo) entries below — current sits between them.
    /// Within each section, entries closest to current are nearest current in
    /// the list:
    ///
    ///   list[0]                          = furthest forward (forward[0])
    ///   list[forward.len() - 1]          = closest forward  (forward.last())
    ///   list[forward.len()]              = current  (== `current_index`)
    ///   list[forward.len() + 1]          = closest back     (back.last())
    ///   list[forward.len() + back.len()] = furthest back    (back[0])
    pub fn history_entries(&self) -> (Vec<HistoryEntryView>, usize) {
        let history = self.history.lock();
        let current = self.snapshot();

        let mut entries = Vec::with_capacity(history.back.len() + history.forward.len() + 1);
        // Forward section: forward[0] (furthest future) at top, forward.last()
        // (closest future) just above current.
        for entry in history.forward.iter() {
            entries.push(self.entry_view(entry));
        }
        let current_index = entries.len();
        entries.push(self.entry_view(&current));
        // Back section: back.last() (closest past) just below current,
        // back[0] (furthest past) at the very bottom.
        for entry in history.back.iter().rev() {
            entries.push(self.entry_view(entry));
        }
        (entries, current_index)
    }

    fn entry_view(&self, entry: &HistoryEntry) -> HistoryEntryView {
        use newt_common::ToUnix;
        // Prefer the formatted strings captured at push time so unmounted
        // VFSes still render meaningfully. The descriptor lookup only
        // determines whether the entry is still navigable.
        let is_alive = self.vfs_info.descriptor(entry.path.vfs_id).is_some();
        HistoryEntryView {
            path: entry.path.clone(),
            vfs_display_name: entry.vfs_display_name.clone(),
            display_path: entry.display_path.clone(),
            is_alive,
            arrived_at: entry.arrived_at.to_unix(),
        }
    }

    /// Move entries between back/forward so that the entry at `target_index`
    /// in the combined view becomes the navigation target. Returns the target
    /// HistoryEntry, or None if `target_index` already refers to the current
    /// position. Caller is responsible for restoring stacks if the resulting
    /// navigation does not land.
    ///
    /// In the combined view, forward entries occupy `[0, current_index)` and
    /// back entries occupy `(current_index, ..]`, so `target_index <
    /// current_index` means going forward, and `>` means going back.
    fn rearrange_history_to_index(&self, target_index: usize) -> Option<HistoryEntry> {
        let mut history = self.history.lock();
        let current_index = history.forward.len();
        if target_index == current_index {
            return None;
        }
        let mut snap = self.snapshot();
        snap.enrichments = self.view_state().enrichment_snapshot();

        if target_index < current_index {
            // Forward navigation: pop entries from forward, push to back.
            let n = current_index - target_index;
            history.back.push(snap);
            let mut popped: Vec<HistoryEntry> = (0..n)
                .map(|_| {
                    history
                        .forward
                        .pop()
                        .expect("forward depth was just measured")
                })
                .collect();
            // popped[n - 1] is the deepest entry from forward — that's the target.
            let target = popped.pop().expect("n > 0");
            for entry in popped {
                history.back.push(entry);
            }
            Some(target)
        } else {
            // Back navigation: pop entries from back, push to forward.
            let n = target_index - current_index;
            history.forward.push(snap);
            let mut popped: Vec<HistoryEntry> = (0..n)
                .map(|_| history.back.pop().expect("back depth was just measured"))
                .collect();
            let target = popped.pop().expect("n > 0");
            for entry in popped {
                history.forward.push(entry);
            }
            Some(target)
        }
    }

    /// Jump directly to the entry at `target_index` in the combined history
    /// view, rearranging back/forward stacks accordingly. If the navigation
    /// fails to land, the stacks are restored to their pre-call state.
    pub async fn navigate_history(&self, target_index: usize) -> Result<(), Error> {
        self.cancel();
        let mut changes_sender = self.navigation_mutex.lock().await;

        let backup = {
            let h = self.history.lock();
            (h.back.clone(), h.forward.clone())
        };

        let target = match self.rearrange_history_to_index(target_index) {
            Some(t) => t,
            None => return Ok(()),
        };
        let target_path = target.path.clone();
        let focused = target.focused;
        let arrived_at = target.arrived_at;

        let result = self
            .navigate_impl(
                target_path,
                NavigationKind::HistoryJump,
                target.enrichments.clone(),
                &mut changes_sender,
            )
            .await;
        drop(changes_sender);

        match &result {
            Ok(NavigationOutcome::Landed) => {
                // Preserve the original arrival time of the entry we landed
                // on, so its position in the history timeline is stable.
                *self.current_arrived_at.lock() = arrived_at;
                if let Some(name) = focused {
                    self.view_state_mut().focus(name);
                }
            }
            Ok(NavigationOutcome::NotLanded) | Err(_) => {
                let mut h = self.history.lock();
                h.back = backup.0;
                h.forward = backup.1;
            }
        }
        result.map(|_| ())
    }

    /// Pop one entry from `back` and navigate to it. If the navigation never
    /// lands (cancelled or errored before any batch arrived), the popped
    /// entry is restored — history reflects what the user actually saw.
    pub async fn navigate_back(&self) -> Result<(), Error> {
        let popped = match self.history.lock().back.pop() {
            Some(e) => e,
            None => return Ok(()),
        };
        let focused = popped.focused.clone();
        let target_path = popped.path.clone();
        let arrived_at = popped.arrived_at;

        self.cancel();
        let mut changes_sender = self.navigation_mutex.lock().await;
        let result = self
            .navigate_impl(
                target_path,
                NavigationKind::Back,
                popped.enrichments.clone(),
                &mut changes_sender,
            )
            .await;
        drop(changes_sender);

        match &result {
            Ok(NavigationOutcome::Landed) => {
                *self.current_arrived_at.lock() = arrived_at;
                if let Some(name) = focused {
                    self.view_state_mut().focus(name);
                }
            }
            Ok(NavigationOutcome::NotLanded) | Err(_) => {
                // Pre-landing cancel/error: the displayed path never changed.
                // Restore the popped entry so the user can retry.
                self.history.lock().back.push(popped);
            }
        }
        result.map(|_| ())
    }

    pub async fn navigate_forward(&self) -> Result<(), Error> {
        let popped = match self.history.lock().forward.pop() {
            Some(e) => e,
            None => return Ok(()),
        };
        let focused = popped.focused.clone();
        let target_path = popped.path.clone();
        let arrived_at = popped.arrived_at;

        self.cancel();
        let mut changes_sender = self.navigation_mutex.lock().await;
        let result = self
            .navigate_impl(
                target_path,
                NavigationKind::Forward,
                popped.enrichments.clone(),
                &mut changes_sender,
            )
            .await;
        drop(changes_sender);

        match &result {
            Ok(NavigationOutcome::Landed) => {
                *self.current_arrived_at.lock() = arrived_at;
                if let Some(name) = focused {
                    self.view_state_mut().focus(name);
                }
            }
            Ok(NavigationOutcome::NotLanded) | Err(_) => {
                self.history.lock().forward.push(popped);
            }
        }
        result.map(|_| ())
    }

    /// The entry keys an action operates on: the selection if there is one,
    /// else the focused entry — and never [`PARENT_KEY`], in either case.
    ///
    /// In filter mode, only entries visible in the current filtered view are
    /// included, so hidden selected files don't piggyback on operations.
    ///
    /// `..` is excluded *here* rather than at each call site. The selection
    /// mutators already keep it out of `all_selected`, but the fall back to
    /// `focused` had no such gate, which is how Delete came to prompt for it.
    /// Every consumer already treats an empty result as "do nothing", so this
    /// one guarantee is all they need. Actions must ask through this or its
    /// wrappers below, never `focused` directly.
    fn effective_keys(view_state: &PaneViewState) -> Vec<String> {
        let keys: Vec<&String> = if view_state.all_selected.is_empty() {
            view_state.actionable_focus().into_iter().collect()
        } else if view_state.filter_mode == FilterMode::Filter {
            view_state
                .all_selected
                .iter()
                .filter(|s| view_state.file_lookup.contains_key(s.as_str()))
                .collect()
        } else {
            view_state.all_selected.iter().collect()
        };

        keys.into_iter()
            .filter(|k| k.as_str() != PARENT_KEY)
            .cloned()
            .collect()
    }

    /// Paths of the effective selection. See [`Self::effective_keys`].
    pub fn get_effective_selection(&self) -> Vec<VfsPath> {
        let view_state = self.view_state.read();
        Self::effective_keys(&view_state)
            .iter()
            .map(|s| view_state.path.join(s))
            .collect()
    }

    /// Entry keys of the effective selection (same rules as
    /// [`get_effective_selection`](Self::get_effective_selection), but
    /// directory-scoped keys instead of joined `VfsPath`s). Used by
    /// per-entry enrichment triggers, which address entries by key.
    pub fn effective_selection_keys(&self) -> Vec<String> {
        let view_state = self.view_state.read();
        Self::effective_keys(&view_state)
    }

    pub fn get_focused_file(&self) -> Option<VfsPath> {
        let view_state = self.view_state.read();

        view_state.focused.as_ref().map(|s| view_state.path.join(s))
    }

    /// Like `get_focused_file`, but dereferences synthetic-VFS entries
    /// (e.g. flat search results) to their underlying source path. For
    /// real filesystem entries this returns the same `VfsPath` as
    /// `get_focused_file` since `source` is unset.
    ///
    /// Use at cmd-layer sites that explicitly intend to operate on the
    /// real file (open with system handler, navigate the *other* pane to
    /// where the file actually lives, mount-archive on the real path,
    /// reveal-in-finder, clipboard, …).
    pub fn get_focused_source(&self) -> Option<VfsPath> {
        let view_state = self.view_state.read();
        let focused = view_state.focused.as_ref()?;
        let file = view_state.files.iter().find(|f| f.key() == focused)?;
        Some(
            file.source
                .clone()
                .unwrap_or_else(|| view_state.path.join(focused)),
        )
    }

    /// Like `get_effective_selection`, but dereferences synthetic-VFS
    /// entries to their underlying source paths. See `get_focused_source`.
    pub fn get_effective_selection_dereferenced(&self) -> Vec<VfsPath> {
        let view_state = self.view_state.read();
        Self::effective_keys(&view_state)
            .iter()
            .map(|key| {
                view_state
                    .files
                    .iter()
                    .find(|f| f.key() == key)
                    .and_then(|f| f.source.clone())
                    .unwrap_or_else(|| view_state.path.join(key))
            })
            .collect()
    }

    pub fn get_focused_file_info(&self) -> Option<newt_common::filesystem::File> {
        let view_state = self.view_state.read();
        let focused = view_state.focused.as_ref()?;
        view_state
            .files
            .iter()
            .find(|f| f.key() == focused)
            .cloned()
    }

    pub fn get_focused_symlink_target(&self) -> Option<String> {
        let view_state = self.view_state.read();
        let focused = view_state.focused.as_ref()?;
        view_state
            .files
            .iter()
            .find(|f| f.key() == focused)
            .and_then(|f| f.symlink_target.clone())
    }

    /// Returns true if the focused item is known to be a directory.
    /// Returns false for non-directories, unknown items, or if nothing is focused.
    pub fn focus_file(&self, name: &str) {
        self.view_state.write().focus(name.to_string());
    }

    pub fn is_focused_dir(&self) -> bool {
        let view_state = self.view_state.read();
        let focused = match view_state.focused.as_ref() {
            Some(f) => f,
            None => return false,
        };
        view_state
            .files
            .iter()
            .find(|f| f.key() == focused)
            .is_some_and(|f| f.is_dir)
    }
}

impl serde::Serialize for Pane {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.view_state.read().serialize(serializer)
    }
}

fn compare_extension(a: &File, b: &File) -> std::cmp::Ordering {
    if a.is_dir && b.is_dir {
        return std::cmp::Ordering::Equal;
    }

    let a = a.name.rfind('.').map(|i| &a.name[i + 1..]).unwrap_or("");
    let b = b.name.rfind('.').map(|i| &b.name[i + 1..]).unwrap_or("");

    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

#[derive(
    Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    #[default]
    QuickSearch,
    Filter,
}

/// Display projection of `File` for the frontend. Carries everything
/// `File` does, plus pre-rendered fields the frontend can't easily
/// derive on its own — currently `source_display`, the source path
/// formatted through the source VFS's descriptor for synthetic VFS
/// entries (search results' "where from" hint).
///
/// Computing display strings on the descriptor is cheap and stays on
/// the host process, so we don't carry the extra string across the RPC
/// boundary; only `File` does. The conversion happens when the pane
/// builds its window.
#[derive(Clone, serde::Serialize, specta::Type)]
pub struct FileView {
    #[serde(flatten)]
    pub file: File,
    /// Pre-rendered "where from" label — the parent directory of
    /// `file.source` rendered through the source VFS's `format_path`,
    /// when `source` is set and the source VFS is still mounted. `None`
    /// for ordinary entries.
    pub source_display: Option<String>,
    /// Annotations from the enrichment overlay, in stable per-enricher
    /// order. Opaque to the pane; the frontend interprets the kinds it
    /// knows (git status → row coloring).
    pub annotations: Vec<Annotation>,
}

/// A windowed slice of the file list sent to the frontend.
#[derive(Default, Clone, serde::Serialize, specta::Type)]
pub struct FileWindow {
    /// The files in the current window, projected for display.
    pub items: Vec<FileView>,
    /// Index of the first item in `items` within the full sorted/filtered list.
    pub offset: usize,
    /// Total number of files in the full sorted/filtered list.
    pub total_count: usize,
}

/// View model for a pane.
#[derive(Default, Clone, serde::Serialize, specta::Type)]
pub struct PaneViewState {
    pub path: VfsPath,
    pub pending_path: Option<VfsPath>,
    pub loading: bool,
    pub partial: bool,
    pub sorting: Sorting,
    pub file_window: FileWindow,
    pub focused: Option<String>,
    /// Selected filenames intersected with the current window (for frontend rendering).
    pub selected: HashSet<String>,
    #[serde(skip)]
    all_selected: HashSet<String>,
    #[serde(skip)]
    drag_base: Option<HashSet<String>>,
    pub filter: Option<String>,
    pub filter_mode: FilterMode,
    pub fs_stats: Option<FsStats>,
    pub stats: PaneStats,
    pub focused_index: Option<usize>,
    pub display_path: String,
    pub vfs_display_name: String,
    pub is_host_local: bool,
    pub breadcrumbs: Vec<Breadcrumb>,
    /// Per-location badges from enrichers (branch indicator, …), in
    /// stable per-enricher order.
    pub context_badges: Vec<ContextBadge>,
    /// Status-bar labels of enrichers currently running, keyed by
    /// enricher id.
    pub enrichment_activity: std::collections::BTreeMap<String, String>,

    /// Enrichment overlay: enricher id → entry key → annotation.
    /// Anchored to the history cursor — survives refresh, cleared on
    /// navigation. Merged into `FileView` at window projection.
    #[serde(skip)]
    enrichments: std::collections::BTreeMap<String, HashMap<String, Annotation>>,
    #[serde(skip)]
    enrichment_badges: std::collections::BTreeMap<String, Vec<ContextBadge>>,
    /// Hidden entries dropped from the listing (see `PaneStats::hidden_count`).
    #[serde(skip)]
    hidden_count: usize,

    /// Full sorted/filtered file list (not serialized — only the window is sent).
    #[serde(skip)]
    files: Vec<File>,
    #[serde(skip)]
    all_files: Vec<File>,
    #[serde(skip)]
    file_lookup: HashMap<String, usize>,
    #[serde(skip)]
    filter_regex: Option<regex::Regex>,
    #[serde(skip)]
    default_filter_mode: FilterMode,
    /// Last-used folders-first flag, cached so enrichment batches can
    /// re-sort in place (they arrive without preference access).
    #[serde(skip)]
    folders_first: bool,
    /// Last viewport hint from the frontend: (first_visible_index, visible_count).
    #[serde(skip)]
    viewport_hint: (usize, usize),
    /// Incremented when the file list changes, forcing recompute_window to
    /// rebuild even if the boundary indices happen to match.
    #[serde(skip)]
    file_generation: u64,
    /// The generation at which the current window was built.
    #[serde(skip)]
    window_generation: u64,
    /// Used to project `File` → `FileView` (rendering the synthetic-VFS
    /// `source` path through the source VFS's descriptor) when the
    /// frontend window is rebuilt. Optional so PaneViewState can still
    /// derive `Default` for tests / placeholder values.
    #[serde(skip)]
    vfs_info: Option<Arc<dyn VfsInfo>>,
}

impl PaneViewState {
    /// Read-only access to the (sorted, filtered) file list. Cmd-layer
    /// helpers reach through this to look up `source` for synthetic-VFS
    /// dereferencing.
    pub fn files(&self) -> &[File] {
        &self.files
    }

    /// The focused entry's key, when it is a legitimate *operation* target —
    /// `None` when focus is on [`PARENT_KEY`].
    ///
    /// Actions that target the focused entry rather than the selection
    /// (Rename) must read focus through this. Navigation wants the opposite
    /// and reads `focused` / [`Pane::get_focused_file`] directly, because
    /// Enter on `..` is the whole point of `..`.
    pub fn actionable_focus(&self) -> Option<&String> {
        self.focused.as_ref().filter(|k| k.as_str() != PARENT_KEY)
    }

    fn recompute_stats(&mut self) {
        let mut stats = PaneStats {
            hidden_count: self.hidden_count,
            ..PaneStats::default()
        };
        for f in &self.files {
            if f.name == PARENT_KEY {
                continue;
            }
            // Directories contribute their computed recursive size when
            // the du enricher has produced one, so a ⌘A after Calculate
            // All Sizes totals the whole directory in the status bar.
            let size = Self::effective_size(&self.enrichments, f).unwrap_or(0);
            if f.is_dir {
                stats.dir_count += 1;
            } else {
                stats.file_count += 1;
            }
            stats.bytes += size;
            if self.all_selected.contains(f.key()) {
                if f.is_dir {
                    stats.selected_dir_count += 1;
                } else {
                    stats.selected_file_count += 1;
                }
                stats.selected_bytes += size;
            }
        }
        if self.filter_mode == FilterMode::Filter {
            // Exclude ".." from both counts for a meaningful "N of M" display
            let visible = self.files.iter().filter(|f| f.name != PARENT_KEY).count();
            let total = self
                .all_files
                .iter()
                .filter(|f| f.name != PARENT_KEY)
                .count();
            if visible != total {
                stats.total_count = Some(total);
            }
        }
        self.stats = stats;
        self.recompute_focused_index();
        // Always recompute the windowed selection after stats, since
        // recompute_window may have early-returned (unchanged boundaries)
        // but the selection itself may have changed.
        self.recompute_selected_window();
    }

    /// Lightweight update: only recomputes focused_index and conditionally
    /// recomputes the window. Use instead of recompute_stats when only the
    /// focused item changed (not files, selection, or filter).
    fn recompute_focused_index(&mut self) {
        self.focused_index = self
            .focused
            .as_ref()
            .and_then(|name| self.file_lookup.get(name).copied());
        self.recompute_window();
    }

    /// Like recompute_focused_index, but also slides the viewport hint to
    /// cover the focused item when it's outside the current viewport range.
    /// Use this when focus is intentionally changing (keyboard nav, click)
    /// but NOT when the file list changes (sort, filter, navigation) — in
    /// those cases the viewport should stay where it is.
    fn recompute_focused_index_and_viewport(&mut self) {
        self.focused_index = self
            .focused
            .as_ref()
            .and_then(|name| self.file_lookup.get(name).copied());
        if let Some(fi) = self.focused_index {
            let (fv, vc) = self.viewport_hint;
            let vc = if vc == 0 { 50 } else { vc };
            if fi < fv || fi >= fv + vc {
                self.viewport_hint = (fi.saturating_sub(vc / 2), vc);
            }
        }
        self.recompute_window();
    }

    /// Recomputes the file window slice. Skips the clone if boundaries
    /// haven't changed and the file list generation matches, keeping the
    /// serialized state identical and making publish() diffing free.
    fn recompute_window(&mut self) {
        let total = self.files.len();
        let (first_visible, visible_count) = self.viewport_hint;

        // Before the frontend reports its viewport, use a reasonable default
        let visible_count = if visible_count == 0 {
            50
        } else {
            visible_count
        };
        let buffer = visible_count * 2; // 2 pages each direction

        // Clamp first_visible to the actual file count to handle stale hints
        let first_visible = first_visible.min(total.saturating_sub(1));
        let start = first_visible.saturating_sub(buffer);
        let end = (first_visible + visible_count + buffer).min(total);

        // Skip the clone if the window boundaries and file contents haven't changed
        if self.window_generation == self.file_generation
            && self.file_window.offset == start
            && self.file_window.items.len() == end - start
            && self.file_window.total_count == total
        {
            return;
        }

        let items: Vec<FileView> = self.files[start..end]
            .iter()
            .map(|f| self.project_file(f))
            .collect();
        self.file_window = FileWindow {
            items,
            offset: start,
            total_count: total,
        };
        self.window_generation = self.file_generation;
        self.recompute_selected_window();
    }

    /// Render `File` → `FileView`, attaching a frontend-friendly
    /// "where from" label for entries that carry a `source` (search
    /// results, etc.). Goes through the source VFS's descriptor so the
    /// label matches the rest of the app's path formatting (e.g. an
    /// archive entry shows `/path/to/foo.zip/inner/dir`, not the raw
    /// archive-internal path).
    fn project_file(&self, f: &File) -> FileView {
        let source_display = f.source.as_ref().and_then(|src| {
            let parent = src.parent()?;
            let info = self.vfs_info.as_ref()?;
            let (desc, meta) = info.descriptor(src.vfs_id)?;
            Some(desc.format_path(&parent.path, &meta))
        });
        let annotations = self
            .enrichments
            .values()
            .filter_map(|entries| entries.get(f.key()).cloned())
            .collect();
        FileView {
            file: f.clone(),
            source_display,
            annotations,
        }
    }

    /// Capture the current overlay for the history entry being left, or
    /// `None` when there is nothing to remember.
    fn enrichment_snapshot(&self) -> Option<Arc<EnrichmentSnapshot>> {
        if self.enrichments.is_empty() && self.enrichment_badges.is_empty() {
            return None;
        }
        Some(Arc::new(EnrichmentSnapshot {
            annotations: self.enrichments.clone(),
            badges: self.enrichment_badges.clone(),
        }))
    }

    /// Replace the overlay with a history entry's captured one
    /// (stale-while-revalidate on history navigation). Runs at the
    /// landing point, before the automatic-enrichment restart is
    /// signalled, so automatic enrichers deterministically supersede
    /// their part; manual results (du) simply stand.
    ///
    /// Deliberately no immediate window rebuild (and no re-sort): at the
    /// landing point `files` can still hold the *previous* directory's
    /// rows for up to the interim-publish throttle, and composing those
    /// with the restored annotations would decorate the wrong rows
    /// wherever keys collide. Bumping the generation is enough — the
    /// landing's `update()` swaps the files in, sorts with the restored
    /// overlay in place, and rebuilds the window in one step.
    fn restore_enrichments(&mut self, snapshot: &EnrichmentSnapshot) {
        self.enrichments = snapshot.annotations.clone();
        self.enrichment_badges = snapshot.badges.clone();
        self.rebuild_context_badges();
        self.file_generation += 1;
    }

    /// Drop the whole enrichment overlay (annotations, badges, activity).
    /// Called when the history cursor moves — annotations are anchored
    /// to the current visit and never survive navigation.
    pub fn clear_enrichments(&mut self) {
        if self.enrichments.is_empty()
            && self.enrichment_badges.is_empty()
            && self.enrichment_activity.is_empty()
        {
            return;
        }
        self.enrichments.clear();
        self.enrichment_badges.clear();
        self.enrichment_activity.clear();
        self.context_badges.clear();
        self.file_generation += 1;
        self.recompute_window();
    }

    /// Drop the overlay of specific enrichers (preference toggled off —
    /// no run happens for them, so no reset batch would clear them).
    fn clear_enrichments_for(&mut self, ids: &[String]) {
        let mut changed = false;
        for id in ids {
            changed |= self.enrichments.remove(id).is_some();
            changed |= self.enrichment_badges.remove(id).is_some();
            self.enrichment_activity.remove(id);
        }
        if changed {
            self.rebuild_context_badges();
            self.file_generation += 1;
            self.recompute_window();
        }
    }

    fn apply_enrichment_batch(&mut self, batch: EnrichmentBatch) {
        // Computed sizes participate in sort-by-size — a batch that
        // touches them while that order is active re-sorts in place.
        let sizes_touched = batch.reset
            || batch
                .entries
                .iter()
                .any(|(_, a)| matches!(a, Annotation::RecursiveSize { .. }));

        let annotations = self.enrichments.entry(batch.enricher.clone()).or_default();
        if batch.reset {
            annotations.clear();
        }
        annotations.extend(batch.entries);

        let badges = self.enrichment_badges.entry(batch.enricher).or_default();
        if batch.reset {
            badges.clear();
        }
        for badge in batch.badges {
            badges.retain(|b| std::mem::discriminant(b) != std::mem::discriminant(&badge));
            badges.push(badge);
        }
        self.rebuild_context_badges();

        if sizes_touched {
            // Computed sizes feed both sort-by-size and the selection
            // totals — keep them live while a walk streams.
            if matches!(self.sorting.key, SortingKey::Size) {
                self.sort(self.folders_first);
            } else {
                self.file_generation += 1;
            }
            self.recompute_stats();
        } else {
            self.file_generation += 1;
            self.recompute_window();
        }
    }

    fn rebuild_context_badges(&mut self) {
        self.context_badges = self.enrichment_badges.values().flatten().cloned().collect();
    }

    fn set_enrichment_activity(&mut self, enricher: String, label: Option<String>) {
        match label {
            Some(label) => {
                self.enrichment_activity.insert(enricher, label);
            }
            None => {
                self.enrichment_activity.remove(&enricher);
            }
        }
    }

    /// Drop the activity labels of the given enrichers (an ended run's
    /// own ids, leaving concurrent runs' labels alone). Returns whether
    /// anything was cleared (i.e. a publish is due).
    fn clear_activity_for(&mut self, ids: &[String]) -> bool {
        let before = self.enrichment_activity.len();
        for id in ids {
            self.enrichment_activity.remove(id);
        }
        self.enrichment_activity.len() != before
    }

    /// Projects all_selected onto the current window so only visible
    /// selection state is serialized to the frontend.
    fn recompute_selected_window(&mut self) {
        self.selected = self
            .file_window
            .items
            .iter()
            .filter(|fv| self.all_selected.contains(fv.file.key()))
            .map(|fv| fv.file.key().to_string())
            .collect();
    }

    /// Returns true if the window actually changed.
    pub fn set_viewport_hint(&mut self, first_visible: usize, visible_count: usize) -> bool {
        self.viewport_hint = (first_visible, visible_count);
        let old_offset = self.file_window.offset;
        let old_len = self.file_window.items.len();
        self.recompute_window();
        self.file_window.offset != old_offset || self.file_window.items.len() != old_len
    }

    /// The size an entry sorts by: a computed recursive size (du
    /// enricher) beats the entry's own reported size.
    fn effective_size(
        enrichments: &std::collections::BTreeMap<String, HashMap<String, Annotation>>,
        f: &File,
    ) -> Option<u64> {
        for annotations in enrichments.values() {
            if let Some(Annotation::RecursiveSize { bytes, .. }) = annotations.get(f.key()) {
                return Some(*bytes);
            }
        }
        f.size
    }

    fn sort(&mut self, folders_first: bool) {
        self.folders_first = folders_first;
        let enrichments = &self.enrichments;
        self.files.sort_by(|a, b| {
            if a.name == PARENT_KEY {
                return std::cmp::Ordering::Less;
            } else if b.name == PARENT_KEY {
                return std::cmp::Ordering::Greater;
            }

            if folders_first {
                if a.is_dir && !b.is_dir {
                    return std::cmp::Ordering::Less;
                } else if !a.is_dir && b.is_dir {
                    return std::cmp::Ordering::Greater;
                }
            }

            let (a, b) = if self.sorting.asc { (a, b) } else { (b, a) };
            match self.sorting.key {
                SortingKey::Name => a
                    .name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.name.cmp(&b.name)),
                SortingKey::Extension => compare_extension(a, b),
                SortingKey::Size => {
                    Self::effective_size(enrichments, a).cmp(&Self::effective_size(enrichments, b))
                }
                SortingKey::User => a
                    .user
                    .partial_cmp(&b.user)
                    .unwrap_or(std::cmp::Ordering::Less),
                SortingKey::Group => a
                    .group
                    .partial_cmp(&b.group)
                    .unwrap_or(std::cmp::Ordering::Less),
                SortingKey::Mode => a.mode.cmp(&b.mode),
                SortingKey::Modified => a.modified.unwrap_or(0).cmp(&b.modified.unwrap_or(0)),
                SortingKey::Accessed => a.accessed.unwrap_or(0).cmp(&b.accessed.unwrap_or(0)),
                SortingKey::Created => a.created.unwrap_or(0).cmp(&b.created.unwrap_or(0)),
            }
        });

        self.file_lookup = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.key().to_string(), index))
            .collect();
        self.file_generation += 1;
    }

    fn update_focus(&mut self) {
        if self.filter_mode == FilterMode::Filter {
            // In filter mode, retain selection based on all files (not just visible ones)
            let all_keys: HashSet<&str> = self.all_files.iter().map(|f| f.key()).collect();
            self.all_selected
                .retain(|name| all_keys.contains(name.as_str()));
        } else {
            self.all_selected
                .retain(|name| self.file_lookup.contains_key(name));
        }

        // If our focused file has disappeared, try to focus the nearest one by index
        if self
            .focused
            .as_ref()
            .is_none_or(|name| !self.file_lookup.contains_key(name))
        {
            // focused_index has not been updated yet, so we still have the chance to use it to find a nearby file to focus.
            // This makes the UI feel more stable when files are added/removed above the focused file.
            let index = self
                .focused_index
                .unwrap_or(0)
                .min(self.files.len().saturating_sub(1));
            self.focused = self.files.get(index).map(|f| f.key().to_string());
        }
    }

    fn update_filter_regex(&mut self) {
        match self.filter_mode {
            FilterMode::QuickSearch => {
                self.filter_regex = self
                    .filter
                    .as_ref()
                    .map(|f| regex::Regex::new(&format!("(?i)^{}", regex::escape(f))).unwrap());
            }
            FilterMode::Filter => {
                self.filter_regex = self.filter.as_ref().and_then(|f| {
                    regex::RegexBuilder::new(f)
                        .case_insensitive(true)
                        .build()
                        .ok()
                });
            }
        }
    }

    fn clear_filter(&mut self) {
        let was_visual = self.filter_mode == FilterMode::Filter;
        self.filter = None;
        self.filter_regex = None;
        self.filter_mode = self.default_filter_mode;
        if was_visual {
            self.files = self.all_files.clone();
            self.file_lookup = self
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| (file.key().to_string(), index))
                .collect();
            self.file_generation += 1;
        }
    }

    /// Clear only if in QuickSearch mode; Filter mode persists across selection changes.
    fn clear_quick_search(&mut self) {
        if self.filter_mode != FilterMode::Filter {
            self.clear_filter();
        }
    }

    fn apply_visual_filter(&mut self) {
        if self.filter_mode != FilterMode::Filter {
            return;
        }

        self.files = self
            .all_files
            .iter()
            .filter(|f| {
                f.name == PARENT_KEY
                    || self
                        .filter_regex
                        .as_ref()
                        .is_none_or(|re| re.is_match(&f.name))
            })
            .cloned()
            .collect();

        self.file_lookup = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.key().to_string(), index))
            .collect();
        self.file_generation += 1;

        // If current focus is no longer visible, pick a new one
        if self
            .focused
            .as_ref()
            .is_none_or(|name| !self.file_lookup.contains_key(name))
        {
            self.focused = self.files.first().map(|f| f.key().to_string());
        }
    }

    // Public API
    pub fn update(
        &mut self,
        display_options: DisplayOptionsInner,
        preferences: &crate::preferences::schema::AppPreferences,
        file_list: &FileList,
    ) {
        // the path is expected to be canonical by now

        self.default_filter_mode = if preferences.behavior.quick_search {
            FilterMode::QuickSearch
        } else {
            FilterMode::Filter
        };

        // If no filter is active, adopt the default mode so the frontend
        // renders the correct input from the start.
        if self.filter.is_none() {
            self.filter_mode = self.default_filter_mode;
        }
        if self.path != *file_list.path() {
            // Path changed: drop the retained focus position (it's only
            // meaningful for an in-place refresh of the same directory).
            self.focused_index = None;
        }
        self.path = file_list.path().clone();
        self.fs_stats = file_list.fs_stats().cloned();
        self.hidden_count = if display_options.show_hidden {
            0
        } else {
            file_list
                .files()
                .iter()
                .filter(|f| f.is_hidden && f.name != PARENT_KEY)
                .count()
        };
        self.files = file_list
            .files()
            .iter()
            .filter(|f| !f.is_hidden || display_options.show_hidden || f.name == PARENT_KEY)
            .cloned()
            .collect();

        self.sort(preferences.appearance.folders_first);
        self.all_files = self.files.clone();
        self.apply_visual_filter();
        self.update_focus();
        self.recompute_stats();
    }

    pub fn focus(&mut self, filename: String) {
        self.clear_quick_search();
        self.focused = Some(filename);
        self.update_focus();
        self.recompute_focused_index_and_viewport();
    }

    pub fn focus_descendant(&mut self, descendant: &VfsPath) {
        if descendant.vfs_id != self.path.vfs_id {
            return;
        }
        if descendant.path.depth() > self.path.path.depth()
            && descendant.path.starts_with(&self.path.path)
            && let Some(next) = descendant.path.components().nth(self.path.path.depth())
        {
            self.focus(next.to_string());
        }
    }

    pub fn set_sorting(&mut self, sorting: Sorting, folders_first: bool) {
        self.sorting = sorting;
        self.sort(folders_first);
        self.recompute_stats();
    }

    pub fn toggle_selected(&mut self, filename: Option<String>, focus_next: bool) {
        self.drag_base = None;
        let Some(filename) = filename.as_ref().or(self.focused.as_ref()).cloned() else {
            return;
        };

        if !self.all_selected.remove(&filename) && self.file_lookup.contains_key(&filename) {
            self.all_selected.insert(filename.clone());
        }
        self.all_selected.remove(PARENT_KEY);

        self.clear_quick_search();
        if focus_next {
            self.relative_jump(1, false);
        } else {
            self.focused = Some(filename);
            self.update_focus();
        }
        self.recompute_stats();
    }

    pub fn select_range(&mut self, filename: String) {
        self.drag_base = None;
        self.clear_quick_search();
        let Some(&start_index) = self
            .focused
            .as_deref()
            .map(|f| self.file_lookup.get(f).unwrap())
        else {
            return;
        };

        let Some(&end_index) = self.file_lookup.get(&filename) else {
            return;
        };

        for i in start_index.min(end_index)..=start_index.max(end_index) {
            self.all_selected.insert(self.files[i].key().to_string());
        }
        self.all_selected.remove(PARENT_KEY);

        self.focused = Some(filename);
        self.recompute_stats();
    }

    pub fn select_all(&mut self) {
        self.drag_base = None;
        self.clear_quick_search();
        self.all_selected = self.file_lookup.keys().cloned().collect();
        // ".." is its own key (unset → falls back to name) so this still works.
        self.all_selected.remove(PARENT_KEY);
        self.recompute_stats();
    }

    pub fn deselect_all(&mut self) {
        self.drag_base = None;
        self.clear_quick_search();
        self.all_selected.clear();
        self.recompute_stats();
    }

    /// Toggle the selection state of every visible entry. Entries hidden
    /// by an active visual filter keep their selection (unlike
    /// `select_all`, which replaces the set wholesale) — inverting is a
    /// per-entry operation, so it only touches what the user can see.
    pub fn invert_selection(&mut self) {
        self.drag_base = None;
        self.clear_quick_search();
        for key in self.file_lookup.keys() {
            if key == PARENT_KEY {
                continue;
            }
            if !self.all_selected.remove(key) {
                self.all_selected.insert(key.clone());
            }
        }
        self.recompute_stats();
    }

    pub fn end_drag_selection(&mut self) {
        self.drag_base = None;
    }

    pub fn set_selection_by_indices(&mut self, start: usize, end: usize, additive: bool) {
        self.clear_quick_search();

        // For additive (Ctrl+drag): auto-snapshot the base on first call,
        // then union base + range.
        let mut selected: HashSet<String> = if additive {
            self.drag_base
                .get_or_insert_with(|| self.all_selected.clone())
                .clone()
        } else {
            self.drag_base = None;
            HashSet::new()
        };

        if let Some(last) = self.files.len().checked_sub(1) {
            let lo = start.min(end).min(last);
            let hi = start.max(end).min(last);
            for i in lo..=hi {
                if self.files[i].name != PARENT_KEY {
                    selected.insert(self.files[i].key().to_string());
                }
            }
        }
        self.all_selected = selected;
        self.recompute_stats();
    }

    pub fn set_selection(&mut self, selected: HashSet<String>, focused: Option<String>) {
        self.drag_base = None;
        self.clear_quick_search();
        self.all_selected = selected;
        self.all_selected.remove(PARENT_KEY);
        if let Some(ref f) = focused
            && self.file_lookup.contains_key(f)
        {
            self.focused = Some(f.clone());
        }
        self.recompute_stats();
    }

    pub fn set_filter(&mut self, filter: Option<String>) {
        self.set_filter_with_mode(filter, self.filter_mode);
    }

    pub fn set_filter_with_mode(&mut self, filter: Option<String>, mode: FilterMode) {
        let Some(filter) = filter else {
            self.clear_filter();
            self.update_focus();
            self.recompute_stats();
            return;
        };

        self.filter_mode = mode;

        match mode {
            FilterMode::QuickSearch => {
                let start_index = *self
                    .focused
                    .as_deref()
                    .map(|f| self.file_lookup.get(f).unwrap())
                    .unwrap_or(&0);

                let new_filter =
                    regex::Regex::new(&format!("(?i)^{}", regex::escape(&filter))).unwrap();

                // Search in the down direction first
                for f in self.files.iter().skip(start_index) {
                    if new_filter.is_match(&f.name) {
                        self.focused = Some(f.key().to_string());
                        self.filter = Some(filter);
                        self.filter_regex = Some(new_filter);
                        self.recompute_stats();
                        return;
                    }
                }

                // Then search in the up direction
                for f in self.files.iter().take(start_index).rev() {
                    if new_filter.is_match(&f.name) {
                        self.focused = Some(f.key().to_string());
                        self.filter = Some(filter);
                        self.filter_regex = Some(new_filter);
                        self.recompute_stats();
                        return;
                    }
                }

                if self.filter.is_none() {
                    self.filter = Some(Default::default());
                    self.update_filter_regex();
                }
                self.recompute_stats();
            }
            FilterMode::Filter => {
                self.filter = Some(filter);
                self.update_filter_regex();
                self.apply_visual_filter();
                self.recompute_stats();
            }
        }
    }

    pub fn relative_jump(&mut self, mut offset: i32, with_selection: bool) {
        let direction = offset.signum();

        if self.files.is_empty() {
            return;
        }

        let mut new_index = self
            .focused
            .as_ref()
            .map(|focused| *self.file_lookup.get(focused).unwrap() as i32)
            .unwrap_or(0);

        let mut i = new_index;
        if with_selection {
            self.all_selected
                .insert(self.files[new_index as usize].key().to_string());
        }
        self.all_selected.remove(PARENT_KEY);

        loop {
            i += direction;
            if i < 0 || i >= (self.files.len() as i32) || offset == 0 {
                break;
            }
            if self.filter_mode == FilterMode::Filter
                || self
                    .filter_regex
                    .as_ref()
                    .is_none_or(|re| re.is_match(&self.files[i as usize].name))
            {
                new_index = i;
                offset -= direction;
            }
        }

        self.focused = Some(self.files[new_index as usize].key().to_string());
        if with_selection {
            self.recompute_stats();
        } else {
            self.recompute_focused_index_and_viewport();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_state(focused: Option<&str>, selected: &[&str]) -> PaneViewState {
        PaneViewState {
            focused: focused.map(str::to_string),
            all_selected: selected.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The bug: `..` never enters `all_selected`, but the fall back to
    /// `focused` had no such gate — so Delete on `..` prompted for it.
    #[test]
    fn focused_parent_is_not_actionable() {
        let vs = view_state(Some(PARENT_KEY), &[]);
        assert!(Pane::effective_keys(&vs).is_empty());
    }

    #[test]
    fn focused_regular_entry_is_actionable() {
        let vs = view_state(Some("file.txt"), &[]);
        assert_eq!(Pane::effective_keys(&vs), vec!["file.txt".to_string()]);
    }

    /// A selection wins over focus, and `..` is stripped even if it somehow
    /// got in — the guarantee is the accessor's, not the mutators'.
    #[test]
    fn parent_is_stripped_from_a_selection() {
        let vs = view_state(Some("a.txt"), &[PARENT_KEY, "b.txt"]);
        assert_eq!(Pane::effective_keys(&vs), vec!["b.txt".to_string()]);
    }

    /// Focus is not merged into a non-empty selection.
    #[test]
    fn selection_excludes_the_focused_entry() {
        let vs = view_state(Some("focused.txt"), &["picked.txt"]);
        assert_eq!(Pane::effective_keys(&vs), vec!["picked.txt".to_string()]);
    }

    /// A selection of only `..` is the same as no selection at all, and the
    /// focus fallback must not resurrect it.
    #[test]
    fn parent_only_selection_yields_nothing() {
        let vs = view_state(Some(PARENT_KEY), &[PARENT_KEY]);
        assert!(Pane::effective_keys(&vs).is_empty());
    }

    /// F2 renames what's under the cursor, ignoring the selection — so it
    /// reads focus directly and needs its own gate.
    #[test]
    fn actionable_focus_rejects_parent() {
        assert_eq!(view_state(Some(PARENT_KEY), &[]).actionable_focus(), None);
    }

    #[test]
    fn actionable_focus_passes_regular_entry() {
        let vs = view_state(Some("file.txt"), &[]);
        assert_eq!(vs.actionable_focus(), Some(&"file.txt".to_string()));
    }

    /// Focus-based actions ignore the selection entirely.
    #[test]
    fn actionable_focus_ignores_selection() {
        let vs = view_state(Some(PARENT_KEY), &["a.txt", "b.txt"]);
        assert_eq!(vs.actionable_focus(), None);
    }

    /// In filter mode, selected-but-hidden entries don't piggyback.
    #[test]
    fn filter_mode_drops_entries_outside_the_view() {
        let mut vs = view_state(None, &["visible.txt", "hidden.txt"]);
        vs.filter_mode = FilterMode::Filter;
        vs.file_lookup = [("visible.txt".to_string(), 0)].into_iter().collect();
        assert_eq!(Pane::effective_keys(&vs), vec!["visible.txt".to_string()]);
    }
}
