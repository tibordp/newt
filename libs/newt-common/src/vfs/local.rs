use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use log::{debug, warn};
use notify::event::RemoveKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;

use std::os::unix::prelude::MetadataExt;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, ListFilesOptions, Mode, VfsFileList};
use crate::{Error, ToUnix};

use std::path::PathBuf;

use super::{Breadcrumb, RegisteredDescriptor, Vfs, VfsDescriptor, VfsMetadata, VfsSpaceInfo};

// ---------------------------------------------------------------------------
// LocalVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LocalVfsDescriptor;

impl VfsDescriptor for LocalVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "local"
    }
    fn display_name(&self) -> &'static str {
        "Local"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read_sync(&self) -> bool {
        true
    }
    fn can_read_async(&self) -> bool {
        false
    }
    fn can_overwrite_sync(&self) -> bool {
        true
    }
    fn can_overwrite_async(&self) -> bool {
        false
    }
    fn can_create_directory(&self) -> bool {
        true
    }
    fn can_create_symlink(&self) -> bool {
        true
    }
    fn can_touch(&self) -> bool {
        true
    }
    fn can_truncate(&self) -> bool {
        true
    }
    fn can_set_metadata(&self) -> bool {
        true
    }
    fn can_remove(&self) -> bool {
        true
    }
    fn can_remove_tree(&self) -> bool {
        true
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_rename(&self) -> bool {
        true
    }
    fn can_copy_within(&self) -> bool {
        true
    }
    fn can_hard_link(&self) -> bool {
        true
    }

    fn format_path(&self, path: &Path) -> String {
        path.to_string_lossy().to_string()
    }

    fn breadcrumbs(&self, path: &Path) -> Vec<Breadcrumb> {
        let mut crumbs = Vec::new();
        let s = path.to_string_lossy();
        let segments: Vec<&str> = s.split('/').filter(|s| !s.is_empty()).collect();

        crumbs.push(Breadcrumb {
            label: "/".to_string(),
            nav_path: "/".to_string(),
        });

        let mut accumulated = String::new();
        for (i, seg) in segments.iter().enumerate() {
            accumulated.push('/');
            accumulated.push_str(seg);
            let is_last = i == segments.len() - 1;
            crumbs.push(Breadcrumb {
                label: if is_last {
                    seg.to_string()
                } else {
                    format!("{}/", seg)
                },
                nav_path: accumulated.clone(),
            });
        }

        crumbs
    }

    fn try_parse_display_path(&self, _input: &str) -> Option<PathBuf> {
        None
    }
}

pub static LOCAL_VFS_DESCRIPTOR: LocalVfsDescriptor = LocalVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&LOCAL_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// LocalVfs — wraps existing filesystem::Local + file_reader::Local logic
// ---------------------------------------------------------------------------

pub struct LocalVfs {
    fs_cache: Arc<crate::filesystem::UidGidCache>,
}

impl LocalVfs {
    pub fn new() -> Self {
        Self {
            fs_cache: Arc::new(crate::filesystem::UidGidCache::new()),
        }
    }
}

