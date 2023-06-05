use std::os::unix::prelude::MetadataExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use log::debug;
use log::info;
use log::warn;
use notify::Config;
use notify::Event;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use parking_lot::Mutex;
use tokio::sync::oneshot::Sender;

use crate::common::Error;
use crate::common::ToUnix;

#[derive(Clone, serde::Serialize)]
pub struct File {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
    pub mode: u32,
    pub modified: Option<i128>,
    pub accessed: Option<i128>,
    pub created: Option<i128>,
}

pub struct FileList {
    path: PathBuf,
    files: Vec<File>,
}

impl FileList {
    pub fn new(path: PathBuf, files: Vec<File>) -> Self {
        Self { path, files }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn files(&self) -> &[File] {
        &self.files
    }
}

/// Canonicalize . and .. segments in a path (without following symlinks or
/// checking whether they exists)
pub fn resolve(path: &Path) -> PathBuf {
    assert!(path.is_absolute());
    let mut ret = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                ret.pop();
            }
            component => ret.push(component.as_os_str()),
        }
    }
    ret
}

#[async_trait::async_trait]
pub trait Filesystem: Send + Sync {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error>;

    async fn list_files(&self, path: PathBuf) -> Result<FileList, Error>;
    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error>;
    async fn create_directory(&self, path: PathBuf) -> Result<(), Error>;
    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error>;
}

struct WatchDropGuard<'a>(usize, PathBuf, &'a Local);
impl Drop for WatchDropGuard<'_> {
    fn drop(&mut self) {
        let mut watcher = self.2.watcher.lock();
        let mut regs = self.2.registrations.lock();

        let mut needs_unwatch = true;
        regs.retain(|p| {
            if p.0 == self.0 {
                false
            } else {
                needs_unwatch = needs_unwatch && (p.1 != self.1);
                true
            }
        });
        if needs_unwatch {
            info!("unwatching {}", self.1.display());
            let _ = watcher.unwatch(&self.1);
        }
    }
}

pub struct Local {
    id: AtomicUsize,
    registrations: Arc<Mutex<Vec<(usize, PathBuf, Option<Sender<()>>)>>>,
    watcher: Mutex<RecommendedWatcher>,
}

impl Local {
    pub fn create() -> Result<Self, Error> {
        let registrations: Arc<Mutex<Vec<(usize, PathBuf, Option<Sender<()>>)>>> =
            Arc::new(Mutex::new(Vec::new()));

        let watcher = {
            let registrations = registrations.clone();
            RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    match res {
                        Ok(event) => {
                            let mut regs = registrations.lock();
                            for (id, watched_path, sender) in regs.iter_mut() {
                                if event.paths.iter().any(|p| p.starts_with(&watched_path)) {
                                    if let Some(sender) = sender.take() {
                                        debug!("notifying {}", id);
                                        let _ = sender.send(());
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("watch error: {:?}", e),
                    };
                },
                Config::default(),
            )?
        };

        Ok(Self {
            id: AtomicUsize::new(0),
            registrations,
            watcher: Mutex::new(watcher),
        })
    }
}

#[async_trait::async_trait]
impl Filesystem for Local {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error> {
        let id = self.id.fetch_add(1, Ordering::SeqCst);
        debug!("registering new watch for {}, id = {}", path.display(), id);

        let (tx, rx) = tokio::sync::oneshot::channel();

        {
            let mut watcher = self.watcher.lock();
            let mut regs = self.registrations.lock();
            let needs_watch = !regs.iter().any(|p| p.1 == path);
            if needs_watch {
                info!("watching {}", path.display());
                watcher.watch(&path, RecursiveMode::NonRecursive)?;
            }
            regs.push((id, path.clone(), Some(tx)));
        }

        let _guard = WatchDropGuard(id, path.clone(), self);
        let _ = rx.await;

        Ok(())
    }

    async fn list_files(&self, mut path: PathBuf) -> Result<FileList, Error> {
        fn reload(path: &Path) -> Result<Vec<File>, Error> {
            let mut ret = Vec::new();
            if let Some(parent) = path.parent() {
                let metadata = parent.symlink_metadata()?;

                #[cfg(target_family = "unix")]
                let mode = metadata.mode();
                #[cfg(target_family = "windows")]
                let mode = metadata.file_attributes() as _;

                ret.push(File {
                    name: "..".to_string(),
                    size: metadata.len(),
                    is_dir: true,
                    is_symlink: metadata.is_symlink(),
                    is_hidden: false,
                    mode,
                    modified: metadata.modified().map(|t| t.to_unix()).ok(),
                    accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                    created: metadata.created().map(|t| t.to_unix()).ok(),
                });
            }

            for maybe_entry in std::fs::read_dir(path)? {
                let entry = maybe_entry?;
                let metadata = entry.metadata()?;
                let file_type = metadata.file_type();

                let name = entry.file_name().into_string().unwrap();
                let mut is_dir = file_type.is_dir();

                if file_type.is_symlink() {
                    let target_metadata = std::fs::metadata(entry.path());
                    // If we e.g. don't have permission to read the target, we show the link details
                    if let Ok(target_metadata) = target_metadata {
                        is_dir = target_metadata.is_dir();
                    }
                }

                #[cfg(target_family = "unix")]
                let mode = metadata.mode();
                #[cfg(target_family = "windows")]
                let mode = metadata.file_attributes() as _;

                ret.push(File {
                    name: name.clone(),
                    size: metadata.len(),
                    is_dir,
                    is_symlink: file_type.is_symlink(),
                    is_hidden: name.starts_with('.'),
                    mode,
                    modified: metadata.modified().map(|t| t.to_unix()).ok(),
                    accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                    created: metadata.created().map(|t| t.to_unix()).ok(),
                });
            }

            Ok(ret)
        }

        assert!(path.is_absolute());
        loop {
            let path_1 = path.clone();
            match tauri::async_runtime::spawn_blocking(move || reload(&path_1)).await? {
                Ok(files) => return Ok(FileList::new(path, files)),
                Err(Error::Io(e)) => match e.kind() {
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
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

    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error> {
        tauri::async_runtime::spawn_blocking(move || {
            std::fs::rename(old_path, new_path).map_err(Error::Io)
        })
        .await?
    }

    async fn create_directory(&self, path: PathBuf) -> Result<(), Error> {
        tauri::async_runtime::spawn_blocking(move || {
            std::fs::create_dir_all(path).map_err(Error::Io)
        })
        .await?
    }

    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error> {
        tauri::async_runtime::spawn_blocking(move || {
            for path in paths {
                if path.is_dir() {
                    std::fs::remove_dir_all(path)?;
                } else {
                    std::fs::remove_file(path)?;
                }
            }
            Ok(())
        })
        .await?
    }
}
