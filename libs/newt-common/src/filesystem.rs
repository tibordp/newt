use std::collections::HashMap;
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
use parking_lot::RwLock;

use crate::rpc::Communicator;
use crate::Error;
use crate::ToUnix;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ListFilesOptions {
    pub strict: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum UserGroup {
    Name(String),
    Id(u32),
}

impl PartialEq for UserGroup {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Name(a), Self::Name(b)) => a == b,
            (Self::Id(a), Self::Id(b)) => a == b,
            _ => false,
        }
    }
}

impl PartialOrd for UserGroup {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Name(a), Self::Name(b)) => a.partial_cmp(b),
            (Self::Id(a), Self::Id(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, Hash)]
pub struct Mode(u32);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct File {
    pub name: String,
    pub size: Option<u64>,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
    pub user: Option<UserGroup>,
    pub group: Option<UserGroup>,
    pub mode: Mode,
    pub modified: Option<i128>,
    pub accessed: Option<i128>,
    pub created: Option<i128>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FsStats {
    free_bytes: u64,
    available_bytes: u64,
    total_bytes: u64,
}

impl From<nix::sys::statvfs::Statvfs> for FsStats {
    fn from(stats: nix::sys::statvfs::Statvfs) -> Self {
        Self {
            free_bytes: (stats.blocks_available() as u64) * (stats.fragment_size() as u64),
            available_bytes: (stats.blocks_available() as u64) * (stats.fragment_size() as u64),
            total_bytes: (stats.blocks() as u64) * (stats.fragment_size() as u64),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct FileList {
    path: PathBuf,
    fs_stats: Option<FsStats>,
    files: Vec<File>,
}

impl FileList {
    pub fn new(path: PathBuf, files: Vec<File>, fs_stats: Option<FsStats>) -> Self {
        Self {
            path,
            files,
            fs_stats,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn files(&self) -> &[File] {
        &self.files
    }

    pub fn fs_stats(&self) -> Option<&FsStats> {
        self.fs_stats.as_ref()
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

struct UidGidCache {
    local_users: RwLock<HashMap<u32, UserGroup>>,
    local_groups: RwLock<HashMap<u32, UserGroup>>,
}

impl UidGidCache {
    pub fn new() -> Self {
        Self {
            local_users: RwLock::new(HashMap::new()),
            local_groups: RwLock::new(HashMap::new()),
        }
    }

    fn group_name(&self, gid: u32) -> Result<UserGroup, Error> {
        {
            let groups = self.local_groups.read();
            if let Some(group) = groups.get(&gid) {
                return Ok(group.clone());
            }
        }

        let group = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))?;
        let group = match group {
            Some(g) => UserGroup::Name(g.name),
            None => UserGroup::Id(gid),
        };

        let mut groups = self.local_groups.write();
        groups.insert(gid, group.clone());

        Ok(group)
    }

    fn user_name(&self, uid: u32) -> Result<UserGroup, Error> {
        {
            let users = self.local_users.read();
            if let Some(user) = users.get(&uid) {
                return Ok(user.clone());
            }
        }

        let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))?;
        let user = match user {
            Some(u) => UserGroup::Name(u.name),
            None => UserGroup::Id(uid),
        };

        let mut users = self.local_users.write();
        users.insert(uid, user.clone());

        Ok(user)
    }
}

#[async_trait::async_trait]
pub trait Filesystem: Send + Sync {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error>;
    async fn list_files(&self, path: PathBuf, options: ListFilesOptions)
        -> Result<FileList, Error>;
    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error>;
    async fn touch(&self, path: PathBuf) -> Result<(), Error>;
    async fn create_directory(&self, path: PathBuf) -> Result<(), Error>;
    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error>;
}

pub struct Local {
    cache: Arc<UidGidCache>,
}

impl Local {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(UidGidCache::new()),
        }
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

    async fn list_files(
        &self,
        mut path: PathBuf,
        options: ListFilesOptions,
    ) -> Result<FileList, Error> {
        assert!(path.is_absolute());
        loop {
            match tokio::task::spawn_blocking({
                let path = path.clone();
                let cache = self.cache.clone();
                move || -> Result<Vec<File>, Error> {
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
                            user: cache.user_name(metadata.uid()).ok(),
                            group: cache.group_name(metadata.gid()).ok(),
                            mode: Mode(mode),
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
                            user: cache.user_name(metadata.uid()).ok(),
                            group: cache.group_name(metadata.gid()).ok(),
                            mode: Mode(mode),
                            modified: metadata.modified().map(|t| t.to_unix()).ok(),
                            accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                            created: metadata.created().map(|t| t.to_unix()).ok(),
                        });
                    }

                    Ok(ret)
                }
            })
            .await?
            {
                Ok(files) => {
                    let stats = nix::sys::statvfs::statvfs(&path).ok().map(Into::into);

                    return Ok(FileList::new(path, files, stats));
                }
                Err(Error::Io(e)) => match (e.kind(), options.strict) {
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

    async fn touch(&self, path: PathBuf) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(path)
                .map_err(Error::Io)
        })
        .await?
        .map(|_| ())
    }

    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || std::fs::rename(old_path, new_path).map_err(Error::Io))
            .await?
    }

    async fn create_directory(&self, path: PathBuf) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(path).map_err(Error::Io))
            .await?
    }

    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error> {
        tokio::task::spawn_blocking(move || {
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
    async fn list_files(
        &self,
        path: PathBuf,
        options: ListFilesOptions,
    ) -> Result<FileList, Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.list_files(path, options).await
    }
    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.rename(old_path, new_path).await
    }
    async fn touch(&self, path: PathBuf) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.touch(path).await
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

pub struct Remote {
    communicator: Communicator,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl Filesystem for Remote {
    async fn poll_changes(&self, path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_POLL_CHANGES, &path)
            .await?;

        Ok(ret?)
    }
    async fn list_files(
        &self,
        path: PathBuf,
        options: ListFilesOptions,
    ) -> Result<FileList, Error> {
        let ret: Result<FileList, Error> = self
            .communicator
            .invoke(crate::api::API_LIST_FILES, &(path, options))
            .await?;

        Ok(ret?)
    }
    async fn rename(&self, old_path: PathBuf, new_path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_RENAME, &(old_path, new_path))
            .await?;

        Ok(ret?)
    }

    async fn touch(&self, path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_TOUCH, &path)
            .await?;

        Ok(ret?)
    }

    async fn create_directory(&self, path: PathBuf) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_CREATE_DIRECTORY, &path)
            .await?;

        Ok(ret?)
    }
    async fn delete_all(&self, paths: Vec<PathBuf>) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_DELETE_ALL, &paths)
            .await?;

        Ok(ret?)
    }
}

/// From busybox.
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
