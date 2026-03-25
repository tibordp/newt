use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::filesystem::{File, Mode};
use crate::vfs::{
    Breadcrumb, DisplayPathMatch, MountRequest, Vfs, VfsAsyncWriter, VfsDescriptor, VfsMetadata,
    VfsSpaceInfo,
};

// ---------------------------------------------------------------------------
// Mock entry model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum MockEntry {
    File {
        content: Vec<u8>,
        mode: u32,
        uid: u32,
        gid: u32,
    },
    Directory {
        mode: u32,
        uid: u32,
        gid: u32,
    },
    Symlink {
        target: PathBuf,
    },
}

// ---------------------------------------------------------------------------
// Failure injection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FailureSpec {
    pub path: PathBuf,
    pub operation: &'static str, // e.g. "remove_file", "overwrite_sync", ...
    pub error: crate::Error,
    pub remaining: Option<u32>, // None = permanent, Some(n) = fail n times then succeed
}

// ---------------------------------------------------------------------------
// Config: which capabilities the mock VFS advertises
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MockVfsConfig {
    pub can_read_sync: bool,
    pub can_read_async: bool,
    pub can_overwrite_sync: bool,
    pub can_overwrite_async: bool,
    pub can_create_directory: bool,
    pub can_create_symlink: bool,
    pub can_set_metadata: bool,
    pub can_remove: bool,
    pub can_remove_tree: bool,
    pub has_symlinks: bool,
    pub can_rename: bool,
    pub can_copy_within: bool,
}

impl Default for MockVfsConfig {
    fn default() -> Self {
        Self {
            can_read_sync: true,
            can_read_async: false,
            can_overwrite_sync: true,
            can_overwrite_async: false,
            can_create_directory: true,
            can_create_symlink: true,
            can_set_metadata: true,
            can_remove: true,
            can_remove_tree: true,
            has_symlinks: true,
            can_rename: true,
            can_copy_within: false,
        }
    }
}

// ---------------------------------------------------------------------------
// MockVfsDescriptor (leaked for 'static lifetime in tests)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct MockVfsDescriptor {
    config: MockVfsConfig,
}

impl VfsDescriptor for MockVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "mock"
    }
    fn display_name(&self) -> &'static str {
        "Mock"
    }
    fn auto_mount_request(&self) -> Option<MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        false
    }
    fn can_read_sync(&self) -> bool {
        self.config.can_read_sync
    }
    fn can_read_async(&self) -> bool {
        self.config.can_read_async
    }
    fn can_overwrite_sync(&self) -> bool {
        self.config.can_overwrite_sync
    }
    fn can_overwrite_async(&self) -> bool {
        self.config.can_overwrite_async
    }
    fn can_create_directory(&self) -> bool {
        self.config.can_create_directory
    }
    fn can_create_symlink(&self) -> bool {
        self.config.can_create_symlink
    }
    fn can_touch(&self) -> bool {
        false
    }
    fn can_truncate(&self) -> bool {
        false
    }
    fn can_set_metadata(&self) -> bool {
        self.config.can_set_metadata
    }
    fn can_remove(&self) -> bool {
        self.config.can_remove
    }
    fn can_remove_tree(&self) -> bool {
        self.config.can_remove_tree
    }
    fn has_symlinks(&self) -> bool {
        self.config.has_symlinks
    }
    fn can_stat_directories(&self) -> bool {
        true
    }
    fn can_fs_stats(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        self.config.can_rename
    }
    fn can_copy_within(&self) -> bool {
        self.config.can_copy_within
    }
    fn can_hard_link(&self) -> bool {
        false
    }
    fn format_path(&self, path: &Path, _mount_meta: &[u8]) -> String {
        path.display().to_string()
    }
    fn breadcrumbs(&self, _path: &Path, _mount_meta: &[u8]) -> Vec<Breadcrumb> {
        Vec::new()
    }
    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        None
    }
}

