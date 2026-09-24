//! Depth-first walk of one VFS's subtrees, reported to a [`Visitor`].
//!
//! Every tree traversal in the crate — copy planning, delete, recursive
//! attributes, du, search — drives this one loop, so the rules live in
//! one place: `..` rows dropped, a directory symlink entered only in
//! follow mode and never twice, a mount point recognized by its device,
//! an excluded subtree never listed, an unreadable directory the
//! visitor's call. The walk sees a single `Vfs`, so anything mounted
//! over it in the registry is invisible by construction.
//!
//! A root is listed flat when its VFS advertises `can_list_recursive`
//! (an object store's one listing per prefix); the tree's enter/leave
//! events are then synthesized from the paths as the batches arrive, so
//! a visitor sees the same sequence either way. Such a VFS has no
//! symlinks, so follow mode changes nothing there.
//!
//! Directory-at-a-time walks list ahead: up to [`PREFETCH`] directories
//! still to come in depth-first order are being listed while the visitor
//! works on the current one, so a networked VFS overlaps its round
//! trips. Events are not reordered; only the I/O is.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::mpsc;

use super::path::{Path, PathBuf};
use super::{File, FlatEntry, MAX_SYMLINK_HOPS, Vfs};
use crate::{Error, ErrorKind};

/// Directories listed ahead of the walk, per walk.
pub const PREFETCH: usize = 4;

#[derive(Debug, Default, Clone)]
pub struct WalkOptions {
    /// Enter directories reached through symlinks. Targets are resolved
    /// on the VFS; a target already on the path from the root, or a chain
    /// longer than [`MAX_SYMLINK_HOPS`], is reported as a cycle instead.
    pub follow_symlinks: bool,
    /// Stop at mount points (rsync's `-x`, `du -x`). A root that is
    /// itself a mount point is always walked.
    pub one_file_system: bool,
    /// Paths omitted together with everything under them: an archive
    /// being written into its own source, `/proc` for a whole-filesystem
    /// walk.
    pub excludes: Vec<PathBuf>,
}

/// One entry of the walk, as the visitor sees it.
pub struct Entry<'a> {
    pub path: &'a Path,
    /// The root's file name, then dirent names down the tree,
    /// `/`-joined: what a copy places under its destination.
    pub rel: &'a str,
    /// 0 for a root.
    pub depth: usize,
    /// In follow mode a link's *target*; the link's own name is in `rel`.
    pub file: &'a File,
    /// A directory on another filesystem than its parent's.
    pub mount_point: bool,
}

/// The visitor's answer to an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Enter the directory; ignored for anything else.
    Descend,
    Skip,
    /// End the walk; nothing further is reported.
    Stop,
}

/// The visitor's answer to a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    Retry,
    /// Carry on as if the directory were empty or the link absent.
    Skip,
}

