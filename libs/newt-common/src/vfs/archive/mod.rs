use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::filesystem::{File, Mode, UserGroup};
use crate::{Error, ErrorKind};

use super::Breadcrumb;

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

fn archive_try_parse_display_path(input: &str, mount_meta: &[u8]) -> Option<PathBuf> {
    let origin_display = String::from_utf8_lossy(mount_meta);
    if input == origin_display.as_ref() {
        return Some(PathBuf::from("/"));
    }
    let rest = input.strip_prefix(origin_display.as_ref())?;
    let rest = rest.strip_prefix('/')?;
    if rest.is_empty() {
        Some(PathBuf::from("/"))
    } else {
        Some(PathBuf::from(format!("/{}", rest)))
    }
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

struct DirectoryTree {
    dirs: HashMap<PathBuf, Vec<File>>,
}

impl DirectoryTree {
    fn list(&self, path: &Path) -> Result<Vec<File>, Error> {
        let normalized = normalize_dir_path(path);
        let entries = match self.dirs.get(&normalized) {
            Some(entries) => entries,
            None => {
                // Check if it exists as a file rather than a directory
                if self.file_info(path).is_ok() {
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
        files.extend(entries.iter().cloned());
        Ok(files)
    }

    fn file_info(&self, path: &Path) -> Result<File, Error> {
        let parent = path.parent().ok_or_else(|| not_found("no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| not_found("no filename"))?
            .to_string_lossy();
        let normalized_parent = normalize_dir_path(parent);
        let children = self
            .dirs
            .get(&normalized_parent)
            .ok_or_else(|| not_found(format!("parent not found: {}", parent.display())))?;
        children
            .iter()
            .find(|f| f.name == *name)
            .cloned()
            .ok_or_else(|| not_found(format!("file not found: {}", path.display())))
    }
}

fn normalize_dir_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let s = s.trim_start_matches('/');
    let s = s.trim_start_matches("./");
    let s = s.trim_end_matches('/');
    PathBuf::from(s)
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

fn mtime_to_i128(mtime: u64) -> Option<i128> {
    Some((mtime as i128) * 1_000)
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

        let file = File {
            name: name.clone(),
            size: if is_dir { None } else { Some(entry.size) },
            is_dir,
            is_hidden: name.starts_with('.'),
            is_symlink,
            symlink_target: entry.link_target.as_ref().map(PathBuf::from),
            user: Some(UserGroup::Id(entry.uid as u32)),
            group: Some(UserGroup::Id(entry.gid as u32)),
            mode: Some(Mode(entry.mode)),
            modified: mtime_to_i128(entry.mtime),
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
