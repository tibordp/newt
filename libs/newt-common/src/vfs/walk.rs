//! Depth-first walk of one VFS's subtrees, reported to a [`Visitor`].
//!
//! Every tree traversal in the crate — copy planning, delete, recursive
//! attributes, du, search — drives this one loop, so the rules live in
//! one place: `..` rows dropped, a directory symlink entered only in
//! follow mode and never twice, a mount point recognized by its device,
//! an excluded subtree never listed, an unreadable directory the
//! visitor's call. The walk sees a single `Vfs`, so anything mounted
//! over it in the registry is invisible by construction.

use std::sync::Arc;

use async_trait::async_trait;

use super::path::{Path, PathBuf};
use super::{File, MAX_SYMLINK_HOPS, Vfs};
use crate::Error;

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
            if walker.visit(child, depth).await? == Flow::Stop {
                return Ok(());
            }
        }
    }
    Ok(())
}

struct Walker<'a> {
    vfs: &'a dyn Vfs,
    options: &'a WalkOptions,
    visitor: &'a mut dyn Visitor,
    stack: Vec<Frame>,
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

        let children = loop {
            match self.vfs.list_files(&child.path, None).await {
                Ok(list) => break list.files,
                Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
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
