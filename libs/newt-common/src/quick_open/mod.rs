//! Quick Open: the entries under a directory, ranked against a query as
//! it is typed.
//!
//! The walk is breadth-first, so the nearest entries arrive first, and
//! runs next to the VFS together with the ranking — host-side in local
//! sessions, agent-side in remote ones — so that only the top of the
//! ranking crosses the RPC boundary. A [`QuickOpenClient`] fronts it with
//! [`Local`] and [`Remote`] impls, like the enrichers. A run lasts until
//! its future is dropped; finishing the walk doesn't end it, since the
//! query can still change.

mod index;

use std::sync::Arc;
use std::time::Duration;

use globset::{Glob, GlobSet, GlobSetBuilder};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::Instant;

use crate::Error;
use crate::rpc::Communicator;
use crate::vfs::gitignore::GitIgnores;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::walk::{self, Control, Entry, Failed, Resume, Visitor, WalkOptions};
use crate::vfs::{File, Vfs, VfsPath, VfsRegistry};

/// Results sent per update.
pub const TOP: usize = 100;

/// Updates while the walk goes on are at most this frequent; a new query
/// is answered at once.
const THROTTLE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuickOpenOptions {
    pub show_hidden: bool,
    pub follow_symlinks: bool,
    /// Leave out what `.gitignore` files and `.git/info/exclude` ignore.
    pub gitignore: bool,
    /// Globs on names never shown or descended into.
    pub exclude: Vec<String>,
    /// Entries walked before stopping.
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct QuickOpenHit {
    /// `/`-separated, relative to the root.
    pub rel: String,
    pub is_dir: bool,
    /// Character indices into `rel` that matched, ascending.
    pub highlights: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct QuickOpenUpdate {
    /// The query the results are ranked against.
    pub query: String,
    pub results: Vec<QuickOpenHit>,
    pub matched: u32,
    pub walked: u32,
    pub walking: bool,
    /// Directories that could not be listed.
    pub unreadable: u32,
    /// The walk stopped at the limit.
    pub truncated: bool,
    /// Why the root itself could not be walked.
    pub error: Option<String>,
}

/// What the walk has found and not yet handed to the ranking.
#[derive(Default)]
struct Inbox {
    entries: Vec<(String, bool)>,
    unreadable: u32,
    truncated: bool,
}

/// Walks and ranks, next to the `VfsRegistry`.
pub struct QuickOpen {
    registry: Arc<VfsRegistry>,
}

impl QuickOpen {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self { registry }
    }

    /// Walk `root` and send the top of the ranking against `query` into
    /// `updates` whenever it changes. Ends when `query`'s sender or
    /// `updates`' receiver goes away, or when dropped.
    pub async fn run(
        &self,
        root: VfsPath,
        options: QuickOpenOptions,
        mut query: watch::Receiver<String>,
        updates: mpsc::Sender<QuickOpenUpdate>,
    ) -> Result<(), Error> {
        let (vfs, path) = self.registry.resolve(&root)?;
        let inbox = Mutex::new(Inbox::default());
        let arrived = Notify::new();
        let walk = collect(&*vfs, path, &options, &inbox, &arrived);
        tokio::pin!(walk);

        let mut index = index::Index::new();
        let mut walking = true;
        let mut error = None;
        let mut query_changed = true;
        let mut dirty = true;
        let mut next_update = Instant::now();
        let mut counts = Inbox::default();
        loop {
            if query_changed || (dirty && Instant::now() >= next_update) {
                let taken = std::mem::take(&mut *inbox.lock());
                counts.unreadable += taken.unreadable;
                counts.truncated |= taken.truncated;
                let text = query.borrow_and_update().clone();
                let (returned, results) = tokio::task::spawn_blocking(move || {
                    for (rel, is_dir) in taken.entries {
                        index.push(rel, is_dir);
                    }
                    index.set_query(&text);
                    index.catch_up();
                    let results = index.top(TOP);
                    (index, results)
                })
                .await
                .expect("ranking does not panic");
                index = returned;
                let update = QuickOpenUpdate {
                    query: index.query().to_string(),
                    results,
                    matched: index.matched() as u32,
                    walked: index.len() as u32,
                    walking,
                    unreadable: counts.unreadable,
                    truncated: counts.truncated,
                    error: error.clone(),
                };
                if updates.send(update).await.is_err() {
                    return Ok(());
                }
                query_changed = false;
                dirty = false;
                next_update = Instant::now() + THROTTLE;
            }
            tokio::select! {
                changed = query.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    query_changed = true;
                }
                result = &mut walk, if walking => {
                    walking = false;
                    dirty = true;
                    next_update = Instant::now();
                    match result {
                        Ok(()) => {}
                        Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                        Err(e) => error = Some(e.message),
                    }
                }
                _ = arrived.notified(), if walking => dirty = true,
                _ = tokio::time::sleep_until(next_update), if dirty => {}
            }
        }
    }
}

