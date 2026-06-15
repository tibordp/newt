use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

// The archive index machinery is keyed by Unix-style *relative* path
// strings. Internally it builds keys via std `PathBuf` with explicit
// `/`-join workarounds, so it keeps using the std path types. The `Vfs`
// trait surface speaks our platform-independent `vfs::path::Path`; each
// trait method converts at the boundary (`path.as_wire_str()`, leading
// `/` stripped by `normalize_dir_path`).
use std::path::{Path as StdPath, PathBuf as StdPathBuf};

use crate::filesystem::{File, Mode, UserGroup};
use crate::vfs::path::{Path, PathBuf};
use crate::{Error, ErrorKind};

use super::{Breadcrumb, DisplayPathMatch, Vfs, VfsPath};
use serde::{Deserialize, Serialize};

/// Archive `mount_meta`. The origin is rendered by the *upstream* VFS's
/// descriptor at mount time, so the archive inherits the upstream's path
/// style — a ZIP on a Windows drive shows `C:\…\x.zip`, not a mangled
/// `/C:\…`. Runtime-only (regenerated per mount); no versioning needed.
#[derive(Serialize, Deserialize)]
struct ArchiveMeta {
    /// Origin via the upstream descriptor's `format_path`.
    display: String,
    /// Origin breadcrumb *labels* from the upstream descriptor — already
    /// styled and segmented in the upstream's convention. Empty only for
    /// legacy/raw bytes, where breadcrumbs fall back to `/`-splitting.
    origin_crumbs: Vec<String>,
    /// The upstream's display separator (`\` on a Windows drive, `/`
    /// elsewhere). Used so the archive interior and the origin↔archive
    /// boundary render in the upstream's style too — *display only*;
    /// `nav_path`s stay canonical `/`.
    sep: char,
}

impl Default for ArchiveMeta {
    fn default() -> Self {
        Self {
            display: String::new(),
            origin_crumbs: Vec::new(),
            sep: '/',
        }
    }
}

fn decode_meta(mount_meta: &[u8]) -> ArchiveMeta {
    bincode::deserialize(mount_meta).unwrap_or_else(|_| ArchiveMeta {
        display: String::from_utf8_lossy(mount_meta).into_owned(),
        ..ArchiveMeta::default()
    })
}

/// The upstream's display separator, read off an intermediate origin
/// breadcrumb (an upstream non-final crumb label ends with it — `Users\`,
/// `u/`). Defaults to `/`.
fn upstream_sep(origin_crumbs: &[String]) -> char {
    origin_crumbs
        .get(origin_crumbs.len().wrapping_sub(2))
        .and_then(|c| c.chars().last())
        .filter(|c| *c == '/' || *c == '\\')
        .unwrap_or('/')
}

mod tar;
mod zip;

pub use self::tar::TarArchiveVfs;
pub use self::zip::ZipArchiveVfs;

/// Build an archive VFS from a `MountRequest::Archive`. Resolves the
/// upstream VFS holding the archive bytes via the registry and picks
/// `ZipArchiveVfs` or `TarArchiveVfs` based on the file extension. The
/// archive's display path (origin rendered through the upstream's
/// `format_path`) is stamped into `mount_meta` so the mounted VFS keeps
/// a stable label even after the origin is unmounted.
///
/// For ZIP archives the mount itself never prompts: the central
/// directory is always cleartext, so listing always works. The askpass
/// provider is plumbed into the mounted VFS so reading an encrypted
/// entry can prompt lazily and cache the password for subsequent reads.
pub async fn mount(
    origin: VfsPath,
    ctx: &crate::api::MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    log::info!("mounting archive VFS for origin={}", origin);
    let (upstream_vfs, archive_path) = ctx.registry.resolve(&origin)?;

    let upstream_desc = upstream_vfs.descriptor();
    let upstream_meta = upstream_vfs.mount_meta();
    let display_path = upstream_desc.format_path(&origin.path, &upstream_meta);
    // Capture the upstream's *own* breadcrumb segmentation/styling for
    // the origin so the archive renders it in the upstream's convention
    // instead of assuming Unix `/`.
    let origin_crumbs: Vec<String> = upstream_desc
        .breadcrumbs(&origin.path, &upstream_meta)
        .into_iter()
        .map(|b| b.label)
        .collect();
    let sep = upstream_sep(&origin_crumbs);
    let mount_meta = bincode::serialize(&ArchiveMeta {
        display: display_path.clone(),
        origin_crumbs,
        sep,
    })
    .unwrap_or_default();

    let vfs: Arc<dyn Vfs> = if is_zip_name(archive_path.as_wire_str()) {
        Arc::new(ZipArchiveVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            display_path,
            ctx.askpass_provider.cloned(),
            ctx.progress_reporter.clone(),
        ))
    } else {
        Arc::new(TarArchiveVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            ctx.progress_reporter.clone(),
        ))
    };
    Ok(vfs)
}

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
    let meta = decode_meta(mount_meta);
    if path.is_root() {
        meta.display
    } else {
        let sep = meta.sep.to_string();
        let inner: Vec<&str> = path.components().collect();
        format!("{}{sep}{}", meta.display, inner.join(&sep))
    }
}