// ---------------------------------------------------------------------------
// MockVfs
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct MockVfs {
    entries: Arc<Mutex<BTreeMap<PathBuf, MockEntry>>>,
    failures: Mutex<Vec<FailureSpec>>,
    descriptor: &'static dyn VfsDescriptor,
}

impl MockVfs {
    pub fn builder() -> MockVfsBuilder {
        MockVfsBuilder::new()
    }

    /// Check if a failure should fire for the given path+operation.
    /// Decrements remaining count; removes exhausted specs.
    fn check_failure(&self, path: &Path, operation: &str) -> Option<crate::Error> {
        let mut failures = self.failures.lock();
        for spec in failures.iter_mut() {
            if spec.path == path && spec.operation == operation {
                match &mut spec.remaining {
                    None => return Some(spec.error.clone()),
                    Some(0) => continue,
                    Some(n) => {
                        *n -= 1;
                        return Some(spec.error.clone());
                    }
                }
            }
        }
        None
    }

    // -- State inspection helpers --

    /// Snapshot of all paths and their types, sorted.
    pub fn snapshot(&self) -> Vec<(PathBuf, &'static str)> {
        self.entries
            .lock()
            .iter()
            .map(|(p, e)| {
                let kind = match e {
                    MockEntry::File { .. } => "file",
                    MockEntry::Directory { .. } => "dir",
                    MockEntry::Symlink { .. } => "symlink",
                };
                (p.clone(), kind)
            })
            .collect()
    }

    /// Read content of a file. Panics if not a file.
    pub fn read_content(&self, path: impl AsRef<Path>) -> Vec<u8> {
        match self.entries.lock().get(path.as_ref()) {
            Some(MockEntry::File { content, .. }) => content.clone(),
            other => panic!(
                "read_content: {:?} is {:?}, not a file",
                path.as_ref(),
                other
            ),
        }
    }

    /// Check if path exists.
    pub fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.entries.lock().contains_key(path.as_ref())
    }

    /// Get mode of a file/directory.
    pub fn get_mode(&self, path: impl AsRef<Path>) -> Option<u32> {
        match self.entries.lock().get(path.as_ref()) {
            Some(MockEntry::File { mode, .. }) | Some(MockEntry::Directory { mode, .. }) => {
                Some(*mode)
            }
            _ => None,
        }
    }

    /// Get uid of a file/directory.
    pub fn get_uid(&self, path: impl AsRef<Path>) -> Option<u32> {
        match self.entries.lock().get(path.as_ref()) {
            Some(MockEntry::File { uid, .. }) | Some(MockEntry::Directory { uid, .. }) => {
                Some(*uid)
            }
            _ => None,
        }
    }

    /// Get gid of a file/directory.
    pub fn get_gid(&self, path: impl AsRef<Path>) -> Option<u32> {
        match self.entries.lock().get(path.as_ref()) {
            Some(MockEntry::File { gid, .. }) | Some(MockEntry::Directory { gid, .. }) => {
                Some(*gid)
            }
            _ => None,
        }
    }

    fn list_children(&self, parent: &Path) -> Vec<File> {
        let entries = self.entries.lock();
        let mut children = Vec::new();
        for (path, entry) in entries.iter() {
            if path.parent() == Some(parent) && path != parent {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                let (is_dir, is_symlink, symlink_target, size, mode, user, group) = match entry {
                    MockEntry::File {
                        content,
                        mode,
                        uid,
                        gid,
                    } => (
                        false,
                        false,
                        None,
                        Some(content.len() as u64),
                        *mode,
                        Some(crate::filesystem::UserGroup::Id(*uid)),
                        Some(crate::filesystem::UserGroup::Id(*gid)),
                    ),
                    MockEntry::Directory { mode, uid, gid } => (
                        true,
                        false,
                        None,
                        None,
                        *mode,
                        Some(crate::filesystem::UserGroup::Id(*uid)),
                        Some(crate::filesystem::UserGroup::Id(*gid)),
                    ),
                    MockEntry::Symlink { target } => {
                        (false, true, Some(target.clone()), None, 0o777, None, None)
                    }
                };
                children.push(File {
                    name,
                    size,
                    is_dir,
                    is_hidden: false,
                    is_symlink,
                    symlink_target,
                    user,
                    group,
                    mode: Some(Mode(mode)),
                    modified: None,
                    accessed: None,
                    created: None,
                });
            }
        }
        children
    }
}

