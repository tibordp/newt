//! Directory tree built from an archive index: the shared listing/stat
//! structure for tar and zip mounts, with in-archive symlink resolution.
//!
//! Keyed by Unix-style *relative* path strings; internally builds keys via
//! std `PathBuf` with explicit `/`-join workarounds (see `resolve_components`).

use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf as StdPathBuf};

use crate::vfs::{File, Mode, Symlink, UserGroup};
use newt_common::{Error, ErrorKind};

pub(super) struct DirectoryTree {
    pub(super) dirs: HashMap<StdPathBuf, Vec<File>>,
}

impl DirectoryTree {
    pub(super) fn list(&self, path: &StdPath) -> Result<Vec<File>, Error> {
        let resolved = self.resolve_path(path, true)?;
        let entries = match self.dirs.get(&resolved) {
            Some(entries) => entries,
            None => {
                if self.lookup_entry(&resolved).is_some() {
                    return Err(Error {
                        kind: ErrorKind::NotADirectory,
                        message: format!("not a directory: {}", path.display()),
                    });
                }
                return Err(Error::not_found(format!(
                    "directory not found: {}",
                    path.display()
                )));
            }
        };

        let mut files = vec![File::parent_dir()];
        for entry in entries {
            let mut file = entry.clone();
            self.fill_symlink_target_metadata(&resolved, &mut file);
            files.push(file);
        }
        Ok(files)
    }

    /// See `Vfs::file_info`.
    pub(super) fn file_info(&self, path: &StdPath, follow: bool) -> Result<File, Error> {
        let normalized = normalize_dir_path(path);
        let resolved = self.resolve_path(&normalized, follow)?;
        if resolved.as_os_str().is_empty() {
            return Ok(File::bare_dir(""));
        }
        let mut file = self
            .lookup_entry(&resolved)
            .ok_or_else(|| Error::not_found(format!("file not found: {}", path.display())))?;
        if follow {
            // Resolution stops at a link whose target is unknown.
            if file.is_symlink() {
                return Err(Error::not_found(format!(
                    "link leads nowhere: {}",
                    path.display()
                )));
            }
            if let Some(name) = normalized.file_name() {
                file.name = name.to_string_lossy().into_owned();
            }
        } else {
            let parent = resolved.parent().unwrap_or(StdPath::new(""));
            self.fill_symlink_target_metadata(parent, &mut file);
        }
        Ok(file)
    }

    /// For symlink entries, follow the target and fill in `is_dir` and `size`
    /// from the resolved target — mirroring the lstat+stat pattern used by the
    /// local filesystem VFS. The entry keeps its `symlink` intact. If
    /// resolution fails (broken link), the original metadata is left
    /// unchanged.
    fn fill_symlink_target_metadata(&self, parent: &StdPath, file: &mut File) {
        if !file.is_symlink() {
            return;
        }
        let mut full_path = parent.to_path_buf();
        full_path.push(&file.name);
        if let Ok(resolved_target) = self.resolve_path(&full_path, true) {
            if self.dirs.contains_key(&resolved_target) {
                file.is_dir = true;
                file.size = None;
            } else if let Some(target_file) = self.lookup_entry(&resolved_target) {
                file.is_dir = target_file.is_dir;
                file.size = target_file.size;
            }
        }
    }

    /// Look up an entry by its exact normalized path (no symlink resolution).
    fn lookup_entry(&self, normalized: &StdPath) -> Option<File> {
        let parent = normalized.parent()?;
        let name = normalized.file_name()?.to_string_lossy();
        let children = self.dirs.get(parent)?;
        children.iter().find(|f| f.name == *name).cloned()
    }

    /// Resolve symlinks in a path within the archive.
    ///
    /// If `follow_last` is true, the final component is also followed if it's
    /// a symlink. Returns the resolved normalized path (no leading slash).
    pub(super) fn resolve_path(
        &self,
        path: &StdPath,
        follow_last: bool,
    ) -> Result<StdPathBuf, Error> {
        let normalized = normalize_dir_path(path);
        let s = normalized.to_string_lossy();
        if s.is_empty() {
            return Ok(StdPathBuf::from(""));
        }
        let components: Vec<String> = s
            .split('/')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        self.resolve_components(&components, follow_last, 0)
    }

