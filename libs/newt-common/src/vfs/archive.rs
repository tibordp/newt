use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::info;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode, UserGroup};
use crate::{Error, ErrorKind};

use super::{Breadcrumb, RegisteredDescriptor, Vfs, VfsDescriptor, VfsPath};

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
// TarArchiveVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct TarArchiveVfsDescriptor;

impl VfsDescriptor for TarArchiveVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "archive"
    }
    fn display_name(&self) -> &'static str {
        "Archive"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn has_origin(&self) -> bool {
        true
    }
    fn can_watch(&self) -> bool {
        false
    }
    fn can_read_sync(&self) -> bool {
        false
    }
    fn can_read_async(&self) -> bool {
        true
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        false
    }
    fn can_create_directory(&self) -> bool {
        false
    }
    fn can_create_symlink(&self) -> bool {
        false
    }
    fn can_touch(&self) -> bool {
        false
    }
    fn can_truncate(&self) -> bool {
        false
    }
    fn can_set_metadata(&self) -> bool {
        false
    }
    fn can_remove(&self) -> bool {
        false
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_fs_stats(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        false
    }
    fn can_copy_within(&self) -> bool {
        false
    }
    fn can_hard_link(&self) -> bool {
        false
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        archive_format_path(path, mount_meta)
    }
    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        archive_breadcrumbs(path, mount_meta)
    }
    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<PathBuf> {
        archive_try_parse_display_path(input, mount_meta)
    }
    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        archive_mount_label(mount_meta)
    }
}

static TAR_ARCHIVE_VFS_DESCRIPTOR: TarArchiveVfsDescriptor = TarArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&TAR_ARCHIVE_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// ZipArchiveVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ZipArchiveVfsDescriptor;

impl VfsDescriptor for ZipArchiveVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "archive_zip"
    }
    fn display_name(&self) -> &'static str {
        "Archive (ZIP)"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn has_origin(&self) -> bool {
        true
    }
    fn can_watch(&self) -> bool {
        false
    }
    fn can_read_sync(&self) -> bool {
        true
    }
    fn can_read_async(&self) -> bool {
        false
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        false
    }
    fn can_create_directory(&self) -> bool {
        false
    }
    fn can_create_symlink(&self) -> bool {
        false
    }
    fn can_touch(&self) -> bool {
        false
    }
    fn can_truncate(&self) -> bool {
        false
    }
    fn can_set_metadata(&self) -> bool {
        false
    }
    fn can_remove(&self) -> bool {
        false
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        false
    }
    fn can_fs_stats(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        false
    }
    fn can_copy_within(&self) -> bool {
        false
    }
    fn can_hard_link(&self) -> bool {
        false
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        archive_format_path(path, mount_meta)
    }
    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        archive_breadcrumbs(path, mount_meta)
    }
    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<PathBuf> {
        archive_try_parse_display_path(input, mount_meta)
    }
    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        archive_mount_label(mount_meta)
    }
}

static ZIP_ARCHIVE_VFS_DESCRIPTOR: ZipArchiveVfsDescriptor = ZipArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&ZIP_ARCHIVE_VFS_DESCRIPTOR));

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
            mode: Mode(0o755),
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
    let s = s.trim_end_matches('/');
    PathBuf::from(s)
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
                mode: Mode(0o755),
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

// ---------------------------------------------------------------------------
// TarArchiveVfs
// ---------------------------------------------------------------------------

pub struct TarArchiveVfs {
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    index: tokio::sync::OnceCell<(iluvatar::ArchiveIndex, DirectoryTree)>,
}

impl TarArchiveVfs {
    pub fn new(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        origin: VfsPath,
        mount_meta: Vec<u8>,
    ) -> Self {
        Self {
            upstream,
            archive_path,
            origin,
            mount_meta,
            index: tokio::sync::OnceCell::new(),
        }
    }

