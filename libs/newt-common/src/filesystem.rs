use std::collections::HashMap;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::rpc::Communicator;
use crate::vfs::VfsPath;
use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StreamId(pub u64);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ListFilesOptions {
    pub strict: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(
    Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, Hash,
)]
pub struct Mode(pub u32);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FsStats {
    free_bytes: u64,
    available_bytes: u64,
    total_bytes: u64,
}

impl From<nix::sys::statvfs::Statvfs> for FsStats {
    fn from(stats: nix::sys::statvfs::Statvfs) -> Self {
        Self {
            free_bytes: ((stats.blocks_available() as u64) * (stats.fragment_size() as u64)),
            available_bytes: ((stats.blocks_available() as u64) * (stats.fragment_size() as u64)),
            total_bytes: ((stats.blocks() as u64) * (stats.fragment_size() as u64)),
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FileList {
    path: VfsPath,
    fs_stats: Option<FsStats>,
    files: Vec<File>,
}

impl FileList {
    pub fn new(path: VfsPath, files: Vec<File>, fs_stats: Option<FsStats>) -> Self {
        Self {
            path,
            files,
            fs_stats,
        }
    }

    pub fn path(&self) -> &VfsPath {
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

/// Resolve a VfsPath by canonicalizing its path component.
pub fn resolve_vfs(vfs_path: &VfsPath) -> VfsPath {
    VfsPath {
        vfs_id: vfs_path.vfs_id,
        path: resolve(&vfs_path.path),
    }
}

pub struct UidGidCache {
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

    pub fn group_name(&self, gid: u32) -> Result<UserGroup, Error> {
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

    pub fn user_name(&self, uid: u32) -> Result<UserGroup, Error> {
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
    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error>;
    async fn list_files(&self, path: VfsPath, options: ListFilesOptions)
        -> Result<FileList, Error>;

    /// Like `list_files`, but sends intermediate batches via `batch_tx`.
    /// The returned `FileList` is always authoritative.
    async fn list_files_streaming(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
        _batch_tx: mpsc::UnboundedSender<Vec<File>>,
    ) -> Result<FileList, Error> {
        self.list_files(path, options).await
    }

    async fn rename(&self, old_path: VfsPath, new_path: VfsPath) -> Result<(), Error>;
    async fn touch(&self, path: VfsPath) -> Result<(), Error>;
    async fn create_directory(&self, path: VfsPath) -> Result<(), Error>;
    async fn delete_all(&self, paths: Vec<VfsPath>) -> Result<(), Error>;
}

pub struct Slow<T: Filesystem>(T);

impl<T: Filesystem> Slow<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }
}

#[async_trait::async_trait]
impl<T: Filesystem> Filesystem for Slow<T> {
    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error> {
        self.0.poll_changes(path).await
    }
    async fn list_files(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
    ) -> Result<FileList, Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.list_files(path, options).await
    }
    async fn list_files_streaming(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
        batch_tx: mpsc::UnboundedSender<Vec<File>>,
    ) -> Result<FileList, Error> {
        // Get the full listing from the inner filesystem, then drip-feed it
        // in batches of 100 with 500ms delays to simulate a slow connection.
        let file_list = self.0.list_files(path, options).await?;
        for chunk in file_list.files().chunks(100) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if batch_tx.send(chunk.to_vec()).is_err() {
                break;
            }
        }
        Ok(file_list)
    }
    async fn rename(&self, old_path: VfsPath, new_path: VfsPath) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.rename(old_path, new_path).await
    }
    async fn touch(&self, path: VfsPath) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.touch(path).await
    }
    async fn create_directory(&self, path: VfsPath) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.create_directory(path).await
    }
    async fn delete_all(&self, paths: Vec<VfsPath>) -> Result<(), Error> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.0.delete_all(paths).await
    }
}

pub type PendingStreams =
    Arc<parking_lot::Mutex<HashMap<StreamId, mpsc::UnboundedSender<Vec<File>>>>>;

pub struct Remote {
    communicator: Communicator,
    pending_streams: PendingStreams,
    next_stream_id: AtomicU64,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self {
            communicator,
            pending_streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            next_stream_id: AtomicU64::new(1),
        }
    }

    pub fn new_with_streams(communicator: Communicator, pending_streams: PendingStreams) -> Self {
        Self {
            communicator,
            pending_streams,
            next_stream_id: AtomicU64::new(1),
        }
    }

    pub fn pending_streams(&self) -> &PendingStreams {
        &self.pending_streams
    }
}

#[async_trait::async_trait]
impl Filesystem for Remote {
    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_POLL_CHANGES, &path)
            .await?;

        Ok(ret?)
    }
    async fn list_files(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
    ) -> Result<FileList, Error> {
        let ret: Result<FileList, Error> = self
            .communicator
            .invoke(crate::api::API_LIST_FILES, &(path, options))
            .await?;

        Ok(ret?)
    }

    async fn list_files_streaming(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
        batch_tx: mpsc::UnboundedSender<Vec<File>>,
    ) -> Result<FileList, Error> {
        let stream_id = StreamId(
            self.next_stream_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );

        // Register the batch sender so HostDispatcher can route Notify messages to it
        self.pending_streams.lock().insert(stream_id, batch_tx);

        // RAII guard to ensure cleanup even on cancellation/error
        struct StreamGuard {
            stream_id: StreamId,
            pending_streams: PendingStreams,
        }
        impl Drop for StreamGuard {
            fn drop(&mut self) {
                self.pending_streams.lock().remove(&self.stream_id);
            }
        }
        let _guard = StreamGuard {
            stream_id,
            pending_streams: self.pending_streams.clone(),
        };

        let ret: Result<FileList, Error> = self
            .communicator
            .invoke(
                crate::api::API_LIST_FILES_STREAMING,
                &(path, options, stream_id),
            )
            .await?;

        Ok(ret?)
    }
    async fn rename(&self, old_path: VfsPath, new_path: VfsPath) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_RENAME, &(old_path, new_path))
            .await?;

        Ok(ret?)
    }

    async fn touch(&self, path: VfsPath) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_TOUCH, &path)
            .await?;

        Ok(ret?)
    }

    async fn create_directory(&self, path: VfsPath) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_CREATE_DIRECTORY, &path)
            .await?;

        Ok(ret?)
    }
    async fn delete_all(&self, paths: Vec<VfsPath>) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_DELETE_ALL, &paths)
            .await?;

        Ok(ret?)
    }
}

// ---------------------------------------------------------------------------
// ShellService — shell expansion (separate from VFS/Filesystem)
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait ShellService: Send + Sync {
    async fn shell_expand(&self, input: String) -> Result<VfsPath, Error>;
}

pub struct LocalShellService;

#[async_trait::async_trait]
impl ShellService for LocalShellService {
    async fn shell_expand(&self, input: String) -> Result<VfsPath, Error> {
        let expanded = tokio::task::spawn_blocking(move || {
            expanduser::expanduser(input).map_err(Error::Io)
        })
        .await??;
        Ok(VfsPath::root(expanded))
    }
}

pub struct ShellRemote {
    communicator: Communicator,
}

impl ShellRemote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl ShellService for ShellRemote {
    async fn shell_expand(&self, input: String) -> Result<VfsPath, Error> {
        let ret: Result<VfsPath, Error> = self
            .communicator
            .invoke(crate::api::API_SHELL_EXPAND, &input)
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