/// Walk `root` breadth-first into `inbox`, nudging `arrived` per entry.
async fn collect(
    vfs: &dyn Vfs,
    root: PathBuf,
    options: &QuickOpenOptions,
    inbox: &Mutex<Inbox>,
    arrived: &Notify,
) -> Result<(), Error> {
    let mut exclude = GlobSetBuilder::new();
    for pattern in &options.exclude {
        match Glob::new(pattern) {
            Ok(glob) => {
                exclude.add(glob);
            }
            Err(e) => log::debug!("quick open: exclude {pattern:?}: {e}"),
        }
    }
    let mut collector = Collector {
        vfs,
        options,
        exclude: exclude.build().unwrap_or_else(|_| GlobSet::empty()),
        root_rel: root.file_name().unwrap_or_default().to_string(),
        ignores: GitIgnores::default(),
        walked: 0,
        inbox,
        arrived,
    };
    if options.gitignore {
        collector.ignores = GitIgnores::above(vfs, &root).await;
    }
    let walk_options = WalkOptions {
        follow_symlinks: options.follow_symlinks,
        follow_root: true,
        one_file_system: true,
        excludes: vec![PathBuf::from_wire_str("/proc")],
    };
    walk::walk_breadth_first(vfs, &[root], &walk_options, &mut collector).await
}

struct Collector<'a> {
    vfs: &'a dyn Vfs,
    options: &'a QuickOpenOptions,
    exclude: GlobSet,
    root_rel: String,
    ignores: GitIgnores,
    walked: u32,
    inbox: &'a Mutex<Inbox>,
    arrived: &'a Notify,
}

#[async_trait::async_trait]
impl Visitor for Collector<'_> {
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, Error> {
        if entry.depth == 0 {
            return Ok(Control::Descend);
        }
        let name = entry.rel.rsplit('/').next().unwrap_or(entry.rel);
        if name == ".git"
            || (!self.options.show_hidden && entry.file.is_hidden)
            || self.exclude.is_match(name)
            || (self.options.gitignore && self.ignores.ignored(entry.path, entry.file.is_dir))
        {
            return Ok(Control::Skip);
        }
        if self.walked >= self.options.limit {
            self.inbox.lock().truncated = true;
            return Ok(Control::Stop);
        }
        self.walked += 1;
        let rel = match entry.rel.strip_prefix(self.root_rel.as_str()) {
            Some(below) if !self.root_rel.is_empty() => below.trim_start_matches('/'),
            _ => entry.rel,
        };
        self.inbox
            .lock()
            .entries
            .push((rel.to_string(), entry.file.is_dir));
        self.arrived.notify_one();
        Ok(Control::Descend)
    }

    async fn listed(&mut self, path: &Path, children: &[File]) -> Result<(), Error> {
        if self.options.gitignore {
            self.ignores.learn(self.vfs, path, children).await;
        }
        Ok(())
    }

    async fn failed(&mut self, what: Failed<'_>, _error: Error) -> Result<Resume, Error> {
        if matches!(what, Failed::Listing(_)) {
            self.inbox.lock().unreadable += 1;
        }
        Ok(Resume::Skip)
    }
}

// ---------------------------------------------------------------------------
// QuickOpenClient — Local / Remote
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait QuickOpenClient: Send + Sync {
    /// [`QuickOpen::run`] wherever the session's filesystem is. Cancel by
    /// dropping the future.
    async fn run(
        &self,
        root: VfsPath,
        options: QuickOpenOptions,
        query: watch::Receiver<String>,
        updates: mpsc::Sender<QuickOpenUpdate>,
    ) -> Result<(), Error>;
}

pub struct Local {
    quick_open: Arc<QuickOpen>,
}

impl Local {
    pub fn new(quick_open: Arc<QuickOpen>) -> Self {
        Self { quick_open }
    }
}

#[async_trait::async_trait]
impl QuickOpenClient for Local {
    async fn run(
        &self,
        root: VfsPath,
        options: QuickOpenOptions,
        query: watch::Receiver<String>,
        updates: mpsc::Sender<QuickOpenUpdate>,
    ) -> Result<(), Error> {
        self.quick_open.run(root, options, query, updates).await
    }
}

/// A run on the other side of the connection: one invoke streaming updates
/// down a stream it opens, and a signal per query change naming that
/// stream. Query changes wait for the run's first update, which the agent
/// sends only once it is listening for them.
pub struct Remote {
    communicator: Communicator,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl QuickOpenClient for Remote {
    async fn run(
        &self,
        root: VfsPath,
        options: QuickOpenOptions,
        mut query: watch::Receiver<String>,
        updates: mpsc::Sender<QuickOpenUpdate>,
    ) -> Result<(), Error> {
        let (stream_id, mut rx) = self.communicator.open_stream();
        let request = (stream_id, root, options, query.borrow_and_update().clone());
        let invoke = self
            .communicator
            .invoke::<_, Result<(), Error>>(crate::api::API_QUICK_OPEN, &request);
        tokio::pin!(invoke);
        let mut listening = false;
        let mut streaming = true;
        loop {
            tokio::select! {
                ret = &mut invoke => return ret?,
                update = rx.recv_item(), if streaming => {
                    let Some(update) = update else {
                        streaming = false;
                        continue;
                    };
                    listening = true;
                    if updates.send(update?).await.is_err() {
                        return Ok(());
                    }
                }
                changed = query.changed(), if listening => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let text = query.borrow_and_update().clone();
                    self.communicator
                        .signal(crate::api::API_QUICK_OPEN_QUERY, &(stream_id, text))?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
