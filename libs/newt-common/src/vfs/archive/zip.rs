use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::info;
use tokio::sync::mpsc;

use crate::Error;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode};

use super::super::{
    Breadcrumb, DisplayPathMatch, RegisteredDescriptor, Vfs, VfsDescriptor, VfsPath,
};
use super::{
    DirectoryTree, RangeReadAdapter, archive_breadcrumbs, archive_format_path, archive_mount_label,
    archive_try_parse_display_path, ensure_ancestors, mtime_to_i64, normalize_dir_path, not_found,
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
    fn is_ephemeral(&self) -> bool {
        true
    }
    fn auto_refresh(&self) -> bool {
        false
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
    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        archive_try_parse_display_path(input, mount_meta)
    }
    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        archive_mount_label(mount_meta)
    }
}

static ZIP_ARCHIVE_VFS_DESCRIPTOR: ZipArchiveVfsDescriptor = ZipArchiveVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&ZIP_ARCHIVE_VFS_DESCRIPTOR));

#[cfg(test)]
#[path = "zip_tests.rs"]
mod zip_tests;

// ---------------------------------------------------------------------------
// ZipArchiveVfs
// ---------------------------------------------------------------------------

pub struct ZipArchiveVfs {
    upstream: Arc<dyn Vfs>,
    archive_path: PathBuf,
    origin: VfsPath,
    mount_meta: Vec<u8>,
    /// Pretty rendering of the archive's origin path, used in askpass
    /// prompts when an encrypted entry needs unlocking.
    display_path: String,
    /// Optional askpass provider used to prompt for the archive password
    /// the first time an encrypted entry is read. Without this, reads of
    /// encrypted entries fail with `PermissionDenied`.
    askpass: Option<Arc<dyn crate::askpass::AskpassProvider>>,
    /// Used to emit a one-shot "Indexing ZIP" progress message while
    /// the central directory is being read. ZIP's central directory
    /// is at EOF and read in a single pass; there's no meaningful
    /// mid-parse progress to report.
    reporter: Arc<dyn super::super::ProgressReporter>,
    /// Cached password for encrypted entries. Filled on first successful
    /// decrypt. The ZIP spec allows different passwords per entry; we
    /// remember the most recently successful one and re-prompt if it
    /// fails on a later entry.
    password: tokio::sync::Mutex<Option<Vec<u8>>>,
    /// Bumped every time the user dismisses an unlock prompt. Pending
    /// reads that started before the dismissal observe a higher
    /// generation than the one they captured at entry and bail out with
    /// `Cancelled` instead of opening a fresh prompt — so dismissing a
    /// single dialog cancels the whole "batch" of concurrent reads (e.g.
    /// the chunked range reads the file viewer fans out on F3) rather
    /// than queueing N more prompts behind it.
    dismiss_gen: std::sync::atomic::AtomicU64,
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
    is_encrypted: bool,
    mode: Option<u32>,
    mtime: Option<u64>,
}