#[derive(Debug, Clone, Copy)]
pub enum Failed<'a> {
    /// The directory could not be listed.
    Listing(&'a Path),
    /// The symlink could not be resolved (follow mode).
    Following(&'a Path),
}

#[async_trait]
pub trait Visitor: Send {
    /// Every entry, directories before their contents.
    async fn entry(&mut self, entry: Entry<'_>) -> Result<Control, Error>;

    /// A directory whose contents have all been reported, including one
    /// whose listing failed and was skipped.
    async fn leave(&mut self, path: &Path, file: &File) -> Result<(), Error> {
        let _ = (path, file);
        Ok(())
    }

    /// Skipping is the default; the walk has already logged the error.
    async fn failed(&mut self, what: Failed<'_>, error: Error) -> Result<Resume, Error> {
        let _ = (what, error);
        Ok(Resume::Skip)
    }

    /// A symlink chain that loops or runs too deep (follow mode). The
    /// entry is not reported.
    async fn cycle(&mut self, path: &Path) -> Result<(), Error> {
        let _ = path;
        Ok(())
    }
}

/// Whether a directory listed under a parent on `parent_device` is the
/// root of another filesystem. Only the local filesystem reports devices,
/// so nothing is ever a mount point elsewhere. Bind mounts share their
/// source's device and go unnoticed; btrfs subvolumes have their own and
/// count, as they do for `rm --one-file-system`.
pub fn is_mount_point(parent_device: Option<u64>, file: &File) -> bool {
    file.is_dir
        && !file.is_symlink
        && matches!((parent_device, file.device_id), (Some(p), Some(d)) if p != d)
}

/// The device of a root's parent, for telling whether the root is itself
/// a mount point. Asked only when the root reports a device at all.
pub async fn parent_device(vfs: &dyn Vfs, path: &Path, file: &File) -> Option<u64> {
    file.device_id?;
    let parent = path.parent()?;
    vfs.file_info(parent).await.ok().and_then(|f| f.device_id)
}

/// The entry for a root. A stat where the VFS has one; the parent's
/// listing where directories are not objects (S3). A VFS root is a
/// directory by definition.
pub async fn root_entry(vfs: &dyn Vfs, path: &Path) -> Result<File, Error> {
    if vfs.descriptor().can_stat_directories() {
        return vfs.file_info(path).await;
    }
    let Some(name) = path.file_name() else {
        return Ok(File::bare_dir(""));
    };
    let parent = path
        .parent()
        .map(Path::to_owned)
        .unwrap_or_else(PathBuf::root);
    vfs.list_files(&parent, None)
        .await?
        .files
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| Error::custom(format!("source not found: {}", path)))
}

/// Walk `roots` in order, depth-first. A root that cannot be classified
/// fails the walk; everything below it goes through the visitor.
pub async fn walk(
    vfs: &dyn Vfs,
    roots: &[PathBuf],
    options: &WalkOptions,
    visitor: &mut dyn Visitor,
) -> Result<(), Error> {
    let mut walker = Walker {
        vfs,
        options,
        visitor,
        stack: Vec::new(),
        inflight: FuturesUnordered::new(),
        pending: HashSet::new(),
        ready: HashMap::new(),
    };
    for root in roots {
        let file = root_entry(vfs, root).await?;
        let parent_device = parent_device(vfs, root, &file).await;
        let child = Child {
            path: root.clone(),
            rel: root.file_name().unwrap_or_default().to_string(),
            file,
            parent_device,
            ancestry: Arc::new(Vec::new()),
            root: true,
        };
        if walker.visit(child, 0).await? == Flow::Stop {
            return Ok(());
        }
        while let Some(frame) = walker.stack.last_mut() {
            let Some(file) = frame.children.next() else {
                let frame = walker.stack.pop().unwrap();
                walker.forget_under(&frame.path);
                walker.visitor.leave(&frame.path, &frame.file).await?;
                continue;
            };
            if file.name == ".." {
                continue;
            }
            let depth = frame.depth + 1;
            let child = Child {
                path: frame.path.join(&file.name),
                rel: if frame.rel.is_empty() {
                    file.name.clone()
                } else {
                    format!("{}/{}", frame.rel, file.name)
                },
                file,
                parent_device: frame.file.device_id,
                ancestry: frame.ancestry.clone(),
                root: false,
            };
            walker.list_ahead();
            if walker.visit(child, depth).await? == Flow::Stop {
                return Ok(());
            }
        }
    }
    Ok(())
}

type Listing = Result<Vec<File>, Error>;
type ListingAhead<'a> = Pin<Box<dyn Future<Output = (String, Listing)> + Send + 'a>>;

struct Walker<'a> {
    vfs: &'a dyn Vfs,
    options: &'a WalkOptions,
    visitor: &'a mut dyn Visitor,
    stack: Vec<Frame>,
    /// Listings started ahead of the walk, keyed by wire path on
    /// completion.
    inflight: FuturesUnordered<ListingAhead<'a>>,
    /// Keys of the listings in flight.
    pending: HashSet<String>,
    /// Listings that completed before the walk reached them.
    ready: HashMap<String, Listing>,
}

/// A directory being listed out.
struct Frame {
    path: PathBuf,
    rel: String,
    depth: usize,
    file: File,
    /// Normalized targets of the directory symlinks followed to get here.
    ancestry: Arc<Vec<PathBuf>>,
    children: std::vec::IntoIter<File>,
}

