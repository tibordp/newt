use std::os::unix::prelude::MetadataExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use std::sync::Arc;
use std::time::Duration;

use log::debug;

use log::warn;
use notify::event::RemoveKind;
use notify::Config;
use notify::Event;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use parking_lot::Mutex;

use crate::common::Error;
use crate::common::ToUnix;

#[derive(Clone, serde::Serialize)]
pub struct File {
    pub name: String,
    pub size: Option<u64>,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
    pub mode: String,
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

pub struct Local {}

impl Local {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl Filesystem for Local {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error> {
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

        // We need to watch all the parents in order to detect folder deletion
        let mut path = path;
        loop {
            watcher.watch(&path, RecursiveMode::NonRecursive)?;
            if !path.pop() {
                break;
            }
        }

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
                    size: None,
                    is_dir: true,
                    is_symlink: metadata.is_symlink(),
                    is_hidden: false,
                    mode: mode_string(mode),
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
                    size: (!is_dir).then_some(metadata.len()),
                    is_dir,
                    is_symlink: file_type.is_symlink(),
                    is_hidden: name.starts_with('.'),
                    mode: mode_string(mode),
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

pub struct Slow<T: Filesystem>(T);

impl<T: Filesystem> Slow<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }
}

#[async_trait::async_trait]
impl<T: Filesystem> Filesystem for Slow<T> {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error> {
        self.0.poll_changes(path).await
    }
    async fn list_files(&self, path: PathBuf) -> Result<FileList, Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.list_files(path).await
    }
    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.rename(old_path, new_path).await
    }
    async fn create_directory(&self, path: PathBuf) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.create_directory(path).await
    }
    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.delete_all(paths).await
    }
}


pub fn mode_string(mode: u32) -> String {
    const TYPE_CHARS: &[u8] = b"?pc?d?b?-?l?s???";
    const MODE_CHARS: &[u8] = b"rwxSTst";

    let mut ret = vec![0; 10];
    let mut idx = 0usize;

    ret[idx] = TYPE_CHARS[((mode >> 12) & 0xf) as usize];
    let mut i = 0;
    let mut m = 0o400;
    loop {
        let mut j = 0;
        let mut k = 0;

        loop {
            idx += 1;
            ret[idx] = b'-';
            if mode & m != 0 {
                ret[idx] = MODE_CHARS[j];
                k = j;
            }
            m >>= 1;
            j += 1;
            if j >= 3 {
                break;
            }
        }
        i += 1;

        if mode & (0o10000 >> i) != 0 {
            ret[idx] = MODE_CHARS[3 + (k & 2) + ((i == 3) as usize)];
        }

        if i >= 3 {
            break;
        }
    }

    unsafe { String::from_utf8_unchecked(ret) }
}
