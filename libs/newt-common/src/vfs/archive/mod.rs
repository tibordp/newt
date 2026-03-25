use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::filesystem::{File, Mode, UserGroup};
use crate::{Error, ErrorKind};

use super::{Breadcrumb, DisplayPathMatch};

mod tar;
mod zip;

pub use self::tar::TarArchiveVfs;
pub use self::zip::ZipArchiveVfs;

fn not_found(msg: impl Into<String>) -> Error {
    Error {
        kind: ErrorKind::NotFound,
        message: msg.into(),
    }
}

// ---------------------------------------------------------------------------
// Archive format detection
// ---------------------------------------------------------------------------

const TAR_EXTENSIONS: &[&str] = &[
    "tar", "tar.gz", "tgz", "tar.bz2", "tbz2", "tbz", "tar.xz", "txz", "tar.zst", "tzst",
    "tar.zstd", "cpio", "cpio.gz", "cpio.bz2", "cpio.xz", "cpio.zst",
];

const ZIP_EXTENSIONS: &[&str] = &["zip", "jar", "war", "ear", "apk", "ipa"];

pub fn is_archive_name(name: &str) -> bool {
    is_tar_name(name) || is_zip_name(name)
}

fn is_tar_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    TAR_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
}

pub fn is_zip_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ZIP_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
}

/// Detect compression format from filename extension.
fn detect_compression_from_name(name: &str) -> iluvatar::CompressionFormat {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".gz") || lower.ends_with(".tgz") {
        iluvatar::CompressionFormat::Gzip
    } else if lower.ends_with(".bz2") || lower.ends_with(".tbz2") || lower.ends_with(".tbz") {
        iluvatar::CompressionFormat::Bzip2
    } else if lower.ends_with(".xz") || lower.ends_with(".txz") {
        iluvatar::CompressionFormat::Xz
    } else if lower.ends_with(".zst") || lower.ends_with(".zstd") || lower.ends_with(".tzst") {
        iluvatar::CompressionFormat::Zstd
    } else {
        iluvatar::CompressionFormat::None
    }
}

// ---------------------------------------------------------------------------
// Shared descriptor helpers
// ---------------------------------------------------------------------------

fn archive_format_path(path: &Path, mount_meta: &[u8]) -> String {
    let origin_display = String::from_utf8_lossy(mount_meta);
    let inner = path.to_string_lossy();
    let inner = inner.trim_start_matches('/');
    if inner.is_empty() {
        origin_display.into_owned()
    } else {
        format!("{}/{}", origin_display, inner)
    }
}

fn archive_breadcrumbs(path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
    let origin_display = String::from_utf8_lossy(mount_meta);

    // Parse the origin display path into breadcrumbs (e.g. /home/user/file.tar.gz)
    let origin_segments: Vec<&str> = origin_display
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let mut crumbs = vec![Breadcrumb {
        label: "/".to_string(),
        nav_path: "/".to_string(),
    }];
    // Origin path segments navigate via ".." to escape into the parent VFS
    // The last origin segment (the archive filename) navigates to archive root "/"
    for (i, seg) in origin_segments.iter().enumerate() {
        let depth_from_root = origin_segments.len() - 1 - i;
        let nav_path = if depth_from_root == 0 {
            "/".to_string()
        } else {
            let mut p = String::from("/");
            for _ in 0..depth_from_root {
                p.push_str("../");
            }
            // Remove trailing slash
            p.pop();
            p
        };
        let is_last_overall = i == origin_segments.len() - 1 && path == Path::new("/");
        crumbs.push(Breadcrumb {
            label: if is_last_overall {
                seg.to_string()
            } else {
                format!("{}/", seg)
            },
            nav_path,
        });
    }

    // Inner archive path segments
    let s = path.to_string_lossy();
    let segments: Vec<&str> = s.split('/').filter(|s| !s.is_empty()).collect();
    let mut accumulated = String::new();
    for (i, seg) in segments.iter().enumerate() {
        accumulated.push('/');
        accumulated.push_str(seg);
        crumbs.push(Breadcrumb {
            label: if i == segments.len() - 1 {
                seg.to_string()
            } else {
                format!("{}/", seg)
            },
            nav_path: accumulated.clone(),
        });
    }

    crumbs
}