    fn resolve_components(
        &self,
        components: &[String],
        follow_last: bool,
        hops: usize,
    ) -> Result<StdPathBuf, Error> {
        if hops > crate::vfs::MAX_SYMLINK_HOPS {
            return Err(Error {
                kind: ErrorKind::Other,
                message: "too many levels of symbolic links".into(),
            });
        }

        // Build paths as `/`-joined strings rather than via `PathBuf::push`,
        // which would insert `\` on Windows and break index lookups (archive
        // entry keys are always stored Unix-style).
        let mut resolved_parts: Vec<String> = Vec::new();

        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;

            let resolved_path: StdPathBuf = StdPathBuf::from(resolved_parts.join("/"));
            let file = self
                .dirs
                .get(&resolved_path)
                .and_then(|children| children.iter().find(|f| f.name == *component));

            match file {
                Some(f) if f.is_symlink() && (!is_last || follow_last) => {
                    if let Some(target) = f.symlink_target() {
                        // Raw link-target string from the archive; interpret
                        // it as a path locally for resolution.
                        let target = StdPath::new(target);
                        let target_resolved = if target.is_absolute() {
                            normalize_path_dotdot(&normalize_dir_path(target))
                        } else {
                            let mut base = StdPathBuf::from(resolved_parts.join("/"));
                            base.push(target);
                            normalize_path_dotdot(&base)
                        };
                        // Resolve target + remaining components together
                        let target_str = target_resolved.to_string_lossy();
                        let mut remaining: Vec<String> = target_str
                            .split('/')
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect();
                        remaining.extend_from_slice(&components[i + 1..]);
                        return self.resolve_components(&remaining, follow_last, hops + 1);
                    }
                    // Symlink with no target — treat as-is
                    resolved_parts.push(component.clone());
                }
                Some(_) => {
                    resolved_parts.push(component.clone());
                }
                None => {
                    // Component not found in tree
                    resolved_parts.push(component.clone());
                    return Ok(StdPathBuf::from(resolved_parts.join("/")));
                }
            }
        }

        Ok(StdPathBuf::from(resolved_parts.join("/")))
    }
}

pub(super) fn normalize_dir_path(path: &StdPath) -> StdPathBuf {
    let s = path.to_string_lossy();
    let s = s.trim_start_matches('/');
    let s = s.trim_start_matches("./");
    let s = s.trim_end_matches('/');
    StdPathBuf::from(s)
}

/// Normalize a path by resolving `.` and `..` components.
/// Absolute paths are treated as relative to the archive root.
///
/// Builds the result with `/` separators regardless of host OS so that
/// archive entries can be looked up by their stored (Unix-style) key on
/// a Windows host without separator mangling.
pub(super) fn normalize_path_dotdot(path: &StdPath) -> StdPathBuf {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => {
                parts.pop();
            }
            // CurDir, RootDir, Prefix — skip
            _ => {}
        }
    }
    StdPathBuf::from(parts.join("/"))
}

/// Convert a normalized PathBuf to a string suitable for index lookups.
pub(super) fn normalized_to_string(path: &StdPath) -> String {
    path.to_string_lossy().into_owned()
}

/// Look up an entry in the iluvatar index by normalized path, falling back
/// to a `./`-prefixed variant (many tar archives store paths like `./foo`).
pub(super) fn index_get<'a>(
    index: &'a iluvatar::ArchiveIndex,
    normalized: &str,
) -> Option<&'a iluvatar::IndexEntry> {
    index
        .get(normalized)
        .or_else(|| index.get(&format!("./{}", normalized)))
}

/// Return the path string used for a given entry in the iluvatar index.
/// Handles the `./`-prefix convention used by many tar generators.
pub(super) fn index_path_str(index: &iluvatar::ArchiveIndex, normalized: &str) -> Option<String> {
    if index.get(normalized).is_some() {
        Some(normalized.to_string())
    } else {
        let dotslash = format!("./{}", normalized);
        if index.get(&dotslash).is_some() {
            Some(dotslash)
        } else {
            None
        }
    }
}

pub(super) fn mtime_to_i64(mtime: u64) -> Option<i64> {
    i64::try_from(mtime).ok().map(|t| t.saturating_mul(1_000))
}