/// An entry about to be reported.
struct Child {
    path: PathBuf,
    rel: String,
    file: File,
    parent_device: Option<u64>,
    ancestry: Arc<Vec<PathBuf>>,
    root: bool,
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

impl Walker<'_> {
    async fn visit(&mut self, mut child: Child, depth: usize) -> Result<Flow, Error> {
        if self
            .options
            .excludes
            .iter()
            .any(|e| child.path.starts_with(e))
        {
            return Ok(Flow::Continue);
        }
        if self.options.follow_symlinks && child.file.is_symlink {
            let mut ancestry = (*child.ancestry).clone();
            let mut hops = 0;
            while child.file.is_symlink {
                if hops >= MAX_SYMLINK_HOPS {
                    self.visitor.cycle(&child.path).await?;
                    return Ok(Flow::Continue);
                }
                let (target, file) = loop {
                    let resolved = async {
                        let target = self.vfs.resolve_link(&child.path).await?;
                        let file = self.vfs.file_info(&target).await?;
                        Ok::<_, Error>((target, file))
                    }
                    .await;
                    match resolved {
                        Ok(resolved) => break resolved,
                        Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                        Err(e) => {
                            log::debug!("walk: cannot follow {}: {}", child.path, e);
                            match self
                                .visitor
                                .failed(Failed::Following(&child.path), e)
                                .await?
                            {
                                Resume::Retry => {}
                                Resume::Skip => return Ok(Flow::Continue),
                            }
                        }
                    }
                };
                if ancestry.contains(&target) || (file.is_dir && child.path.starts_with(&target)) {
                    self.visitor.cycle(&child.path).await?;
                    return Ok(Flow::Continue);
                }
                ancestry.push(target.clone());
                child.path = target;
                child.file = file;
                hops += 1;
            }
            child.ancestry = Arc::new(ancestry);
        }

        let mount_point = is_mount_point(child.parent_device, &child.file);
        let control = self
            .visitor
            .entry(Entry {
                path: &child.path,
                rel: &child.rel,
                depth,
                file: &child.file,
                mount_point,
            })
            .await?;
        let enter = match control {
            Control::Stop => return Ok(Flow::Stop),
            Control::Skip => false,
            Control::Descend => {
                child.file.is_dir
                    && !child.file.is_symlink
                    && (child.root || !(mount_point && self.options.one_file_system))
            }
        };
        if !enter {
            return Ok(Flow::Continue);
        }

        if child.root && self.vfs.descriptor().can_list_recursive() {
            return self.flat(&child, depth).await;
        }
        let children = loop {
            match self.listing(&child.path).await {
                Ok(files) => break files,
                Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
                Err(e) => {
                    log::debug!("walk: cannot list {}: {}", child.path, e);
                    match self.visitor.failed(Failed::Listing(&child.path), e).await? {
                        Resume::Retry => {}
                        Resume::Skip => break Vec::new(),
                    }
                }
            }
        };
        self.stack.push(Frame {
            path: child.path,
            rel: child.rel,
            depth,
            file: child.file,
            ancestry: child.ancestry,
            children: children.into_iter(),
        });
        Ok(Flow::Continue)
    }

    /// The listing of `path`: the one started ahead if there is one,
    /// waited for if it is still in flight, fetched now otherwise (a
    /// retry after a failure always fetches).
    async fn listing(&mut self, path: &Path) -> Listing {
        let key = path.as_wire_str();
        if let Some(listing) = self.ready.remove(key) {
            return listing;
        }
        if self.pending.remove(key) {
            while let Some((done, listing)) = self.inflight.next().await {
                if done == key {
                    return listing;
                }
                if self.pending.remove(&done) {
                    self.ready.insert(done, listing);
                }
            }
        }
        self.vfs.list_files(path, None).await.map(|l| l.files)
    }

    /// Start listing the directories next in depth-first order, up to
    /// [`PREFETCH`] at a time counting those already done and waiting.
    fn list_ahead(&mut self) {
        let mut budget = PREFETCH.saturating_sub(self.pending.len() + self.ready.len());
        for frame in self.stack.iter().rev() {
            if budget == 0 {
                break;
            }
            for file in frame.children.as_slice() {
                if budget == 0 {
                    break;
                }
                if file.name == ".."
                    || !file.is_dir
                    || file.is_symlink
                    || (self.options.one_file_system && is_mount_point(frame.file.device_id, file))
                {
                    continue;
                }
                let path = frame.path.join(&file.name);
                if self.options.excludes.iter().any(|e| path.starts_with(e)) {
                    continue;
                }
                let key = path.as_wire_str().to_string();
                if self.pending.contains(&key) || self.ready.contains_key(&key) {
                    continue;
                }
                let vfs = self.vfs;
                self.pending.insert(key.clone());
                self.inflight.push(Box::pin(async move {
                    let listing = vfs.list_files(&path, None).await.map(|l| l.files);
                    (key, listing)
                }));
                budget -= 1;
            }
        }
    }