    async fn ensure_indexed(&self) -> Result<&(iluvatar::ArchiveIndex, DirectoryTree), Error> {
        self.index
            .get_or_try_init(|| async {
                info!(
                    "archive: indexing tar/cpio archive {}",
                    self.archive_path.display()
                );

                let details = self.upstream.file_details(&self.archive_path).await?;
                let file_size = details.size;
                let descriptor = self.upstream.descriptor();

                let index = if descriptor.can_read_sync() {
                    // Sync upstream (e.g. local filesystem): stream via sync reader
                    let reader = self.upstream.open_read_sync(&self.archive_path).await?;
                    tokio::task::spawn_blocking(move || {
                        let archive = iluvatar::sync::Archive::from_reader(reader, file_size)
                            .map_err(|e| {
                                Error::custom(format!("failed to index archive: {}", e))
                            })?;
                        let tree = build_directory_tree_from_iluvatar(archive.list());
                        let (_, index) = archive.into_parts();
                        Ok::<_, Error>((index, tree))
                    })
                    .await
                    .map_err(|e| Error::custom(format!("indexing task panicked: {}", e)))??
                } else if descriptor.can_read_async() {
                    // Async upstream (e.g. S3, SFTP): stream via async reader
                    let reader = self.upstream.open_read_async(&self.archive_path).await?;
                    let archive = iluvatar::tokio::Archive::from_reader(reader, file_size)
                        .await
                        .map_err(|e| Error::custom(format!("failed to index archive: {}", e)))?;
                    let tree = build_directory_tree_from_iluvatar(archive.list());
                    let (_, index) = archive.into_parts();
                    (index, tree)
                } else {
                    return Err(Error::custom(
                        "upstream VFS supports neither sync nor async read",
                    ));
                };

                info!(
                    "archive: indexed {} entries from {}",
                    index.0.entries.len(),
                    self.archive_path.display()
                );

                Ok(index)
            })
            .await
    }

    /// Drive the sans-I/O ReadEngine using upstream's read_range for I/O.
    async fn drive_read_engine(
        &self,
        path_in_archive: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Vec<u8>, Error> {
        let (index, _) = self.ensure_indexed().await?;

        let mut engine = if let Some((offset, len)) = range {
            iluvatar::ReadEngine::new_range(index, path_in_archive, offset, len)
        } else {
            iluvatar::ReadEngine::new(index, path_in_archive)
        }
        .map_err(|e| Error::custom(format!("failed to create read engine: {}", e)))?;

        let mut output = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut position: u64 = 0;
        loop {
            match engine.step() {
                iluvatar::EngineRequest::NeedInput => {
                    let chunk = self
                        .upstream
                        .read_range(&self.archive_path, position, buf.len() as u64)
                        .await?;
                    if chunk.data.is_empty() {
                        engine.signal_eof();
                    } else {
                        position += chunk.data.len() as u64;
                        engine.provide_data(&chunk.data);
                    }
                }
                iluvatar::EngineRequest::SeekAndRead { offset, len } => {
                    position = offset;
                    let chunk = self
                        .upstream
                        .read_range(&self.archive_path, position, len as u64)
                        .await?;
                    if chunk.data.is_empty() {
                        engine.signal_eof();
                    } else {
                        position += chunk.data.len() as u64;
                        engine.provide_data(&chunk.data);
                    }
                }
                iluvatar::EngineRequest::OutputReady => loop {
                    let n = engine.read_output(&mut buf);
                    if n == 0 {
                        break;
                    }
                    output.extend_from_slice(&buf[..n]);
                },
                iluvatar::EngineRequest::Done => break,
                iluvatar::EngineRequest::Error(e) => {
                    return Err(Error::custom(format!(
                        "failed to read file from archive: {}",
                        e
                    )));
                }
            }
        }

        Ok(output)
    }
}

fn build_directory_tree_from_iluvatar(entries: Vec<&iluvatar::IndexEntry>) -> DirectoryTree {
    let mut dirs: HashMap<PathBuf, Vec<File>> = HashMap::new();
    let mut seen_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    dirs.insert(PathBuf::from(""), Vec::new());
    seen_dirs.insert(PathBuf::from(""));

    for entry in &entries {
        let path = entry.path.trim_start_matches('/').trim_end_matches('/');
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
            mode: Mode(entry.mode),
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

#[async_trait::async_trait]
impl Vfs for TarArchiveVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &TAR_ARCHIVE_VFS_DESCRIPTOR
    }

    fn origin(&self) -> Option<&VfsPath> {
        Some(&self.origin)
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.mount_meta.clone()
    }

    async fn list_files(
        &self,
        path: &Path,
        _batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
        let (_, tree) = self.ensure_indexed().await?;
        tree.list(path)
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        // Archive is immutable while mounted — block forever.
        std::future::pending().await
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let (index, _) = self.ensure_indexed().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();

        let entry = index
            .get(&path_str)
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;

        Ok(FileDetails {
            size: entry.size,
            mime_type: None,
            is_dir: entry.entry_type.is_directory(),
            is_symlink: matches!(entry.entry_type, iluvatar::EntryType::SymLink),
            symlink_target: entry.link_target.as_ref().map(PathBuf::from),
            user: Some(UserGroup::Id(entry.uid as u32)),
            group: Some(UserGroup::Id(entry.gid as u32)),
            mode: Some(Mode(entry.mode)),
            modified: mtime_to_i128(entry.mtime),
            accessed: None,
            created: None,
        })
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let (_, tree) = self.ensure_indexed().await?;
        tree.file_info(path)
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy().to_string();
        let data = self.drive_read_engine(&path_str, None).await?;
        Ok(Box::new(std::io::Cursor::new(data)))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let (index, _) = self.ensure_indexed().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();

        let entry = index
            .get(&path_str)
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;
        let total_size = entry.size;
        let path_str = path_str.to_string();

        let data = self
            .drive_read_engine(&path_str, Some((offset, length)))
            .await?;

        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }
}

// ---------------------------------------------------------------------------
// ZipArchiveVfs
// ---------------------------------------------------------------------------

pub struct ZipArchiveVfs {
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    index: tokio::sync::OnceCell<(ZipIndex, DirectoryTree)>,
}

struct ZipIndex {
    entries: HashMap<String, ZipEntry>,
}

struct ZipEntry {
    size: u64,
    is_dir: bool,
    mode: u32,
    mtime: Option<u64>,
}

impl ZipArchiveVfs {
    pub fn new(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        origin: VfsPath,
        mount_meta: Vec<u8>,
    ) -> Self {
        Self {
            upstream,
            archive_path,
            origin,
            mount_meta,
            index: tokio::sync::OnceCell::new(),
        }
    }