fn archive_try_parse_display_path(input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
    let origin_display = String::from_utf8_lossy(mount_meta);
    if input == origin_display.as_ref() {
        return Some(DisplayPathMatch::exact(PathBuf::from("/")));
    }
    let rest = input.strip_prefix(origin_display.as_ref())?;
    let rest = rest.strip_prefix('/')?;
    let path = if rest.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(format!("/{}", rest))
    };
    Some(DisplayPathMatch::exact(path))
}

fn archive_mount_label(mount_meta: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(mount_meta);
    if s.is_empty() {
        None
    } else {
        Some(s.into_owned())
    }
}

// ---------------------------------------------------------------------------
// Directory tree built from archive index
// ---------------------------------------------------------------------------

/// Maximum number of symlink hops before we declare a loop (matches Linux MAXSYMLINKS).
const MAX_SYMLINK_HOPS: usize = 40;

struct DirectoryTree {
    dirs: HashMap<PathBuf, Vec<File>>,
}

impl DirectoryTree {
    fn list(&self, path: &Path) -> Result<Vec<File>, Error> {
        let resolved = self.resolve_path(path, true)?;
        let entries = match self.dirs.get(&resolved) {
            Some(entries) => entries,
            None => {
                // Check if it exists as a file rather than a directory
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
            name: "..".to_string(),
            size: None,
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
        }];
        for entry in entries {
            let mut file = entry.clone();
            self.fill_symlink_target_metadata(&resolved, &mut file);
            files.push(file);
        }
        Ok(files)
    }

    fn file_info(&self, path: &Path) -> Result<File, Error> {
        let normalized = normalize_dir_path(path);
        let resolved = self.resolve_path(&normalized, false)?;
        let mut file = self
            .lookup_entry(&resolved)
            .ok_or_else(|| not_found(format!("file not found: {}", path.display())))?;
        let parent = resolved.parent().unwrap_or(Path::new(""));
        self.fill_symlink_target_metadata(parent, &mut file);
        Ok(file)
    }

    /// For symlink entries, follow the target and fill in `is_dir` and `size`
    /// from the resolved target — mirroring the lstat+stat pattern used by the
    /// local filesystem VFS. The entry keeps `is_symlink=true` and
    /// `symlink_target` intact.  If resolution fails (broken link), the
    /// original metadata is left unchanged.
    fn fill_symlink_target_metadata(&self, parent: &Path, file: &mut File) {
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
    fn lookup_entry(&self, normalized: &Path) -> Option<File> {
        let parent = normalized.parent()?;
        let name = normalized.file_name()?.to_string_lossy();
        let children = self.dirs.get(parent)?;
        children.iter().find(|f| f.name == *name).cloned()
    }

    /// Resolve symlinks in a path within the archive.
    ///
    /// If `follow_last` is true, the final component is also followed if it's
    /// a symlink. Returns the resolved normalized path (no leading slash).
    fn resolve_path(&self, path: &Path, follow_last: bool) -> Result<PathBuf, Error> {
        let normalized = normalize_dir_path(path);
        let s = normalized.to_string_lossy();
        if s.is_empty() {
            return Ok(PathBuf::from(""));
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
    ) -> Result<PathBuf, Error> {
        if hops > MAX_SYMLINK_HOPS {
            return Err(Error {
                kind: ErrorKind::Other,
                message: "too many levels of symbolic links".into(),
            });
        }

        let mut resolved = PathBuf::new();

        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;

            let file = self
                .dirs
                .get(&resolved)
                .and_then(|children| children.iter().find(|f| f.name == *component));

            match file {
                Some(f) if f.is_symlink && (!is_last || follow_last) => {
                    if let Some(ref target) = f.symlink_target {
                        let target_resolved = if target.is_absolute() {
                            normalize_path_dotdot(&normalize_dir_path(target))
                        } else {
                            let mut base = resolved.clone();
                            base.push(target.as_path());
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
                    resolved.push(component);
                }
                Some(_) => {
                    resolved.push(component);
                }
                None => {
                    // Component not found in tree
                    resolved.push(component);
                    return Ok(resolved);
                }
            }
        }

        Ok(resolved)
    }
}

fn normalize_dir_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let s = s.trim_start_matches('/');
    let s = s.trim_start_matches("./");
    let s = s.trim_end_matches('/');
    PathBuf::from(s)
}

/// Normalize a path by resolving `.` and `..` components.
/// Absolute paths are treated as relative to the archive root.
fn normalize_path_dotdot(path: &Path) -> PathBuf {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(s) => parts.push(s),
            std::path::Component::ParentDir => {
                parts.pop();
            }
            // CurDir, RootDir, Prefix — skip
            _ => {}
        }
    }
    if parts.is_empty() {
        PathBuf::from("")
    } else {
        parts.iter().collect()
    }
}

