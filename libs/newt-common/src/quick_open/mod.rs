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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::Instant;

use crate::Error;
use crate::rpc::Communicator;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::walk::{self, Control, Entry, Failed, Resume, Visitor, WalkOptions};
use crate::vfs::{File, Vfs, VfsPath, VfsRegistry};

/// Results sent per update.
pub const TOP: usize = 100;

/// Updates while the walk goes on are at most this frequent; a new query
/// is answered at once.
const THROTTLE: Duration = Duration::from_millis(100);

/// Larger ignore files are not read.
const MAX_IGNORE_FILE: u64 = 1 << 20;

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

/// Correlates a remote run's updates and query changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuickOpenId(pub u64);

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
        ignores: HashMap::new(),
        walked: 0,
        inbox,
        arrived,
    };
    if options.gitignore {
        collector.ignores = ignores_above(vfs, &root).await;
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
    /// Ignore rules by the directory they're in, by wire path.
    ignores: HashMap<String, Gitignore>,
    walked: u32,
    inbox: &'a Mutex<Inbox>,
    arrived: &'a Notify,
}

impl Collector<'_> {
    /// Whether the nearest rule about `path` among its directories' says
    /// to ignore it.
    fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if let Some(rules) = self.ignores.get(d.as_wire_str()) {
                let below = path.strip_prefix(d).unwrap_or_default();
                let found = rules.matched(below, is_dir);
                if found.is_ignore() {
                    return true;
                }
                if found.is_whitelist() {
                    return false;
                }
            }
            dir = d.parent();
        }
        false
    }
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
            || (self.options.gitignore && self.ignored(entry.path, entry.file.is_dir))
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
        if !self.options.gitignore {
            return Ok(());
        }
        let has = |name: &str| children.iter().any(|f| f.name == name);
        let mut files = Vec::new();
        if has(".gitignore") {
            files.push(path.join(".gitignore"));
        }
        if has(".git") {
            files.push(path.join(".git/info/exclude"));
        }
        if let Some(rules) = read_ignores(self.vfs, &files).await {
            self.ignores.insert(path.as_wire_str().to_string(), rules);
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

/// The rules in `files`, which are all relative to the same directory;
/// `None` when there are none.
async fn read_ignores(vfs: &dyn Vfs, files: &[PathBuf]) -> Option<Gitignore> {
    if files.is_empty() {
        return None;
    }
    let mut builder = GitignoreBuilder::new("");
    let mut any = false;
    for file in files {
        let Ok(chunk) = vfs.read_range(file, 0, MAX_IGNORE_FILE).await else {
            continue;
        };
        for line in String::from_utf8_lossy(&chunk.data).lines() {
            if builder.add_line(None, line).is_ok() {
                any = true;
            }
        }
    }
    any.then(|| builder.build().ok()).flatten()
}

/// The rules of the directories above `root` up to the top of the
/// repository it's in, if it's in one.
async fn ignores_above(vfs: &dyn Vfs, root: &Path) -> HashMap<String, Gitignore> {
    let ancestors: Vec<&Path> = std::iter::successors(root.parent(), |p| p.parent()).collect();
    let tops = futures::future::join_all(
        ancestors
            .iter()
            .map(|dir| async move { vfs.file_info(&dir.join(".git")).await.is_ok() }),
    )
    .await;
    let Some(top) = tops.iter().position(|&is_top| is_top) else {
        return HashMap::new();
    };
    let mut ignores = HashMap::new();
    for (i, dir) in ancestors[..=top].iter().enumerate() {
        let mut files = vec![dir.join(".gitignore")];
        if i == top {
            files.push(dir.join(".git/info/exclude"));
        }
        if let Some(rules) = read_ignores(vfs, &files).await {
            ignores.insert(dir.as_wire_str().to_string(), rules);
        }
    }
    ignores
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

pub type PendingQuickOpens = Arc<Mutex<HashMap<QuickOpenId, mpsc::Sender<QuickOpenUpdate>>>>;

/// A run on the other side of the connection: one invoke streaming updates as
/// `API_QUICK_OPEN_UPDATE` notifications, and a signal per query change.
/// Query changes wait for the run's first update, which the agent sends
/// only once it is listening for them.
pub struct Remote {
    communicator: Communicator,
    pending: PendingQuickOpens,
    next_id: std::sync::atomic::AtomicU64,
}

impl Remote {
    pub fn new(communicator: Communicator, pending: PendingQuickOpens) -> Self {
        Self {
            communicator,
            pending,
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
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
        let id = QuickOpenId(
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let (tx, mut rx) = mpsc::channel(4);
        self.pending.lock().insert(id, tx);
        struct Guard {
            id: QuickOpenId,
            pending: PendingQuickOpens,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                self.pending.lock().remove(&self.id);
            }
        }
        let _guard = Guard {
            id,
            pending: self.pending.clone(),
        };

        let request = (id, root, options, query.borrow_and_update().clone());
        let invoke = self
            .communicator
            .invoke::<_, Result<(), Error>>(crate::api::API_QUICK_OPEN, &request);
        tokio::pin!(invoke);
        let mut listening = false;
        loop {
            tokio::select! {
                ret = &mut invoke => return ret?,
                update = rx.recv() => {
                    let update = update.expect("the sender stays in `pending` until this returns");
                    listening = true;
                    if updates.send(update).await.is_err() {
                        return Ok(());
                    }
                }
                changed = query.changed(), if listening => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let text = query.borrow_and_update().clone();
                    self.communicator
                        .signal(crate::api::API_QUICK_OPEN_QUERY, &(id, text))?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