    async fn ensure_indexed(&self) -> Result<&(ZipIndex, DirectoryTree), Error> {
        self.index
            .get_or_try_init(|| async {
                info!(
                    "archive: indexing ZIP archive {}",
                    self.archive_path.display()
                );

                let details = self.upstream.file_details(&self.archive_path).await?;
                let file_size = details.size;
                let handle = tokio::runtime::Handle::current();

                let upstream = self.upstream.clone();
                let archive_path = self.archive_path.clone();

                let (zip_index, tree) = tokio::task::spawn_blocking(move || {
                    let adapter = RangeReadAdapter {
                        handle,
                        upstream,
                        archive_path,
                        file_size,
                        position: 0,
                    };
                    let zip = zip::ZipArchive::new(adapter)
                        .map_err(|e| Error::custom(format!("failed to read ZIP archive: {}", e)))?;
                    build_zip_index(zip)
                })
                .await
                .map_err(|e| Error::custom(format!("ZIP indexing task panicked: {}", e)))??;

                info!(
                    "archive: indexed {} entries from ZIP {}",
                    zip_index.entries.len(),
                    self.archive_path.display()
                );

                Ok((zip_index, tree))
            })
            .await
    }

    /// Extract a file from the ZIP using a fresh RangeReadAdapter. Runs in
    /// spawn_blocking since the zip crate is sync-only.
    async fn extract_zip_file(&self, path_in_archive: String) -> Result<Vec<u8>, Error> {
        let details = self.upstream.file_details(&self.archive_path).await?;
        let file_size = details.size;
        let handle = tokio::runtime::Handle::current();
        let upstream = self.upstream.clone();
        let archive_path = self.archive_path.clone();

        tokio::task::spawn_blocking(move || {
            let adapter = RangeReadAdapter {
                handle,
                upstream,
                archive_path,
                file_size,
                position: 0,
            };
            let mut zip = zip::ZipArchive::new(adapter)
                .map_err(|e| Error::custom(format!("failed to open ZIP: {}", e)))?;
            let mut entry = zip
                .by_name(&path_in_archive)
                .map_err(|e| not_found(format!("file not found in ZIP: {}", e)))?;
            let mut buf = Vec::with_capacity(entry.size() as usize);
            std::io::Read::read_to_end(&mut entry, &mut buf)?;
            Ok(buf)
        })
        .await
        .map_err(|e| Error::custom(format!("ZIP extraction task panicked: {}", e)))?
    }
}

fn zip_mtime(entry: &zip::read::ZipFile) -> Option<u64> {
    let dt = entry.last_modified()?;
    let odt: time::OffsetDateTime = dt.try_into().ok()?;
    let unix = odt.unix_timestamp();
    if unix >= 0 { Some(unix as u64) } else { None }
}

fn build_zip_index(
    mut zip: zip::ZipArchive<RangeReadAdapter>,
) -> Result<(ZipIndex, DirectoryTree), Error> {
    let mut entries = HashMap::new();
    let mut dirs: HashMap<PathBuf, Vec<File>> = HashMap::new();
    let mut seen_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    dirs.insert(PathBuf::from(""), Vec::new());
    seen_dirs.insert(PathBuf::from(""));

    for i in 0..zip.len() {
        let entry = zip
            .by_index_raw(i)
            .map_err(|e| Error::custom(format!("failed to read ZIP entry: {}", e)))?;

        let raw_name = entry.name().to_string();
        let path = raw_name.trim_start_matches('/').trim_end_matches('/');
        if path.is_empty() {
            continue;
        }

        let is_dir = entry.is_dir();
        let size = entry.size();
        let mode = entry
            .unix_mode()
            .unwrap_or(if is_dir { 0o755 } else { 0o644 });
        let mtime = zip_mtime(&entry);

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

        let file = File {
            name: name.clone(),
            size: if is_dir { None } else { Some(size) },
            is_dir,
            is_hidden: name.starts_with('.'),
            is_symlink: false,
            symlink_target: None,
            user: None,
            group: None,
            mode: Mode(mode),
            modified: mtime.and_then(mtime_to_i128),
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
            entries.insert(
                path.to_string(),
                ZipEntry {
                    size,
                    is_dir,
                    mode,
                    mtime,
                },
            );
            continue;
        }

        dirs.entry(parent).or_default().push(file);

        if is_dir {
            seen_dirs.insert(entry_path.clone());
            dirs.entry(entry_path).or_default();
        }

        entries.insert(
            path.to_string(),
            ZipEntry {
                size,
                is_dir,
                mode,
                mtime,
            },
        );
    }

    Ok((ZipIndex { entries }, DirectoryTree { dirs }))
}

#[async_trait::async_trait]
impl Vfs for ZipArchiveVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &ZIP_ARCHIVE_VFS_DESCRIPTOR
    }

    fn origin(&self) -> Option<&VfsPath> {
        Some(&self.origin)
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.mount_meta.clone()
    }

    async fn list_files(
        &self,
        path: &Path,
        _batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
        let (_, tree) = self.ensure_indexed().await?;
        tree.list(path)
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        std::future::pending().await
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let (index, _) = self.ensure_indexed().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();

        let entry = index
            .entries
            .get(path_str.as_ref())
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;

        Ok(FileDetails {
            size: entry.size,
            mime_type: None,
            is_dir: entry.is_dir,
            is_symlink: false,
            symlink_target: None,
            user: None,
            group: None,
            mode: Some(Mode(entry.mode)),
            modified: entry.mtime.and_then(mtime_to_i128),
            accessed: None,
            created: None,
        })
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let (_, tree) = self.ensure_indexed().await?;
        tree.file_info(path)
    }

    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy().to_string();
        let data = self.extract_zip_file(path_str).await?;
        Ok(Box::new(std::io::Cursor::new(data)))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let (index, _) = self.ensure_indexed().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();

        let entry = index
            .entries
            .get(path_str.as_ref())
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;
        let total_size = entry.size;

        let data = self.extract_zip_file(path_str.to_string()).await?;
        let start = (offset as usize).min(data.len());
        let end = ((offset + length) as usize).min(data.len());

        Ok(FileChunk {
            data: data[start..end].to_vec(),
            offset,
            total_size,
        })
    }
}
