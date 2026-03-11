use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::info;
use tokio::sync::mpsc;

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode};

use super::super::{Breadcrumb, RegisteredDescriptor, Vfs, VfsDescriptor, VfsPath};
use super::{
    DirectoryTree, RangeReadAdapter, archive_breadcrumbs, archive_format_path, archive_mount_label,
    archive_try_parse_display_path, ensure_ancestors, mtime_to_i128, normalize_dir_path, not_found,
};

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
    fn auto_mount_request(&self) -> Option<super::super::MountRequest> {
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
    fn can_stat_directories(&self) -> bool {
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

static ZIP_ARCHIVE_VFS_DESCRIPTOR: ZipArchiveVfsDescriptor = ZipArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&ZIP_ARCHIVE_VFS_DESCRIPTOR));

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
    /// Original name as stored in the ZIP archive (for `by_name` lookups).
    raw_name: String,
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
        let path = raw_name
            .trim_start_matches('/')
            .trim_start_matches("./")
            .trim_end_matches('/');
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
                    raw_name: raw_name.clone(),
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
                raw_name: raw_name.clone(),
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
        let (index, _) = self.ensure_indexed().await?;
        let normalized = normalize_dir_path(path);
        let path_str = normalized.to_string_lossy();
        let entry = index
            .entries
            .get(path_str.as_ref())
            .ok_or_else(|| not_found(format!("file not found in archive: {}", path_str)))?;
        let data = self.extract_zip_file(entry.raw_name.clone()).await?;
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

        let data = self.extract_zip_file(entry.raw_name.clone()).await?;
        let start = (offset as usize).min(data.len());
        let end = ((offset + length) as usize).min(data.len());

        Ok(FileChunk {
            data: data[start..end].to_vec(),
            offset,
            total_size,
        })
    }
}