// ---------------------------------------------------------------------------
// Vfs trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Vfs for MockVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        self.descriptor
    }

    async fn list_files(
        &self,
        path: &Path,
        _batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<Vec<File>, crate::Error> {
        if let Some(e) = self.check_failure(path, "list_files") {
            return Err(e);
        }
        // Verify directory exists
        match self.entries.lock().get(path) {
            Some(MockEntry::Directory { .. }) => {}
            None if path == Path::new("/") => {} // root always exists implicitly
            _ => {
                return Err(crate::Error {
                    kind: crate::ErrorKind::NotFound,
                    message: format!("directory not found: {}", path.display()),
                });
            }
        }
        Ok(self.list_children(path))
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), crate::Error> {
        // Never returns in real usage; for tests just pend forever
        std::future::pending().await
    }

    async fn fs_stats(
        &self,
        _path: &Path,
    ) -> Result<Option<crate::filesystem::FsStats>, crate::Error> {
        Ok(None)
    }

    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, crate::Error> {
        if let Some(e) = self.check_failure(path, "open_read_sync") {
            return Err(e);
        }
        match self.entries.lock().get(path) {
            Some(MockEntry::File { content, .. }) => Ok(Box::new(Cursor::new(content.clone()))),
            _ => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("file not found: {}", path.display()),
            }),
        }
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin>, crate::Error> {
        if let Some(e) = self.check_failure(path, "open_read_async") {
            return Err(e);
        }
        match self.entries.lock().get(path) {
            Some(MockEntry::File { content, .. }) => {
                Ok(Box::new(std::io::Cursor::new(content.clone())))
            }
            _ => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("file not found: {}", path.display()),
            }),
        }
    }

    async fn read_range(
        &self,
        path: &Path,
        offset: u64,
        length: u64,
    ) -> Result<crate::file_reader::FileChunk, crate::Error> {
        match self.entries.lock().get(path) {
            Some(MockEntry::File { content, .. }) => {
                let start = offset as usize;
                let end = (offset + length) as usize;
                let data = content[start..end.min(content.len())].to_vec();
                Ok(crate::file_reader::FileChunk {
                    data,
                    offset,
                    total_size: content.len() as u64,
                })
            }
            _ => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("file not found: {}", path.display()),
            }),
        }
    }

    async fn file_details(
        &self,
        path: &Path,
    ) -> Result<crate::file_reader::FileDetails, crate::Error> {
        match self.entries.lock().get(path) {
            Some(MockEntry::File {
                content,
                mode,
                uid,
                gid,
            }) => Ok(crate::file_reader::FileDetails {
                size: content.len() as u64,
                mime_type: None,
                is_dir: false,
                is_symlink: false,
                symlink_target: None,
                user: Some(crate::filesystem::UserGroup::Id(*uid)),
                group: Some(crate::filesystem::UserGroup::Id(*gid)),
                mode: Some(Mode(*mode)),
                modified: None,
                accessed: None,
                created: None,
            }),
            Some(MockEntry::Directory { mode, uid, gid }) => Ok(crate::file_reader::FileDetails {
                size: 0,
                mime_type: None,
                is_dir: true,
                is_symlink: false,
                symlink_target: None,
                user: Some(crate::filesystem::UserGroup::Id(*uid)),
                group: Some(crate::filesystem::UserGroup::Id(*gid)),
                mode: Some(Mode(*mode)),
                modified: None,
                accessed: None,
                created: None,
            }),
            Some(MockEntry::Symlink { target }) => Ok(crate::file_reader::FileDetails {
                size: 0,
                mime_type: None,
                is_dir: false,
                is_symlink: true,
                symlink_target: Some(target.clone()),
                user: None,
                group: None,
                mode: None,
                modified: None,
                accessed: None,
                created: None,
            }),
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path.display()),
            }),
        }
    }

    async fn file_info(&self, path: &Path) -> Result<File, crate::Error> {
        if let Some(e) = self.check_failure(path, "file_info") {
            return Err(e);
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match self.entries.lock().get(path) {
            Some(MockEntry::File {
                content,
                mode,
                uid,
                gid,
            }) => Ok(File {
                name,
                size: Some(content.len() as u64),
                is_dir: false,
                is_hidden: false,
                is_symlink: false,
                symlink_target: None,
                user: Some(crate::filesystem::UserGroup::Id(*uid)),
                group: Some(crate::filesystem::UserGroup::Id(*gid)),
                mode: Some(Mode(*mode)),
                modified: None,
                accessed: None,
                created: None,
            }),
            Some(MockEntry::Directory { mode, uid, gid }) => Ok(File {
                name,
                size: None,
                is_dir: true,
                is_hidden: false,
                is_symlink: false,
                symlink_target: None,
                user: Some(crate::filesystem::UserGroup::Id(*uid)),
                group: Some(crate::filesystem::UserGroup::Id(*gid)),
                mode: Some(Mode(*mode)),
                modified: None,
                accessed: None,
                created: None,
            }),
            Some(MockEntry::Symlink { target }) => Ok(File {
                name,
                size: None,
                is_dir: false,
                is_hidden: false,
                is_symlink: true,
                symlink_target: Some(target.clone()),
                user: None,
                group: None,
                mode: Some(Mode(0o777)),
                modified: None,
                accessed: None,
                created: None,
            }),
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path.display()),
            }),
        }
    }

    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, crate::Error> {
        if let Some(e) = self.check_failure(path, "overwrite_sync") {
            return Err(e);
        }
        Ok(Box::new(MockWriter {
            buf: Vec::new(),
            path: path.to_path_buf(),
            entries: self.entries.clone(),
        }))
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, crate::Error> {
        if let Some(e) = self.check_failure(path, "overwrite_async") {
            return Err(e);
        }
        Ok(Box::new(MockWriter {
            buf: Vec::new(),
            path: path.to_path_buf(),
            entries: self.entries.clone(),
        }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "create_directory") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        if entries.contains_key(path) {
            return Err(crate::Error {
                kind: crate::ErrorKind::AlreadyExists,
                message: format!("already exists: {}", path.display()),
            });
        }
        entries.insert(
            path.to_path_buf(),
            MockEntry::Directory {
                mode: 0o755,
                uid: 1000,
                gid: 1000,
            },
        );
        Ok(())
    }

    async fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(link, "create_symlink") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        entries.insert(
            link.to_path_buf(),
            MockEntry::Symlink {
                target: target.to_path_buf(),
            },
        );
        Ok(())
    }

    async fn touch(&self, _path: &Path) -> Result<(), crate::Error> {
        Err(crate::Error::not_supported())
    }

    async fn truncate(&self, _path: &Path) -> Result<(), crate::Error> {
        Err(crate::Error::not_supported())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "remove_file") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        match entries.get(path) {
            Some(MockEntry::File { .. }) | Some(MockEntry::Symlink { .. }) => {
                entries.remove(path);
                Ok(())
            }
            Some(MockEntry::Directory { .. }) => Err(crate::Error {
                kind: crate::ErrorKind::IsADirectory,
                message: format!("is a directory: {}", path.display()),
            }),
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path.display()),
            }),
        }
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "remove_dir") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        // Check that it's a directory
        match entries.get(path) {
            Some(MockEntry::Directory { .. }) => {}
            _ => {
                return Err(crate::Error {
                    kind: crate::ErrorKind::NotADirectory,
                    message: format!("not a directory: {}", path.display()),
                });
            }
        }
        // Check not empty
        let has_children = entries
            .keys()
            .any(|k| k != path && k.parent() == Some(path));
        if has_children {
            return Err(crate::Error {
                kind: crate::ErrorKind::DirectoryNotEmpty,
                message: format!("directory not empty: {}", path.display()),
            });
        }
        entries.remove(path);
        Ok(())
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "remove_tree") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        let to_remove: Vec<PathBuf> = entries
            .keys()
            .filter(|k| k.starts_with(path))
            .cloned()
            .collect();
        if to_remove.is_empty() {
            return Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path.display()),
            });
        }
        for k in to_remove {
            entries.remove(&k);
        }
        Ok(())
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, crate::Error> {
        match self.entries.lock().get(path) {
            Some(MockEntry::File { mode, uid, gid, .. })
            | Some(MockEntry::Directory { mode, uid, gid, .. }) => Ok(VfsMetadata {
                permissions: Some(*mode),
                uid: Some(*uid),
                gid: Some(*gid),
                ..Default::default()
            }),
            Some(MockEntry::Symlink { .. }) => Ok(VfsMetadata::default()),
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path.display()),
            }),
        }
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "set_metadata") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        match entries.get_mut(path) {
            Some(MockEntry::File { mode, uid, gid, .. })
            | Some(MockEntry::Directory { mode, uid, gid, .. }) => {
                if let Some(p) = meta.permissions {
                    *mode = p;
                }
                if let Some(u) = meta.uid {
                    *uid = u;
                }
                if let Some(g) = meta.gid {
                    *gid = g;
                }
                Ok(())
            }
            Some(MockEntry::Symlink { .. }) => Ok(()),
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path.display()),
            }),
        }
    }

    async fn available_space(&self, _path: &Path) -> Result<VfsSpaceInfo, crate::Error> {
        Ok(VfsSpaceInfo {
            total_bytes: None,
            used_bytes: None,
            available_bytes: None,
        })
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(from, "rename") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        // Collect all entries under `from` (including `from` itself)
        let to_move: Vec<(PathBuf, MockEntry)> = entries
            .iter()
            .filter(|(k, _)| k.starts_with(from))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if to_move.is_empty() {
            return Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", from.display()),
            });
        }
        for (old_path, entry) in &to_move {
            entries.remove(old_path);
            let suffix = old_path.strip_prefix(from).unwrap();
            let new_path = to.join(suffix);
            entries.insert(new_path, entry.clone());
        }
        Ok(())
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(from, "copy_within") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        match entries.get(from).cloned() {
            Some(entry) => {
                entries.insert(to.to_path_buf(), entry);
                Ok(())
            }
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", from.display()),
            }),
        }
    }

    async fn hard_link(&self, _link: &Path, _target: &Path) -> Result<(), crate::Error> {
        Err(crate::Error::not_supported())
    }
}