impl ZipArchiveVfs {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        upstream: Arc<dyn Vfs>,
        archive_path: PathBuf,
        origin: VfsPath,
        mount_meta: Vec<u8>,
        display_path: String,
        askpass: Option<Arc<dyn crate::askpass::AskpassProvider>>,
        reporter: Arc<dyn super::super::ProgressReporter>,
    ) -> Self {
        Self {
            upstream,
            archive_path,
            origin,
            mount_meta,
            display_path,
            askpass,
            reporter,
            password: tokio::sync::Mutex::new(None),
            dismiss_gen: std::sync::atomic::AtomicU64::new(0),
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

                // One-shot progress message; clear on exit.
                let mut extra = std::collections::BTreeMap::new();
                extra.insert("path".to_string(), self.display_path.clone());
                self.reporter.report(Some(super::super::VfsProgress {
                    stage: "Indexing".into(),
                    processed: None,
                    total: None,
                    extra,
                }));
                struct ClearOnDrop<'a>(&'a Arc<dyn super::super::ProgressReporter>);
                impl Drop for ClearOnDrop<'_> {
                    fn drop(&mut self) {
                        self.0.report(None);
                    }
                }
                let _clear = ClearOnDrop(&self.reporter);

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

    /// Extract an entry from the ZIP. Cleartext entries take a fast
    /// path; encrypted entries try the cached password first, and on
    /// `PermissionDenied` prompt the user (via the configured askpass)
    /// until a working password is supplied or the prompt is cancelled.
    /// A successfully-validated password is cached for future reads.
    ///
    /// Concurrency: the prompt-and-validate phase is serialised by the
    /// password mutex. To prevent N concurrent reads from all queueing
    /// up their own prompts after the user dismisses one of them, we
    /// snapshot a "dismiss generation" before queueing and bail out if
    /// it has advanced by the time we hold the lock — a fresh read
    /// (started after the dismissal) sees the new generation and is
    /// allowed to prompt.
    async fn extract_zip_file(
        &self,
        path_in_archive: String,
        encrypted: bool,
    ) -> Result<Vec<u8>, Error> {
        use std::sync::atomic::Ordering;

        if !encrypted {
            return self.spawn_extract(path_in_archive, None).await;
        }

        // Snapshot the dismissal counter at task entry — *before* any
        // lock or prompt. This pins our "birth time" relative to
        // dismissal events: peers that increment the counter while
        // we're queued on the prompt lock will then make our post-lock
        // check trip and we'll bail instead of opening a fresh prompt.
        // (Capturing it later — e.g. just before the slow-path lock —
        // races with the dismissal-and-release sequence and lets a
        // queued task think it was born after the dismissal.)
        let my_gen = self.dismiss_gen.load(Ordering::Acquire);

        // Fast path: try the currently-cached password without serialising
        // on the prompt lock once the archive has been unlocked.
        let cached_at_start = self.password.lock().await.clone();
        if let Some(pw) = cached_at_start {
            match self.spawn_extract(path_in_archive.clone(), Some(pw)).await {
                Ok(buf) => return Ok(buf),
                Err(e) if e.kind == crate::ErrorKind::PermissionDenied => {}
                Err(e) => return Err(e),
            }
        }

        let mut guard = self.password.lock().await;

        if self.dismiss_gen.load(Ordering::Acquire) > my_gen {
            // A peer dismissed an unlock prompt while we were queued.
            // Treat the whole batch as cancelled rather than queueing N
            // more prompts behind theirs.
            return Err(Error::cancelled());
        }

        // Did a peer set a (different) password while we waited?
        if let Some(pw) = guard.clone() {
            match self.spawn_extract(path_in_archive.clone(), Some(pw)).await {
                Ok(buf) => return Ok(buf),
                Err(e) if e.kind == crate::ErrorKind::PermissionDenied => {}
                Err(e) => return Err(e),
            }
        }

        let askpass = self.askpass.as_ref().ok_or_else(|| Error {
            kind: crate::ErrorKind::PermissionDenied,
            message: format!(
                "ZIP archive {} entry is encrypted, but no askpass provider is configured",
                self.display_path
            ),
        })?;
        let mut prompt = format!("Password for archive {}:", self.display_path);
        loop {
            let resp = askpass
                .prompt(crate::askpass::AskpassRequest {
                    prompt_type: crate::askpass::PromptType::Secret,
                    prompt: prompt.clone(),
                })
                .await;
            let Some(s) = resp.0 else {
                self.dismiss_gen.fetch_add(1, Ordering::Release);
                return Err(Error::cancelled());
            };
            let bytes = s.into_bytes();
            match self
                .spawn_extract(path_in_archive.clone(), Some(bytes.clone()))
                .await
            {
                Ok(buf) => {
                    *guard = Some(bytes);
                    return Ok(buf);
                }
                Err(e) if e.kind == crate::ErrorKind::PermissionDenied => {
                    prompt = format!(
                        "Incorrect password — try again. Password for archive {}:",
                        self.display_path
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Run a one-shot ZIP extraction inside `spawn_blocking`.
    async fn spawn_extract(
        &self,
        path_in_archive: String,
        password: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, Error> {
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
            let mut entry = match password {
                Some(pw) => zip
                    .by_name_decrypt(&path_in_archive, &pw)
                    .map_err(map_zip_error)?,
                None => zip.by_name(&path_in_archive).map_err(map_zip_error)?,
            };
            let mut buf = Vec::with_capacity(entry.size() as usize);
            std::io::Read::read_to_end(&mut entry, &mut buf)?;
            Ok(buf)
        })
        .await
        .map_err(|e| Error::custom(format!("ZIP extraction task panicked: {}", e)))?
    }
}

/// Map a `zip::result::ZipError` to our `Error`, distinguishing missing
/// entries, password problems, and everything else.
fn map_zip_error(e: zip::result::ZipError) -> Error {
    use zip::result::ZipError;
    match e {
        ZipError::FileNotFound => not_found("file not found in ZIP"),
        ZipError::InvalidPassword => Error {
            kind: crate::ErrorKind::PermissionDenied,
            message: "incorrect password for ZIP entry".into(),
        },
        ZipError::UnsupportedArchive(msg) if msg == ZipError::PASSWORD_REQUIRED => Error {
            kind: crate::ErrorKind::PermissionDenied,
            message: "ZIP entry requires a password".into(),
        },
        other => Error::custom(format!("ZIP extraction failed: {}", other)),
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
        let unix_mode = entry.unix_mode();
        let mtime = zip_mtime(&entry);
        let is_encrypted = entry.encrypted();

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
            mode: unix_mode.map(Mode),
            modified: mtime.and_then(mtime_to_i64),
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
            entries.insert(
                path.to_string(),
                ZipEntry {
                    raw_name: raw_name.clone(),
                    size,
                    is_dir,
                    is_encrypted,
                    mode: unix_mode,
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
                is_encrypted,
                mode: unix_mode,
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
        _batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::super::VfsFileList, Error> {
        let (_, tree) = self.ensure_indexed().await?;
        Ok(tree.list(path)?.into())
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
            mime_type: crate::file_reader::guess_mime_type(path),
            is_dir: entry.is_dir,
            is_symlink: false,
            symlink_target: None,
            user: None,
            group: None,
            mode: entry.mode.map(Mode),
            modified: entry.mtime.and_then(mtime_to_i64),
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
        let data = self
            .extract_zip_file(entry.raw_name.clone(), entry.is_encrypted)
            .await?;
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

        let data = self
            .extract_zip_file(entry.raw_name.clone(), entry.is_encrypted)
            .await?;
        let start = (offset as usize).min(data.len());
        let end = ((offset + length) as usize).min(data.len());

        Ok(FileChunk {
            data: data[start..end].to_vec(),
            offset,
            total_size,
        })
    }
}