fn archive_breadcrumbs(path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
    let ArchiveMeta {
        display,
        origin_crumbs,
        sep,
    } = decode_meta(mount_meta);

    // Inner archive path segments (no leading slash, no empties).
    let segments: Vec<&str> = path.components().collect();

    // Origin crumb labels in the *upstream's* style (`C:\`, `Users\`, …
    // or `/`, `home/`, …). Legacy/raw bytes have none → Unix-split the
    // display string and prepend a `/` root, the historical behaviour.
    let origin_labels: Vec<String> = if origin_crumbs.is_empty() {
        std::iter::once("/".to_string())
            .chain(
                display
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            )
            .collect()
    } else {
        origin_crumbs
    };

    let n = origin_labels.len();
    let mut crumbs = Vec::with_capacity(n + segments.len());
    // Origin crumbs navigate via `..` to escape back into the parent VFS;
    // the archive-file crumb (last origin) is the archive root.
    for (i, label) in origin_labels.iter().enumerate() {
        let depth_from_root = n - 1 - i;
        let nav_path = if depth_from_root == 0 {
            "/".to_string()
        } else {
            let mut p = String::from("/");
            for _ in 0..depth_from_root {
                p.push_str("../");
            }
            p.pop();
            p
        };
        // Non-last origin labels already carry the upstream separator;
        // the archive-file label is bare, so append the archive's `/`
        // when inner segments follow it.
        let is_archive_file = i == n - 1;
        let crumb_label = if is_archive_file && !segments.is_empty() {
            format!("{label}{sep}")
        } else {
            label.clone()
        };
        crumbs.push(Breadcrumb {
            label: crumb_label,
            nav_path,
        });
    }

    // Inner archive path segments.
    let mut accumulated = String::new();
    for (i, seg) in segments.iter().enumerate() {
        accumulated.push('/');
        accumulated.push_str(seg);
        crumbs.push(Breadcrumb {
            label: if i == segments.len() - 1 {
                seg.to_string()
            } else {
                format!("{}{sep}", seg)
            },
            nav_path: accumulated.clone(),
        });
    }

    crumbs
}

fn archive_try_parse_display_path(input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
    let display = decode_meta(mount_meta).display;
    if input == display {
        return Some(DisplayPathMatch::exact(PathBuf::root()));
    }
    let rest = input.strip_prefix(&display)?;
    let rest = rest.strip_prefix('/')?;
    Some(DisplayPathMatch::exact(PathBuf::from_wire_str(rest)))
}

fn archive_mount_label(mount_meta: &[u8]) -> Option<String> {
    let display = decode_meta(mount_meta).display;
    (!display.is_empty()).then_some(display)
}

// ---------------------------------------------------------------------------
// Directory tree built from archive index
// ---------------------------------------------------------------------------

/// Maximum number of symlink hops before we declare a loop (matches Linux MAXSYMLINKS).
const MAX_SYMLINK_HOPS: usize = 40;

struct DirectoryTree {
    dirs: HashMap<StdPathBuf, Vec<File>>,
}

impl DirectoryTree {
    fn list(&self, path: &StdPath) -> Result<Vec<File>, Error> {
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

    fn file_info(&self, path: &StdPath) -> Result<File, Error> {
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
    fn resolve_path(&self, path: &StdPath, follow_last: bool) -> Result<StdPathBuf, Error> {
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

fn normalize_dir_path(path: &StdPath) -> StdPathBuf {
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
fn normalize_path_dotdot(path: &StdPath) -> StdPathBuf {
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
fn normalized_to_string(path: &StdPath) -> String {
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
                key: None,
                source: None,
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

/// Adapter that implements `Read + Seek` by calling `upstream.read_range()`
/// via `Handle::block_on()`. Designed to be used inside `spawn_blocking`.
struct RangeReadAdapter {
    handle: tokio::runtime::Handle,
    upstream: Arc<dyn Vfs>,
    // Our VFS path: passed straight to the upstream `Vfs::read_range`.
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
            name: name.clone(),
            size,
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

#[cfg(test)]
mod display_tests {
    use super::*;

    fn meta(display: &str, crumbs: &[&str], sep: char) -> Vec<u8> {
        bincode::serialize(&ArchiveMeta {
            display: display.to_string(),
            origin_crumbs: crumbs.iter().map(|s| s.to_string()).collect(),
            sep,
        })
        .unwrap()
    }

    #[test]
    fn windows_origin_renders_backslash_throughout() {
        let m = meta(
            r"C:\Users\Tibor\Downloads\hello.zip",
            &[r"C:\", r"Users\", r"Tibor\", r"Downloads\", "hello.zip"],
            '\\',
        );
        let p = PathBuf::from_wire_str("/sub/file");
        assert_eq!(
            archive_format_path(&p, &m),
            r"C:\Users\Tibor\Downloads\hello.zip\sub\file"
        );
        let joined: String = archive_breadcrumbs(&p, &m)
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(joined, r"C:\Users\Tibor\Downloads\hello.zip\sub\file");
        // Archive root: the origin verbatim, no trailing separator.
        assert_eq!(
            archive_format_path(&PathBuf::root(), &m),
            r"C:\Users\Tibor\Downloads\hello.zip"
        );
    }

    #[test]
    fn unix_origin_unchanged() {
        let m = meta("/home/u/x.zip", &["/", "home/", "u/", "x.zip"], '/');
        let p = PathBuf::from_wire_str("/a/b");
        assert_eq!(archive_format_path(&p, &m), "/home/u/x.zip/a/b");
        let joined: String = archive_breadcrumbs(&p, &m)
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(joined, "/home/u/x.zip/a/b");
    }
}