// ---------------------------------------------------------------------------
// Writer: captures bytes, writes to entries map on drop (sync) or finish (async)
// ---------------------------------------------------------------------------

type EntryMap = Arc<Mutex<BTreeMap<PathBuf, MockEntry>>>;

struct MockWriter {
    buf: Vec<u8>,
    path: PathBuf,
    entries: EntryMap,
}

impl MockWriter {
    fn commit(&mut self) {
        let content = std::mem::take(&mut self.buf);
        self.entries.lock().insert(
            self.path.clone(),
            MockEntry::File {
                content,
                mode: 0o644,
                uid: 1000,
                gid: 1000,
            },
        );
    }
}

impl Write for MockWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for MockWriter {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            self.commit();
        }
    }
}

#[async_trait]
impl VfsAsyncWriter for MockWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, crate::Error> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), crate::Error> {
        self.commit();
        // Clear buf so Drop doesn't double-insert
        self.buf.clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub struct MockVfsBuilder {
    entries: BTreeMap<PathBuf, MockEntry>,
    failures: Vec<FailureSpec>,
    config: MockVfsConfig,
}

impl MockVfsBuilder {
    pub fn new() -> Self {
        let mut entries = BTreeMap::new();
        entries.insert(
            PathBuf::from("/"),
            MockEntry::Directory {
                mode: 0o755,
                uid: 1000,
                gid: 1000,
            },
        );
        Self {
            entries,
            failures: Vec::new(),
            config: MockVfsConfig::default(),
        }
    }