/// A member's path as the tree keys it: `/`-separated, without leading
/// `/` or `./`, trailing `/`, or empty and `.` segments. `None` for a path
/// naming nothing, or leaving the archive (`../../.bashrc`), so nothing
/// can copy it out above its destination.
pub(super) fn member_path(path: &str) -> Option<String> {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if segments.contains(&"..") {
        log::warn!("archive: leaving out {path:?}, which points outside the archive");
        return None;
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

/// The tree of an archive's members, given in archive order, each with
/// its path inside the archive. The tree names each entry from its path
/// (and hides a dot-name), so callers leave `name` empty.
///
/// A name can be mentioned more than once: repeated, or once as a member
/// and once as the folder later members imply (`d` a symlink, then
/// `d/x`). Its last mention decides what it is — a mention by a child
/// makes it a folder — so no listing holds two entries of one name, and
/// each member stays its own entry. A member under a name that ends up as
/// anything but a folder can't be reached and is left out, so copying out
/// never writes through a link an archive planted where a folder is.
pub(super) fn build_tree<'a>(members: impl IntoIterator<Item = (&'a str, File)>) -> DirectoryTree {
    enum Mention {
        Member(usize),
        Implied,
    }
    let mut kept: Vec<File> = Vec::new();
    let mut last: HashMap<String, Mention> = HashMap::new();
    // A folder's last own entry, for its metadata when a child's mention
    // came after it.
    let mut folder_entry: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut mention = |last: &mut HashMap<String, Mention>, path: &str, m: Mention| {
        if last.insert(path.to_string(), m).is_none() {
            order.push(path.to_string());
        }
    };

    for (raw, mut file) in members {
        let Some(path) = member_path(raw) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(&path);
        file.is_hidden |= name.starts_with('.');
        file.name = name.to_string();
        for (slash, _) in path.match_indices('/') {
            mention(&mut last, &path[..slash], Mention::Implied);
        }
        let index = kept.len();
        if file.is_dir {
            folder_entry.insert(path.clone(), index);
        }
        mention(&mut last, &path, Mention::Member(index));
        kept.push(file);
    }

    let is_folder = |path: &str| match last[path] {
        Mention::Implied => true,
        Mention::Member(i) => kept[i].is_dir,
    };
    let mut dirs: HashMap<StdPathBuf, Vec<File>> = HashMap::new();
    dirs.insert(StdPathBuf::from(""), Vec::new());
    for path in &order {
        if !path
            .match_indices('/')
            .all(|(slash, _)| is_folder(&path[..slash]))
        {
            continue;
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let file = if is_folder(path) {
            dirs.entry(StdPathBuf::from(path.as_str())).or_default();
            match folder_entry.get(path) {
                Some(&i) => kept[i].clone(),
                None => File::bare_dir(name),
            }
        } else {
            match last[path] {
                Mention::Member(i) => kept[i].clone(),
                Mention::Implied => unreachable!("an implied mention is a folder"),
            }
        };
        dirs.entry(StdPathBuf::from(parent)).or_default().push(file);
    }
    DirectoryTree { dirs }
}

/// Minimum time between partial tree snapshots during indexing.
pub(super) const SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// `entries` in archive order, as [`build_tree`] lists them: of several
/// members sharing a path, the last — the one `ArchiveIndex::get`, and so
/// every read, resolves to.
pub(super) fn build_directory_tree_from_iluvatar(
    entries: &[iluvatar::IndexEntry],
) -> DirectoryTree {
    fn trimmed(path: &str) -> &str {
        path.trim_start_matches('/')
            .trim_start_matches("./")
            .trim_end_matches('/')
    }
    // Hard link entries typically have size=0 — they take the target's,
    // the target's last member like everything else.
    let by_path: HashMap<&str, &iluvatar::IndexEntry> =
        entries.iter().map(|e| (trimmed(&e.path), e)).collect();

    build_tree(entries.iter().map(|entry| {
        let is_dir = entry.entry_type.is_directory();
        let is_symlink = matches!(entry.entry_type, iluvatar::EntryType::SymLink);
        let size = match (&entry.entry_type, &entry.link_target) {
            _ if is_dir => None,
            (iluvatar::EntryType::HardLink, Some(target)) => by_path
                .get(trimmed(target))
                .map(|t| t.size)
                .or(Some(entry.size)),
            _ => Some(entry.size),
        };
        let file = File {
            attributes: None,
            name: String::new(),
            size,
            allocated_size: None,
            device_id: None,
            inode: None,
            hard_links: None,
            is_dir,
            is_hidden: false,
            symlink: is_symlink.then(|| Symlink::from_read(entry.link_target.clone())),
            user: Some(UserGroup::Id(entry.uid as u32)),
            group: Some(UserGroup::Id(entry.gid as u32)),
            mode: Some(Mode(entry.mode)),
            modified: mtime_to_i64(entry.mtime),
            accessed: None,
            created: None,
            key: None,
            source: None,
        };
        (entry.path.as_str(), file)
    }))
}

#[cfg(test)]
mod build_tree_tests {
    use super::{DirectoryTree, build_tree};
    use crate::vfs::{File, Mode, Symlink};
    use std::path::Path as StdPath;

    fn file() -> File {
        File {
            is_dir: false,
            ..File::bare_dir("")
        }
    }

    fn dir(mode: u32) -> File {
        File {
            mode: Some(Mode(mode)),
            ..File::bare_dir("")
        }
    }

    fn names(tree: &DirectoryTree, path: &str) -> Vec<(String, bool)> {
        let mut names: Vec<(String, bool)> = tree.dirs[StdPath::new(path)]
            .iter()
            .map(|f| (f.name.clone(), f.is_dir))
            .collect();
        names.sort();
        names
    }

    fn link(target: Symlink) -> File {
        File {
            symlink: Some(target),
            ..file()
        }
    }

    #[test]
    fn file_info_follows_a_link_only_when_asked() {
        let target = File {
            size: Some(5),
            mode: Some(Mode(0o640)),
            ..file()
        };
        let tree = build_tree([
            ("d/data", target),
            ("to_data", link(Symlink::Target("d/data".into()))),
            ("to_d", link(Symlink::Target("d".into()))),
        ]);

        let entry = tree.file_info(StdPath::new("to_data"), false).unwrap();
        assert_eq!(entry.symlink_target(), Some("d/data"));
        assert_eq!(entry.mode, None);

        let followed = tree.file_info(StdPath::new("to_data"), true).unwrap();
        assert!(!followed.is_symlink());
        assert_eq!(followed.name, "to_data");
        assert_eq!(followed.size, Some(5));
        assert_eq!(followed.mode.map(|m| m.0), Some(0o640));

        assert!(tree.file_info(StdPath::new("to_d"), false).unwrap().is_dir);
        assert!(tree.file_info(StdPath::new("to_d"), true).unwrap().is_dir);
    }

    #[test]
    fn following_a_link_that_leads_nowhere_fails() {
        let tree = build_tree([
            ("unknown", link(Symlink::Unknown)),
            ("dangling", link(Symlink::Target("missing".into()))),
        ]);
        for name in ["unknown", "dangling"] {
            assert!(tree.file_info(StdPath::new(name), false).is_ok(), "{name}");
            let err = tree.file_info(StdPath::new(name), true).unwrap_err();
            assert_eq!(err.kind, newt_common::ErrorKind::NotFound, "{name}");
        }
    }

    #[test]
    fn a_file_and_a_folder_of_one_name_take_the_last_mention() {
        let tree = build_tree([("d", file()), ("d/x", file())]);
        assert_eq!(names(&tree, ""), [("d".to_string(), true)]);
        assert_eq!(names(&tree, "d"), [("x".to_string(), false)]);

        let tree = build_tree([("d/x", file()), ("d", file())]);
        assert_eq!(names(&tree, ""), [("d".to_string(), false)]);
        assert!(!tree.dirs.contains_key(StdPath::new("d")));
    }

    #[test]
    fn a_folder_keeps_its_own_entry_when_a_child_mentions_it_later() {
        let tree = build_tree([
            ("./d/", dir(0o700)),
            ("d/x", file()),
            ("/e/.hidden", file()),
        ]);
        let d = &tree.dirs[StdPath::new("")]
            .iter()
            .find(|f| f.name == "d")
            .cloned()
            .unwrap();
        assert_eq!(d.mode.as_ref().map(|m| m.0), Some(0o700));
        assert!(tree.dirs[StdPath::new("e")][0].is_hidden);
    }

    #[test]
    fn members_leaving_the_archive_are_left_out() {
        let tree = build_tree([
            ("../evil", file()),
            ("a/../../evil", file()),
            ("ok", file()),
        ]);
        assert_eq!(names(&tree, ""), [("ok".to_string(), false)]);
    }
}