impl Default for LocalVfs {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Vfs for LocalVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &LOCAL_VFS_DESCRIPTOR
    }

    async fn list_files(
        &self,
        path: &Path,
        opts: ListFilesOptions,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<VfsFileList, Error> {
        assert!(path.is_absolute());
        let mut path = path.to_path_buf();
        loop {
            match tokio::task::spawn_blocking({
                let path = path.clone();
                let cache = self.fs_cache.clone();
                let batch_tx = batch_tx.as_ref().cloned();
                move || -> Result<Vec<File>, Error> {
                    const BATCH_SIZE: usize = 500;

                    let mut ret = Vec::new();
                    let mut batch = Vec::new();

                    if let Some(parent) = path.parent() {
                        let metadata = parent.symlink_metadata()?;
                        let mode = metadata.mode();
                        let file = File {
                            name: "..".to_string(),
                            size: None,
                            is_dir: true,
                            is_symlink: metadata.is_symlink(),
                            symlink_target: None,
                            is_hidden: false,
                            user: cache.user_name(metadata.uid()).ok(),
                            group: cache.group_name(metadata.gid()).ok(),
                            mode: Mode(mode),
                            modified: metadata.modified().map(|t| t.to_unix()).ok(),
                            accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                            created: metadata.created().map(|t| t.to_unix()).ok(),
                        };
                        batch.push(file.clone());
                        ret.push(file);
                    }

                    for maybe_entry in std::fs::read_dir(&path)? {
                        let entry = maybe_entry?;
                        let metadata = entry.metadata()?;
                        let file_type = metadata.file_type();

                        let name = entry.file_name().into_string().unwrap();
                        let mut is_dir = file_type.is_dir();

                        let symlink_target = if file_type.is_symlink() {
                            let target_metadata = std::fs::metadata(entry.path());
                            if let Ok(target_metadata) = target_metadata {
                                is_dir = target_metadata.is_dir();
                            }
                            std::fs::read_link(entry.path()).ok()
                        } else {
                            None
                        };

                        let mode = metadata.mode();
                        let file = File {
                            name: name.clone(),
                            size: (!is_dir).then_some(metadata.len()),
                            is_dir,
                            is_symlink: file_type.is_symlink(),
                            symlink_target,
                            is_hidden: name.starts_with('.'),
                            user: cache.user_name(metadata.uid()).ok(),
                            group: cache.group_name(metadata.gid()).ok(),
                            mode: Mode(mode),
                            modified: metadata.modified().map(|t| t.to_unix()).ok(),
                            accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                            created: metadata.created().map(|t| t.to_unix()).ok(),
                        };
                        batch.push(file.clone());
                        ret.push(file);

                        if batch.len() >= BATCH_SIZE {
                            if let Some(ref tx) = batch_tx {
                                if tx.send(std::mem::take(&mut batch)).is_err() {
                                    // Receiver dropped — cancelled
                                    return Ok(ret);
                                }
                            } else {
                                batch.clear();
                            }
                        }
                    }

                    // Send any remaining entries as a final batch
                    if let Some(ref tx) = batch_tx {
                        if !batch.is_empty() {
                            let _ = tx.send(batch);
                        }
                    }

                    Ok(ret)
                }
            })
            .await?
            {
                Ok(files) => {
                    let stats = nix::sys::statvfs::statvfs(&path)
                        .ok()
                        .map(crate::filesystem::FsStats::from);
                    return Ok(VfsFileList {
                        path,
                        files,
                        fs_stats: stats,
                    });
                }
                Err(Error::Io(e)) => match (e.kind(), opts.strict) {
                    (std::io::ErrorKind::NotFound, false)
                    | (std::io::ErrorKind::NotADirectory, _) => {
                        if !path.pop() {
                            return Err(e.into());
                        }
                    }
                    _ => return Err(e.into()),
                },
                Err(e) => return Err(e),
            }
        }
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Arc::new(Mutex::new(Some(tx)));

        let mut watcher = {
            let path = path.clone();
            RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    match res {
                        Ok(event) => {
                            debug!("{:?} (while watching {})", event, path.display());
                            let should_notify = match event.kind {
                                EventKind::Remove(RemoveKind::Folder) => event
                                    .paths
                                    .iter()
                                    .any(|p| path.starts_with(p) || p.starts_with(&path)),
                                EventKind::Access(_) => false,
                                _ => event.paths.iter().any(|p| p.starts_with(&path)),
                            };

                            if should_notify {
                                if let Some(s) = tx.lock().take() {
                                    let _ = s.send(());
                                }
                            }
                        }
                        Err(e) => warn!("watch error: {:?}", e),
                    };
                },
                Config::default(),
            )?
        };

        let mut watch_path = path;
        loop {
            watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;
            if !watch_path.pop() {
                break;
            }
        }

        let _ = rx.await;
        Ok(())
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let path = path.to_path_buf();
        let cache = self.fs_cache.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;

            let symlink_meta = std::fs::symlink_metadata(&path)?;
            let is_symlink = symlink_meta.is_symlink();
            let symlink_target = if is_symlink {
                std::fs::read_link(&path).ok()
            } else {
                None
            };

            let meta = if is_symlink {
                std::fs::metadata(&path).unwrap_or(symlink_meta)
            } else {
                symlink_meta
            };

            let is_dir = meta.is_dir();
            let size = meta.len();
            let mode = meta.mode();

            // MIME detection for files
            let mime_type = if is_dir {
                None
            } else {
                let file = std::fs::File::open(&path)?;
                let mut buf = vec![0u8; 8192.min(size as usize)];
                let mut reader = std::io::BufReader::new(file);
                let n = reader.read(&mut buf)?;
                let header = &buf[..n];

                let detected = mimetype_detector::detect(header);
                if detected.is("application/octet-stream") {
                    // No specific match — fall back to null-byte heuristic
                    if !header.contains(&0) {
                        Some("text/plain".to_string())
                    } else {
                        Some("application/octet-stream".to_string())
                    }
                } else {
                    Some(detected.mime().to_string())
                }
            };

            Ok(FileDetails {
                size,
                mime_type,
                is_dir,
                is_symlink,
                symlink_target,
                user: cache.user_name(meta.uid()).ok(),
                group: cache.group_name(meta.gid()).ok(),
                mode: Some(Mode(mode)),
                modified: meta.modified().map(|t| t.to_unix()).ok(),
                accessed: meta.accessed().map(|t| t.to_unix()).ok(),
                created: meta.created().map(|t| t.to_unix()).ok(),
            })
        })
        .await?
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&path)?;
            let metadata = file.metadata()?;
            let total_size = metadata.len();
            file.seek(SeekFrom::Start(offset))?;
            let to_read = length.min(total_size.saturating_sub(offset)) as usize;
            let mut data = vec![0u8; to_read];
            let mut total_read = 0;
            while total_read < to_read {
                let n = file.read(&mut data[total_read..])?;
                if n == 0 {
                    break;
                }
                total_read += n;
            }
            data.truncate(total_read);
            Ok(FileChunk {
                data,
                offset,
                total_size,
            })
        })
        .await?
    }

    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let path = path.to_path_buf();
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::open(&path).map_err(Error::Io))
                .await??;
        Ok(Box::new(file))
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let path = path.to_path_buf();
        let cache = self.fs_cache.clone();
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            let is_symlink = meta.is_symlink();
            let symlink_target = if is_symlink {
                std::fs::read_link(&path).ok()
            } else {
                None
            };
            let mut is_dir = meta.is_dir();
            if is_symlink {
                if let Ok(target_meta) = std::fs::metadata(&path) {
                    is_dir = target_meta.is_dir();
                }
            }
            let mode = meta.mode();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(File {
                is_hidden: name.starts_with('.'),
                name,
                size: (!is_dir).then_some(meta.len()),
                is_dir,
                is_symlink,
                symlink_target,
                user: cache.user_name(meta.uid()).ok(),
                group: cache.group_name(meta.gid()).ok(),
                mode: Mode(mode),
                modified: meta.modified().map(|t| t.to_unix()).ok(),
                accessed: meta.accessed().map(|t| t.to_unix()).ok(),
                created: meta.created().map(|t| t.to_unix()).ok(),
            })
        })
        .await?
    }

    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
        let path = path.to_path_buf();
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::create(&path).map_err(Error::Io))
                .await??;
        Ok(Box::new(file))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&path).map_err(Error::Io))
            .await?
    }

    async fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let link = link.to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &link)?;
            #[cfg(not(unix))]
            return Err(Error::NotSupported);
            Ok(())
        })
        .await?
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::remove_file(&path)?;
            Ok(())
        })
        .await?
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::remove_dir(&path)?;
            Ok(())
        })
        .await?
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                // symlink_metadata doesn't follow symlinks, so this is a real directory
                std::fs::remove_dir_all(&path)?;
            } else {
                // Files and symlinks (including symlinks to directories)
                std::fs::remove_file(&path)?;
            }
            Ok(())
        })
        .await?
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::symlink_metadata(&path)?;
            Ok(VfsMetadata {
                permissions: Some(meta.mode()),
                uid: Some(meta.uid()),
                gid: Some(meta.gid()),
                atime: meta.accessed().ok(),
                mtime: meta.modified().ok(),
            })
        })
        .await?
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let path = path.to_path_buf();
        let meta = meta.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                if let Some(permissions) = meta.permissions {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
                }

                let uid = meta.uid.map(nix::unistd::Uid::from_raw);
                let gid = meta.gid.map(nix::unistd::Gid::from_raw);
                if uid.is_some() || gid.is_some() {
                    nix::unistd::chown(&path, uid, gid)?;
                }

                if meta.atime.is_some() || meta.mtime.is_some() {
                    let current_meta = std::fs::metadata(&path)?;
                    let atime = meta.atime.map_or_else(
                        || filetime::FileTime::from_last_access_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    let mtime = meta.mtime.map_or_else(
                        || filetime::FileTime::from_last_modification_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    filetime::set_file_times(&path, atime, mtime)?;
                }
            }
            Ok(())
        })
        .await?
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::rename(&from, &to).map_err(Error::Io)).await?
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let stats = nix::sys::statvfs::statvfs(&path)?;
            let frag = stats.fragment_size() as u64;
            Ok(VfsSpaceInfo {
                total_bytes: Some(stats.blocks() as u64 * frag),
                used_bytes: Some(
                    (stats.blocks() as u64).saturating_sub(stats.blocks_free() as u64) * frag,
                ),
                available_bytes: Some(stats.blocks_available() as u64 * frag),
            })
        })
        .await?
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        tokio::task::spawn_blocking(move || {
            // Try FICLONE (instant COW clone) first on Linux
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::io::AsRawFd;
                let src = std::fs::File::open(&from)?;
                let dst = std::fs::File::create(&to)?;
                let ret = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
                if ret == 0 {
                    return Ok(());
                }
                // FICLONE failed (unsupported FS), clean up and fall through to fs::copy
                drop(dst);
                let _ = std::fs::remove_file(&to);
            }

            // Fall back to kernel-level copy
            std::fs::copy(&from, &to)?;
            Ok(())
        })
        .await?
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let link = link.to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || std::fs::hard_link(&target, &link).map_err(Error::Io))
            .await?
    }
}