    pub fn config(mut self, config: MockVfsConfig) -> Self {
        self.config = config;
        self
    }

    pub fn dir(mut self, path: impl AsRef<Path>) -> Self {
        self.ensure_parents(path.as_ref());
        self.entries.insert(
            path.as_ref().to_path_buf(),
            MockEntry::Directory {
                mode: 0o755,
                uid: 1000,
                gid: 1000,
            },
        );
        self
    }

    pub fn dir_with_mode(mut self, path: impl AsRef<Path>, mode: u32) -> Self {
        self.ensure_parents(path.as_ref());
        self.entries.insert(
            path.as_ref().to_path_buf(),
            MockEntry::Directory {
                mode,
                uid: 1000,
                gid: 1000,
            },
        );
        self
    }

    pub fn dir_with_owner(mut self, path: impl AsRef<Path>, mode: u32, uid: u32, gid: u32) -> Self {
        self.ensure_parents(path.as_ref());
        self.entries.insert(
            path.as_ref().to_path_buf(),
            MockEntry::Directory { mode, uid, gid },
        );
        self
    }

    pub fn file(mut self, path: impl AsRef<Path>, content: &[u8]) -> Self {
        self.ensure_parents(path.as_ref());
        self.entries.insert(
            path.as_ref().to_path_buf(),
            MockEntry::File {
                content: content.to_vec(),
                mode: 0o644,
                uid: 1000,
                gid: 1000,
            },
        );
        self
    }

