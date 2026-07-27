//! Directory tree built from an archive index: the shared listing/stat
//! structure for tar and zip mounts, with in-archive symlink resolution.
//!
//! Keyed by Unix-style *relative* path strings; internally builds keys via
//! std `PathBuf` with explicit `/`-join workarounds (see `resolve_components`).

use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf as StdPathBuf};

use crate::vfs::{File, Mode, UserGroup};
use crate::{Error, ErrorKind};

use super::not_found;

/// Maximum number of symlink hops before we declare a loop (matches Linux MAXSYMLINKS).
pub(super) const MAX_SYMLINK_HOPS: usize = 40;

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
                return Err(not_found(format!(
                    "directory not found: {}",
                    path.display()
                )));
            }
        };

        let mut files = vec![File {
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
        }];
        for entry in entries {
            let mut file = entry.clone();
            self.fill_symlink_target_metadata(&resolved, &mut file);
            files.push(file);
        }
        Ok(files)
    }

    pub(super) fn file_info(&self, path: &StdPath) -> Result<File, Error> {
        let normalized = normalize_dir_path(path);
        let resolved = self.resolve_path(&normalized, false)?;
        let mut file = self
            .lookup_entry(&resolved)
            .ok_or_else(|| not_found(format!("file not found: {}", path.display())))?;
        let parent = resolved.parent().unwrap_or(StdPath::new(""));
        self.fill_symlink_target_metadata(parent, &mut file);
        Ok(file)
    }

    /// For symlink entries, follow the target and fill in `is_dir` and `size`
    /// from the resolved target — mirroring the lstat+stat pattern used by the
    /// local filesystem VFS. The entry keeps `is_symlink=true` and
    /// `symlink_target` intact.  If resolution fails (broken link), the
    /// original metadata is left unchanged.
    fn fill_symlink_target_metadata(&self, parent: &StdPath, file: &mut File) {
        if !file.is_symlink {
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
        if hops > MAX_SYMLINK_HOPS {
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
                Some(f) if f.is_symlink && (!is_last || follow_last) => {
                    if let Some(ref target) = f.symlink_target {
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

pub(super) fn ensure_ancestors(
    dirs: &mut HashMap<StdPathBuf, Vec<File>>,
    seen_dirs: &mut std::collections::HashSet<StdPathBuf>,
    path: &StdPath,
) {
    if seen_dirs.contains(path) {
        return;
    }
    if let Some(parent) = path.parent() {
        ensure_ancestors(dirs, seen_dirs, parent);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.is_empty() {
            dirs.entry(parent.to_path_buf()).or_default().push(File {
                attributes: None,
                name,
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
            });
        }
    }
    seen_dirs.insert(path.to_path_buf());
    dirs.entry(path.to_path_buf()).or_default();
}

/// Minimum time between partial tree snapshots during indexing.
pub(super) const SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

pub(super) fn build_directory_tree_from_iluvatar(
    entries: Vec<&iluvatar::IndexEntry>,
) -> DirectoryTree {
    // Build a quick lookup for hard link target sizes
    let entry_by_path: HashMap<&str, &iluvatar::IndexEntry> = entries
        .iter()
        .map(|e| {
            let p = e
                .path
                .trim_start_matches('/')
                .trim_start_matches("./")
                .trim_end_matches('/');
            (p, *e)
        })
        .collect();

    let mut dirs: HashMap<StdPathBuf, Vec<File>> = HashMap::new();
    let mut seen_dirs: std::collections::HashSet<StdPathBuf> = std::collections::HashSet::new();

    dirs.insert(StdPathBuf::from(""), Vec::new());
    seen_dirs.insert(StdPathBuf::from(""));

    for entry in &entries {
        let path = entry
            .path
            .trim_start_matches('/')
            .trim_start_matches("./")
            .trim_end_matches('/');
        if path.is_empty() {
            continue;
        }

        let entry_path = StdPathBuf::from(path);
        let parent = entry_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let name = entry_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if name.is_empty() {
            continue;
        }

        ensure_ancestors(&mut dirs, &mut seen_dirs, &parent);

        let is_dir = entry.entry_type.is_directory();
        let is_symlink = matches!(entry.entry_type, iluvatar::EntryType::SymLink);
        let is_hardlink = matches!(entry.entry_type, iluvatar::EntryType::HardLink);

        // Hard link entries typically have size=0 — use the target's size instead.
        let size = if is_dir {
            None
        } else if is_hardlink {
            if let Some(ref target) = entry.link_target {
                let target_norm = target
                    .trim_start_matches('/')
                    .trim_start_matches("./")
                    .trim_end_matches('/');
                entry_by_path
                    .get(target_norm)
                    .map(|t| t.size)
                    .or(Some(entry.size))
            } else {
                Some(entry.size)
            }
        } else {
            Some(entry.size)
        };

        let file = File {
            attributes: None,
            name: name.clone(),
            size,
            allocated_size: None,
            device_id: None,
            inode: None,
            hard_links: None,
            is_dir,
            is_hidden: name.starts_with('.'),
            is_symlink,
            symlink_target: if is_symlink {
                entry.link_target.clone()
            } else {
                None
            },
            user: Some(UserGroup::Id(entry.uid as u32)),
            group: Some(UserGroup::Id(entry.gid as u32)),
            mode: Some(Mode(entry.mode)),
            modified: mtime_to_i64(entry.mtime),
            accessed: None,
            created: None,
            key: None,
            source: None,
        };

        if is_dir && seen_dirs.contains(&entry_path) {
            // Already added as an implicit ancestor — replace synthetic entry
            // with real metadata.
            if let Some(children) = dirs.get_mut(&parent)
                && let Some(existing) = children.iter_mut().find(|f| f.name == name)
            {
                *existing = file;
            }
            continue;
        }

        dirs.entry(parent).or_default().push(file);

        if is_dir {
            seen_dirs.insert(entry_path.clone());
            dirs.entry(entry_path).or_default();
        }
    }

    DirectoryTree { dirs }
}