/// Convert a normalized PathBuf to a string suitable for index lookups.
fn normalized_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Look up an entry in the iluvatar index by normalized path, falling back
/// to a `./`-prefixed variant (many tar archives store paths like `./foo`).
fn index_get<'a>(
    index: &'a iluvatar::ArchiveIndex,
    normalized: &str,
) -> Option<&'a iluvatar::IndexEntry> {
    index
        .get(normalized)
        .or_else(|| index.get(&format!("./{}", normalized)))
}

/// Return the path string used for a given entry in the iluvatar index.
/// Handles the `./`-prefix convention used by many tar generators.
fn index_path_str(index: &iluvatar::ArchiveIndex, normalized: &str) -> Option<String> {
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

fn mtime_to_i64(mtime: u64) -> Option<i64> {
    i64::try_from(mtime).ok().map(|t| t.saturating_mul(1_000))
}

fn ensure_ancestors(
    dirs: &mut HashMap<PathBuf, Vec<File>>,
    seen_dirs: &mut std::collections::HashSet<PathBuf>,
    path: &Path,
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
                name,
                size: None,
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
            });
        }
    }
    seen_dirs.insert(path.to_path_buf());
    dirs.entry(path.to_path_buf()).or_default();
}

// ---------------------------------------------------------------------------
// RangeReadAdapter — wraps async read_range into sync Read + Seek
// ---------------------------------------------------------------------------

use std::io::{Seek, SeekFrom};
use std::sync::Arc;

use super::Vfs;

/// Adapter that implements `Read + Seek` by calling `upstream.read_range()`
/// via `Handle::block_on()`. Designed to be used inside `spawn_blocking`.
struct RangeReadAdapter {
    handle: tokio::runtime::Handle,
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    file_size: u64,
    position: u64,
}

impl Read for RangeReadAdapter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.file_size {
            return Ok(0);
        }
        let len = buf.len() as u64;
        let chunk = self
            .handle
            .block_on(
                self.upstream
                    .read_range(&self.archive_path, self.position, len),
            )
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let n = chunk.data.len();
        buf[..n].copy_from_slice(&chunk.data);
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for RangeReadAdapter {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.file_size as i64 + n,
            SeekFrom::Current(n) => self.position as i64 + n,
        };
        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek to negative position",
            ));
        }
        self.position = new_pos as u64;
        Ok(self.position)
    }
}

/// Minimum time between partial tree snapshots during indexing.
const SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

fn build_directory_tree_from_iluvatar(entries: Vec<&iluvatar::IndexEntry>) -> DirectoryTree {
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

    let mut dirs: HashMap<PathBuf, Vec<File>> = HashMap::new();
    let mut seen_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    dirs.insert(PathBuf::from(""), Vec::new());
    seen_dirs.insert(PathBuf::from(""));

    for entry in &entries {
        let path = entry
            .path
            .trim_start_matches('/')
            .trim_start_matches("./")
            .trim_end_matches('/');
        if path.is_empty() {
            continue;
        }

        let entry_path = PathBuf::from(path);
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
            name: name.clone(),
            size,
            is_dir,
            is_hidden: name.starts_with('.'),
            is_symlink,
            symlink_target: if is_symlink {
                entry.link_target.as_ref().map(PathBuf::from)
            } else {
                None
            },
            user: Some(UserGroup::Id(entry.uid as u32)),
            group: Some(UserGroup::Id(entry.gid as u32)),
            mode: Some(Mode(entry.mode)),
            modified: mtime_to_i64(entry.mtime),
            accessed: None,
            created: None,
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