    pub fn file_with_mode(mut self, path: impl AsRef<Path>, content: &[u8], mode: u32) -> Self {
        self.ensure_parents(path.as_ref());
        self.entries.insert(
            path.as_ref().to_path_buf(),
            MockEntry::File {
                content: content.to_vec(),
                mode,
                uid: 1000,
                gid: 1000,
            },
        );
        self
    }

    pub fn file_with_owner(
        mut self,
        path: impl AsRef<Path>,
        content: &[u8],
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Self {
        self.ensure_parents(path.as_ref());
        self.entries.insert(
            path.as_ref().to_path_buf(),
            MockEntry::File {
                content: content.to_vec(),
                mode,
                uid,
                gid,
            },
        );
        self
    }

    pub fn symlink(mut self, link: impl AsRef<Path>, target: impl AsRef<Path>) -> Self {
        self.ensure_parents(link.as_ref());
        self.entries.insert(
            link.as_ref().to_path_buf(),
            MockEntry::Symlink {
                target: target.as_ref().to_path_buf(),
            },
        );
        self
    }

    pub fn failure(mut self, spec: FailureSpec) -> Self {
        self.failures.push(spec);
        self
    }

    fn ensure_parents(&mut self, path: &Path) {
        let mut current = path.parent();
        while let Some(p) = current {
            if p == Path::new("/") || self.entries.contains_key(p) {
                break;
            }
            self.entries.insert(
                p.to_path_buf(),
                MockEntry::Directory {
                    mode: 0o755,
                    uid: 1000,
                    gid: 1000,
                },
            );
            current = p.parent();
        }
    }

    pub fn build(self) -> Arc<MockVfs> {
        let descriptor: &'static dyn VfsDescriptor = Box::leak(Box::new(MockVfsDescriptor {
            config: self.config,
        }));

        Arc::new(MockVfs {
            entries: Arc::new(Mutex::new(self.entries)),
            failures: Mutex::new(self.failures),
            descriptor,
        })
    }
}