    /// Drop what was listed ahead under a directory the walk has left:
    /// children the visitor chose not to enter.
    fn forget_under(&mut self, dir: &Path) {
        let under = |key: &String| {
            PathBuf::from_wire_str(key)
                .parent()
                .is_some_and(|p| p.as_wire_str() == dir.as_wire_str())
        };
        self.ready.retain(|key, _| !under(key));
        self.pending.retain(|key| !under(key));
    }

    /// Walk a root through its flat listing. Entries are reported as
    /// the batches arrive: a directory is
    /// opened when its own entry or its first descendant comes up and
    /// closed once a path outside it does. A listing that fails part-way
    /// goes through the visitor; on Retry it is restarted and everything
    /// up to the last path consumed is dropped, which path order makes
    /// exact. Nothing here has a device, so nothing is a mount point.
    async fn flat(&mut self, root: &Child, depth: usize) -> Result<Flow, Error> {
        let vfs = self.vfs;
        let mut state = Flat {
            root_path: root.path.clone(),
            root_rel: root.rel.clone(),
            depth,
            open: vec![(root.path.clone(), root.file.clone())],
            passing: None,
            last: None,
        };
        loop {
            let (tx, mut rx) = mpsc::channel(4);
            let mut producer = std::pin::pin!(vfs.list_recursive(&root.path, tx));
            let mut produced: Option<Result<(), Error>> = None;
            let mut stopped = false;
            loop {
                tokio::select! {
                    result = &mut producer, if produced.is_none() => produced = Some(result),
                    batch = rx.recv() => match batch {
                        Some(batch) => {
                            for entry in batch {
                                if self.flat_entry(&mut state, entry).await? == Flow::Stop {
                                    stopped = true;
                                    break;
                                }
                            }
                            if stopped {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
            if stopped {
                return Ok(Flow::Stop);
            }
            // The sender is gone, so the producer is done or about to be.
            let result = match produced {
                Some(result) => result,
                None => producer.await,
            };
            match result {
                Ok(()) => break,
                Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
                Err(e) => {
                    log::debug!("walk: flat listing of {} failed: {}", root.path, e);
                    match self.visitor.failed(Failed::Listing(&root.path), e).await? {
                        Resume::Retry => {}
                        Resume::Skip => break,
                    }
                }
            }
        }
        while let Some((p, f)) = state.open.pop() {
            self.visitor.leave(&p, &f).await?;
        }
        Ok(Flow::Continue)
    }

    async fn flat_entry(&mut self, state: &mut Flat, entry: FlatEntry) -> Result<Flow, Error> {
        let FlatEntry { path, file } = entry;
        if let Some(last) = &state.last
            && path.as_wire_str() <= last.as_wire_str()
        {
            return Ok(Flow::Continue);
        }
        state.last = Some(path.clone());
        if let Some(p) = &state.passing {
            if path.starts_with(p) {
                return Ok(Flow::Continue);
            }
            state.passing = None;
        }
        if !path.starts_with(&state.root_path)
            || path.as_wire_str() == state.root_path.as_wire_str()
        {
            return Ok(Flow::Continue);
        }
        while !path.starts_with(&state.open.last().unwrap().0) {
            let (p, f) = state.open.pop().unwrap();
            self.visitor.leave(&p, &f).await?;
        }
        // Directories between the innermost open one and this entry.
        let top = state.open.last().unwrap().0.clone();
        let parent = path.parent().map_or("", Path::as_wire_str);
        let mut dir = top.clone();
        let between = path.strip_prefix(&top).unwrap_or_default();
        for name in between.split('/').filter(|s| !s.is_empty()) {
            if dir.as_wire_str() == parent {
                break;
            }
            dir = dir.join(name);
            match self.open_dir(state, &dir, File::bare_dir(name)).await? {
                Opened::Entered => {}
                Opened::Passed => {
                    state.passing = Some(dir);
                    return Ok(Flow::Continue);
                }
                Opened::Stopped => return Ok(Flow::Stop),
            }
        }
        if file.is_dir && !file.is_symlink {
            return Ok(match self.open_dir(state, &path, file).await? {
                Opened::Entered => Flow::Continue,
                Opened::Passed => {
                    state.passing = Some(path);
                    Flow::Continue
                }
                Opened::Stopped => Flow::Stop,
            });
        }
        if self.options.excludes.iter().any(|e| path.starts_with(e)) {
            return Ok(Flow::Continue);
        }
        let rel = rel_under(&state.root_rel, &state.root_path, &path);
        let control = self
            .visitor
            .entry(Entry {
                path: &path,
                rel: &rel,
                depth: state.depth + path.depth() - state.root_path.depth(),
                file: &file,
                mount_point: false,
            })
            .await?;
        Ok(if control == Control::Stop {
            Flow::Stop
        } else {
            Flow::Continue
        })
    }

    /// Report a directory of a flat listing and, when the visitor enters
    /// it, put it on the open stack.
    async fn open_dir(
        &mut self,
        state: &mut Flat,
        path: &Path,
        file: File,
    ) -> Result<Opened, Error> {
        if self.options.excludes.iter().any(|e| path.starts_with(e)) {
            return Ok(Opened::Passed);
        }
        let rel = rel_under(&state.root_rel, &state.root_path, path);
        let control = self
            .visitor
            .entry(Entry {
                path,
                rel: &rel,
                depth: state.depth + path.depth() - state.root_path.depth(),
                file: &file,
                mount_point: false,
            })
            .await?;
        Ok(match control {
            Control::Descend => {
                state.open.push((path.to_owned(), file));
                Opened::Entered
            }
            Control::Skip => Opened::Passed,
            Control::Stop => Opened::Stopped,
        })
    }
}

/// A flat listing in progress.
struct Flat {
    root_path: PathBuf,
    root_rel: String,
    depth: usize,
    /// Directories entered and not yet left, the root at the bottom.
    open: Vec<(PathBuf, File)>,
    /// A subtree being passed over: skipped by the visitor or excluded.
    passing: Option<PathBuf>,
    /// The last path consumed, for dropping a restarted listing's replay.
    last: Option<PathBuf>,
}

enum Opened {
    Entered,
    Passed,
    Stopped,
}

/// `rel` of a path below a root, given the root's own.
fn rel_under(root_rel: &str, root_path: &Path, path: &Path) -> String {
    let below = path.strip_prefix(root_path).unwrap_or_default();
    if root_rel.is_empty() {
        below.to_string()
    } else {
        format!("{root_rel}/{below}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FailureSpec, MockVfs, MockVfsConfig};

    /// Records the walk as one line per callback.
    #[derive(Default)]
    struct Log {
        lines: Vec<String>,
        skip: Vec<&'static str>,
        stop_at: Option<&'static str>,
        retries: u32,
    }

    #[async_trait]
    impl Visitor for Log {
        async fn entry(&mut self, e: Entry<'_>) -> Result<Control, Error> {
            let kind = if e.file.is_symlink {
                "link"
            } else if e.file.is_dir {
                "dir"
            } else {
                "file"
            };
            let mp = if e.mount_point { " mount" } else { "" };
            self.lines.push(format!(
                "{} {} rel={} d={}{}",
                kind, e.path, e.rel, e.depth, mp
            ));
            if self.stop_at == Some(e.path.as_wire_str()) {
                return Ok(Control::Stop);
            }
            if self.skip.contains(&e.path.as_wire_str()) {
                return Ok(Control::Skip);
            }
            Ok(Control::Descend)
        }
        async fn leave(&mut self, path: &Path, _file: &File) -> Result<(), Error> {
            self.lines.push(format!("leave {}", path));
            Ok(())
        }
        async fn failed(&mut self, what: Failed<'_>, error: Error) -> Result<Resume, Error> {
            let (what, path) = match what {
                Failed::Listing(p) => ("list", p),
                Failed::Following(p) => ("follow", p),
            };
            self.lines
                .push(format!("failed {} {}: {}", what, path, error.message));
            if self.retries > 0 {
                self.retries -= 1;
                return Ok(Resume::Retry);
            }
            Ok(Resume::Skip)
        }
        async fn cycle(&mut self, path: &Path) -> Result<(), Error> {
            self.lines.push(format!("cycle {}", path));
            Ok(())
        }
    }

    fn roots(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(|p| PathBuf::from_wire_str(p)).collect()
    }

    async fn run(vfs: &MockVfs, roots_: &[&str], options: WalkOptions, log: Log) -> Vec<String> {
        let mut log = log;
        walk(vfs, &roots(roots_), &options, &mut log).await.unwrap();
        log.lines
    }

    #[tokio::test]
    async fn reports_entries_pre_order_and_directories_post_order() {
        let vfs = MockVfs::builder()
            .dir("/top")
            .file("/top/a.txt", b"a")
            .dir("/top/sub")
            .file("/top/sub/c.txt", b"c")
            .symlink("/top/link", "/top/a.txt")
            .build();
        let lines = run(&vfs, &["/top"], WalkOptions::default(), Log::default()).await;
        assert_eq!(
            lines,
            [
                "dir /top rel=top d=0",
                "file /top/a.txt rel=top/a.txt d=1",
                "link /top/link rel=top/link d=1",
                "dir /top/sub rel=top/sub d=1",
                "file /top/sub/c.txt rel=top/sub/c.txt d=2",
                "leave /top/sub",
                "leave /top",
            ]
        );
    }

    #[tokio::test]
    async fn walks_the_vfs_root_and_several_roots_in_order() {
        let vfs = MockVfs::builder()
            .file("/b.txt", b"b")
            .dir("/a")
            .file("/a/x", b"x")
            .build();
        let lines = run(
            &vfs,
            &["/b.txt", "/a"],
            WalkOptions::default(),
            Log::default(),
        )
        .await;
        assert_eq!(
            lines,
            [
                "file /b.txt rel=b.txt d=0",
                "dir /a rel=a d=0",
                "file /a/x rel=a/x d=1",
                "leave /a",
            ]
        );
        let lines = run(&vfs, &["/"], WalkOptions::default(), Log::default()).await;
        assert_eq!(lines[0], "dir / rel= d=0");
        assert_eq!(lines[1], "dir /a rel=a d=1");
    }

    #[tokio::test]
    async fn skip_leaves_a_directory_unentered_and_stop_ends_the_walk() {
        let vfs = MockVfs::builder()
            .dir("/top/skipped")
            .file("/top/skipped/x", b"x")
            .dir("/top/z")
            .file("/top/z/y", b"y")
            .build();
        let lines = run(
            &vfs,
            &["/top"],
            WalkOptions::default(),
            Log {
                skip: vec!["/top/skipped"],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            lines,
            [
                "dir /top rel=top d=0",
                "dir /top/skipped rel=top/skipped d=1",
                "dir /top/z rel=top/z d=1",
                "file /top/z/y rel=top/z/y d=2",
                "leave /top/z",
                "leave /top",
            ]
        );
        let lines = run(
            &vfs,
            &["/top"],
            WalkOptions::default(),
            Log {
                stop_at: Some("/top/z"),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(lines.last().unwrap(), "dir /top/z rel=top/z d=1");
    }

    #[tokio::test]
    async fn an_unreadable_directory_is_retried_then_skipped_and_still_left() {
        let vfs = MockVfs::builder()
            .dir("/top/bad")
            .file("/top/bad/x", b"x")
            .file("/top/ok", b"o")
            .failure(FailureSpec {
                path: PathBuf::from_wire_str("/top/bad"),
                operation: "list_files",
                error: Error::custom("denied"),
                remaining: None,
            })
            .build();
        let lines = run(
            &vfs,
            &["/top"],
            WalkOptions::default(),
            Log {
                retries: 1,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            lines,
            [
                "dir /top rel=top d=0",
                "dir /top/bad rel=top/bad d=1",
                "failed list /top/bad: denied",
                "failed list /top/bad: denied",
                "leave /top/bad",
                "file /top/ok rel=top/ok d=1",
                "leave /top",
            ]
        );
    }

    #[tokio::test]
    async fn follow_mode_enters_directory_links_once_and_reports_cycles() {
        let vfs = MockVfs::builder()
            .dir("/top/real")
            .file("/top/real/x", b"x")
            .symlink("/top/to_real", "/top/real")
            .symlink("/top/real/up", "/top")
            .symlink("/top/dangling", "/top/nowhere")
            .build();
        let lines = run(&vfs, &["/top"], WalkOptions::default(), Log::default()).await;
        assert!(lines.iter().all(|l| !l.starts_with("file /top/to_real")));

        let lines = run(
            &vfs,
            &["/top"],
            WalkOptions {
                follow_symlinks: true,
                ..Default::default()
            },
            Log::default(),
        )
        .await;
        assert_eq!(
            lines,
            [
                "dir /top rel=top d=0",
                "failed follow /top/dangling: not found: /top/nowhere",
                "dir /top/real rel=top/real d=1",
                "cycle /top/real/up",
                "file /top/real/x rel=top/real/x d=2",
                "leave /top/real",
                "dir /top/real rel=top/to_real d=1",
                "cycle /top/real/up",
                "file /top/real/x rel=top/to_real/x d=2",
                "leave /top/real",
                "leave /top",
            ]
        );
    }

    #[tokio::test]
    async fn mount_points_are_marked_and_crossed_unless_told_otherwise() {
        let vfs = MockVfs::builder()
            .dir("/top")
            .mount_point("/top/mnt")
            .file("/top/mnt/b", b"b")
            .file("/top/a", b"a")
            .build();
        let lines = run(&vfs, &["/top"], WalkOptions::default(), Log::default()).await;
        assert_eq!(
            lines,
            [
                "dir /top rel=top d=0",
                "file /top/a rel=top/a d=1",
                "dir /top/mnt rel=top/mnt d=1 mount",
                "file /top/mnt/b rel=top/mnt/b d=2",
                "leave /top/mnt",
                "leave /top",
            ]
        );
        let one_fs = WalkOptions {
            one_file_system: true,
            ..Default::default()
        };
        let lines = run(&vfs, &["/top"], one_fs.clone(), Log::default()).await;
        assert_eq!(
            lines,
            [
                "dir /top rel=top d=0",
                "file /top/a rel=top/a d=1",
                "dir /top/mnt rel=top/mnt d=1 mount",
                "leave /top",
            ]
        );
        // A root that is a mount point is walked regardless.
        let lines = run(&vfs, &["/top/mnt"], one_fs, Log::default()).await;
        assert_eq!(
            lines,
            [
                "dir /top/mnt rel=mnt d=0 mount",
                "file /top/mnt/b rel=mnt/b d=1",
                "leave /top/mnt",
            ]
        );
    }

    #[tokio::test]
    async fn excludes_cover_their_subtrees() {
        let vfs = MockVfs::builder()
            .dir("/proc/1")
            .file("/proc/1/status", b"")
            .file("/etc/hosts", b"")
            .build();
        let lines = run(
            &vfs,
            &["/"],
            WalkOptions {
                excludes: vec![PathBuf::from_wire_str("/proc")],
                ..Default::default()
            },
            Log::default(),
        )
        .await;
        assert!(lines.iter().all(|l| !l.contains("/proc")), "{lines:?}");
        assert!(lines.contains(&"file /etc/hosts rel=etc/hosts d=2".to_string()));
    }

    /// A tree without empty directories: a flat listing carries no
    /// directories of its own, so an empty one has nothing to be
    /// synthesized from.
    fn flat_tree(flat: bool) -> Arc<MockVfs> {
        MockVfs::builder()
            .config(MockVfsConfig {
                can_stat_directories: false,
                can_list_recursive: flat,
                ..Default::default()
            })
            .file("/top/a.txt", b"a")
            .file("/top/skipped/x", b"x")
            .file("/top/skipped/deep/y", b"y")
            .file("/top/sub/c.txt", b"c")
            .file("/top/sub/d/e/f.txt", b"f")
            .symlink("/top/sub/link", "/top/a.txt")
            .file("/top/z/y", b"y")
            .file("/topmost", b"t")
            .build()
    }

    #[tokio::test]
    async fn a_flat_listing_reports_the_same_sequence_as_the_tree_walk() {
        type Scenario = (&'static [&'static str], WalkOptions, fn() -> Log);
        let scenarios: Vec<Scenario> = vec![
            (&["/top"], WalkOptions::default(), Log::default),
            (&["/top"], WalkOptions::default(), || Log {
                skip: vec!["/top/skipped", "/top/sub/d"],
                ..Default::default()
            }),
            (&["/top"], WalkOptions::default(), || Log {
                stop_at: Some("/top/sub/c.txt"),
                ..Default::default()
            }),
            (
                &["/top"],
                WalkOptions {
                    excludes: vec![
                        PathBuf::from_wire_str("/top/skipped"),
                        PathBuf::from_wire_str("/top/sub/d/e/f.txt"),
                    ],
                    ..Default::default()
                },
                Log::default,
            ),
            (
                &["/top/sub", "/topmost", "/top/z"],
                WalkOptions::default(),
                Log::default,
            ),
            (&["/"], WalkOptions::default(), Log::default),
        ];
        for (roots_, options, log) in scenarios {
            let tree = run(&flat_tree(false), roots_, options.clone(), log()).await;
            let flat = run(&flat_tree(true), roots_, options, log()).await;
            assert_eq!(flat, tree, "roots {roots_:?}");
        }
        // The directories the flat listing never mentioned were
        // synthesized where the tree walk lists them.
        let flat = run(
            &flat_tree(true),
            &["/top"],
            WalkOptions::default(),
            Log::default(),
        )
        .await;
        assert!(flat.contains(&"dir /top/sub/d/e rel=top/sub/d/e d=3".to_string()));
        assert!(flat.contains(&"leave /top/sub/d/e".to_string()));
    }

    #[tokio::test]
    async fn a_failed_flat_listing_is_retried_then_skipped() {
        let vfs = MockVfs::builder()
            .config(MockVfsConfig {
                can_list_recursive: true,
                ..Default::default()
            })
            .file("/top/a", b"a")
            .failure(FailureSpec {
                path: PathBuf::from_wire_str("/top"),
                operation: "list_recursive",
                error: Error::custom("throttled"),
                remaining: None,
            })
            .build();
        let lines = run(
            &vfs,
            &["/top"],
            WalkOptions::default(),
            Log {
                retries: 1,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            lines,
            [
                "dir /top rel=top d=0",
                "failed list /top: throttled",
                "failed list /top: throttled",
                "leave /top",
            ]
        );
    }

    #[tokio::test]
    async fn a_flat_listing_failing_part_way_resumes_after_the_last_path_on_retry() {
        let tree = run(
            &flat_tree(false),
            &["/top"],
            WalkOptions::default(),
            Log::default(),
        )
        .await;
        let vfs = MockVfs::builder()
            .config(MockVfsConfig {
                can_stat_directories: false,
                can_list_recursive: true,
                ..Default::default()
            })
            .file("/top/a.txt", b"a")
            .file("/top/skipped/x", b"x")
            .file("/top/skipped/deep/y", b"y")
            .file("/top/sub/c.txt", b"c")
            .file("/top/sub/d/e/f.txt", b"f")
            .symlink("/top/sub/link", "/top/a.txt")
            .file("/top/z/y", b"y")
            .file("/topmost", b"t")
            .failure(FailureSpec {
                path: PathBuf::from_wire_str("/top"),
                operation: "list_recursive_page",
                error: Error::custom("throttled"),
                remaining: Some(1),
            })
            .build();
        let mut lines = run(
            &vfs,
            &["/top"],
            WalkOptions::default(),
            Log {
                retries: 1,
                ..Default::default()
            },
        )
        .await;
        let failed = lines
            .iter()
            .position(|l| l == "failed list /top: throttled")
            .expect("the failure was reported");
        // The first batch got through before the failure...
        assert!(failed > 1, "{lines:?}");
        lines.remove(failed);
        // ...and the retry reported nothing twice and left nothing out.
        assert_eq!(lines, tree);
    }

    #[tokio::test]
    async fn listings_ahead_overlap_without_reordering_events() {
        let mut builder = MockVfs::builder().config(MockVfsConfig {
            list_latency: Some(std::time::Duration::from_millis(2)),
            ..Default::default()
        });
        for d in 0..8 {
            builder = builder.file(&format!("/top/d{d}/f"), b"");
        }
        let vfs = builder.build();
        let lines = run(&vfs, &["/top"], WalkOptions::default(), Log::default()).await;
        let expected: Vec<String> = std::iter::once("dir /top rel=top d=0".to_string())
            .chain((0..8).flat_map(|d| {
                [
                    format!("dir /top/d{d} rel=top/d{d} d=1"),
                    format!("file /top/d{d}/f rel=top/d{d}/f d=2"),
                    format!("leave /top/d{d}"),
                ]
            }))
            .chain(std::iter::once("leave /top".to_string()))
            .collect();
        assert_eq!(lines, expected);
        let overlap = vfs.max_concurrent_listings();
        assert!(
            (2..=PREFETCH + 1).contains(&overlap),
            "listings should overlap up to the prefetch bound, saw {overlap}"
        );
    }

    #[tokio::test]
    async fn roots_are_classified_through_the_parent_listing_without_stat() {
        let vfs = MockVfs::builder()
            .config(MockVfsConfig {
                can_stat_directories: false,
                ..Default::default()
            })
            .dir("/bucket/prefix")
            .file("/bucket/prefix/k", b"k")
            .build();
        let lines = run(
            &vfs,
            &["/bucket/prefix"],
            WalkOptions::default(),
            Log::default(),
        )
        .await;
        assert_eq!(lines[0], "dir /bucket/prefix rel=prefix d=0");
        let missing = walk(
            &*vfs,
            &roots(&["/bucket/nope"]),
            &WalkOptions::default(),
            &mut Log::default(),
        )
        .await;
        assert!(missing.is_err());
    }
}
