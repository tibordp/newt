use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use crate::vfs::path::{Path, PathBuf};

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
        /// Raw link target — flows straight into `File::symlink_target` /
        /// `FileDetails::symlink_target` (both `Option<String>`).
        target: String,
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
    /// Mimic object stores (S3): `read_range` starting at or past the
    /// file size is an error, not an empty chunk.
    pub strict_range_reads: bool,
    pub can_overwrite_sync: bool,
    pub can_overwrite_async: bool,
    pub can_create_directory: bool,
    pub can_create_symlink: bool,
    pub can_set_metadata: bool,
    pub can_remove: bool,
    pub can_remove_tree: bool,
    pub can_trash: bool,
    pub has_symlinks: bool,
    pub can_rename: bool,
    pub can_copy_within: bool,
}

impl Default for MockVfsConfig {
    fn default() -> Self {
        Self {
            can_read_sync: true,
            can_read_async: false,
            strict_range_reads: false,
            can_overwrite_sync: true,
            can_overwrite_async: false,
            can_create_directory: true,
            can_create_symlink: true,
            can_set_metadata: true,
            can_remove: true,
            can_remove_tree: true,
            can_trash: true,
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
    fn can_trash(&self) -> bool {
        self.config.can_trash
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
        crate::vfs::unix_display_path(path)
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
    /// Keyed by the canonical wire string of the in-VFS path
    /// (`Path::as_wire_str`), so lookups are independent of host OS
    /// separators and round-trip through the VFS path type.
    entries: Arc<Mutex<BTreeMap<String, MockEntry>>>,
    failures: Mutex<Vec<FailureSpec>>,
    descriptor: &'static dyn VfsDescriptor,
    strict_range_reads: bool,
    trashed: Mutex<Vec<String>>,
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
            if spec.path == *path && spec.operation == operation {
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
                (PathBuf::from_wire_str(p), kind)
            })
            .collect()
    }

    /// Read content of a file. Panics if not a file.
    pub fn read_content(&self, path: &str) -> Vec<u8> {
        let key = PathBuf::from_wire_str(path);
        match self.entries.lock().get(key.as_wire_str()) {
            Some(MockEntry::File { content, .. }) => content.clone(),
            other => panic!("read_content: {:?} is {:?}, not a file", key, other),
        }
    }

    /// Paths that were moved to the (simulated) trash, in call order.
    pub fn trashed_paths(&self) -> Vec<PathBuf> {
        self.trashed
            .lock()
            .iter()
            .map(|p| PathBuf::from_wire_str(p))
            .collect()
    }

    /// Check if path exists.
    pub fn exists(&self, path: &str) -> bool {
        self.entries
            .lock()
            .contains_key(PathBuf::from_wire_str(path).as_wire_str())
    }

    /// Get mode of a file/directory.
    pub fn get_mode(&self, path: &str) -> Option<u32> {
        match self
            .entries
            .lock()
            .get(PathBuf::from_wire_str(path).as_wire_str())
        {
            Some(MockEntry::File { mode, .. }) | Some(MockEntry::Directory { mode, .. }) => {
                Some(*mode)
            }
            _ => None,
        }
    }

    /// Get uid of a file/directory.
    pub fn get_uid(&self, path: &str) -> Option<u32> {
        match self
            .entries
            .lock()
            .get(PathBuf::from_wire_str(path).as_wire_str())
        {
            Some(MockEntry::File { uid, .. }) | Some(MockEntry::Directory { uid, .. }) => {
                Some(*uid)
            }
            _ => None,
        }
    }

    /// Get gid of a file/directory.
    pub fn get_gid(&self, path: &str) -> Option<u32> {
        match self
            .entries
            .lock()
            .get(PathBuf::from_wire_str(path).as_wire_str())
        {
            Some(MockEntry::File { gid, .. }) | Some(MockEntry::Directory { gid, .. }) => {
                Some(*gid)
            }
            _ => None,
        }
    }

    fn list_children(&self, parent: &Path) -> Vec<File> {
        let entries = self.entries.lock();
        let mut children = Vec::new();
        for (path_str, entry) in entries.iter() {
            let path = PathBuf::from_wire_str(path_str);
            let parent_matches = path.parent().map(Path::as_wire_str) == Some(parent.as_wire_str());
            if parent_matches && path.as_wire_str() != parent.as_wire_str() {
                let name = path.file_name().unwrap().to_string();
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
                    attributes: None,
                    name,
                    size,
                    allocated_size: None,
                    device_id: None,
                    inode: None,
                    hard_links: None,
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
                    key: None,
                    source: None,
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
    ) -> Result<crate::vfs::VfsFileList, crate::Error> {
        if let Some(e) = self.check_failure(path, "list_files") {
            return Err(e);
        }
        // Verify directory exists
        match self.entries.lock().get(path.as_wire_str()) {
            Some(MockEntry::Directory { .. }) => {}
            None if path.is_root() => {} // root always exists implicitly
            _ => {
                return Err(crate::Error {
                    kind: crate::ErrorKind::NotFound,
                    message: format!("directory not found: {}", path),
                });
            }
        }
        Ok(self.list_children(path).into())
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
        match self.entries.lock().get(path.as_wire_str()) {
            Some(MockEntry::File { content, .. }) => Ok(Box::new(Cursor::new(content.clone()))),
            _ => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("file not found: {}", path),
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
        match self.entries.lock().get(path.as_wire_str()) {
            Some(MockEntry::File { content, .. }) => {
                Ok(Box::new(std::io::Cursor::new(content.clone())))
            }
            _ => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("file not found: {}", path),
            }),
        }
    }

    async fn read_range(
        &self,
        path: &Path,
        offset: u64,
        length: u64,
    ) -> Result<crate::file_reader::FileChunk, crate::Error> {
        if let Some(e) = self.check_failure(path, "read_range") {
            return Err(e);
        }
        match self.entries.lock().get(path.as_wire_str()) {
            Some(MockEntry::File { content, .. }) => {
                if self.strict_range_reads && offset >= content.len() as u64 {
                    return Err(crate::Error::custom(format!(
                        "range start {} is beyond object size {}",
                        offset,
                        content.len()
                    )));
                }
                let start = (offset as usize).min(content.len());
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
                message: format!("file not found: {}", path),
            }),
        }
    }

    async fn file_details(
        &self,
        path: &Path,
    ) -> Result<crate::file_reader::FileDetails, crate::Error> {
        match self.entries.lock().get(path.as_wire_str()) {
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
                message: format!("not found: {}", path),
            }),
        }
    }

    async fn file_info(&self, path: &Path) -> Result<File, crate::Error> {
        if let Some(e) = self.check_failure(path, "file_info") {
            return Err(e);
        }
        let name = path.file_name().unwrap_or_default().to_string();
        match self.entries.lock().get(path.as_wire_str()) {
            Some(MockEntry::File {
                content,
                mode,
                uid,
                gid,
            }) => Ok(File {
                attributes: None,
                name,
                size: Some(content.len() as u64),
                allocated_size: None,
                device_id: None,
                inode: None,
                hard_links: None,
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
                key: None,
                source: None,
            }),
            Some(MockEntry::Directory { mode, uid, gid }) => Ok(File {
                attributes: None,
                name,
                size: None,
                allocated_size: None,
                device_id: None,
                inode: None,
                hard_links: None,
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
                key: None,
                source: None,
            }),
            Some(MockEntry::Symlink { target }) => Ok(File {
                attributes: None,
                name,
                size: None,
                allocated_size: None,
                device_id: None,
                inode: None,
                hard_links: None,
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
                key: None,
                source: None,
            }),
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path),
            }),
        }
    }

    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, crate::Error> {
        if let Some(e) = self.check_failure(path, "overwrite_sync") {
            return Err(e);
        }
        Ok(Box::new(MockWriter {
            buf: Vec::new(),
            path: path.as_wire_str().to_string(),
            entries: self.entries.clone(),
        }))
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, crate::Error> {
        if let Some(e) = self.check_failure(path, "overwrite_async") {
            return Err(e);
        }
        Ok(Box::new(MockWriter {
            buf: Vec::new(),
            path: path.as_wire_str().to_string(),
            entries: self.entries.clone(),
        }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "create_directory") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        if entries.contains_key(path.as_wire_str()) {
            return Err(crate::Error {
                kind: crate::ErrorKind::AlreadyExists,
                message: format!("already exists: {}", path),
            });
        }
        entries.insert(
            path.as_wire_str().to_string(),
            MockEntry::Directory {
                mode: 0o755,
                uid: 1000,
                gid: 1000,
            },
        );
        Ok(())
    }

    async fn create_symlink(&self, link: &Path, target: &str) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(link, "create_symlink") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        entries.insert(
            link.as_wire_str().to_string(),
            MockEntry::Symlink {
                target: target.to_string(),
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
        match entries.get(path.as_wire_str()) {
            Some(MockEntry::File { .. }) | Some(MockEntry::Symlink { .. }) => {
                entries.remove(path.as_wire_str());
                Ok(())
            }
            Some(MockEntry::Directory { .. }) => Err(crate::Error {
                kind: crate::ErrorKind::IsADirectory,
                message: format!("is a directory: {}", path),
            }),
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path),
            }),
        }
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "remove_dir") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        // Check that it's a directory
        match entries.get(path.as_wire_str()) {
            Some(MockEntry::Directory { .. }) => {}
            _ => {
                return Err(crate::Error {
                    kind: crate::ErrorKind::NotADirectory,
                    message: format!("not a directory: {}", path),
                });
            }
        }
        // Check not empty
        let has_children = entries.keys().any(|k| {
            let kp = PathBuf::from_wire_str(k);
            kp.as_wire_str() != path.as_wire_str()
                && kp.parent().map(Path::as_wire_str) == Some(path.as_wire_str())
        });
        if has_children {
            return Err(crate::Error {
                kind: crate::ErrorKind::DirectoryNotEmpty,
                message: format!("directory not empty: {}", path),
            });
        }
        entries.remove(path.as_wire_str());
        Ok(())
    }

    async fn remove_tree(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "remove_tree") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        let to_remove: Vec<String> = entries
            .keys()
            .filter(|k| PathBuf::from_wire_str(k).starts_with(path))
            .cloned()
            .collect();
        if to_remove.is_empty() {
            return Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path),
            });
        }
        for k in to_remove {
            entries.remove(&k);
        }
        Ok(())
    }

    async fn trash_item(&self, path: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "trash_item") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        let to_remove: Vec<String> = entries
            .keys()
            .filter(|k| PathBuf::from_wire_str(k).starts_with(path))
            .cloned()
            .collect();
        if to_remove.is_empty() {
            return Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", path),
            });
        }
        for k in to_remove {
            entries.remove(&k);
        }
        self.trashed.lock().push(path.as_wire_str().to_string());
        Ok(())
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, crate::Error> {
        match self.entries.lock().get(path.as_wire_str()) {
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
                message: format!("not found: {}", path),
            }),
        }
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(path, "set_metadata") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        match entries.get_mut(path.as_wire_str()) {
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
                message: format!("not found: {}", path),
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
        let to_move: Vec<(String, MockEntry)> = entries
            .iter()
            .filter(|(k, _)| PathBuf::from_wire_str(k).starts_with(from))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if to_move.is_empty() {
            return Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", from),
            });
        }
        for (old_key, entry) in &to_move {
            entries.remove(old_key);
            let old_path = PathBuf::from_wire_str(old_key);
            let suffix = old_path.strip_prefix(from).unwrap();
            // Re-root the (possibly empty) suffix under `to`, component-wise
            // so separators stay canonical.
            let mut new_path = to.to_owned();
            for seg in PathBuf::from_wire_str(suffix).components() {
                new_path.push(seg);
            }
            entries.insert(new_path.as_wire_str().to_string(), entry.clone());
        }
        Ok(())
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), crate::Error> {
        if let Some(e) = self.check_failure(from, "copy_within") {
            return Err(e);
        }
        let mut entries = self.entries.lock();
        match entries.get(from.as_wire_str()).cloned() {
            Some(entry) => {
                entries.insert(to.as_wire_str().to_string(), entry);
                Ok(())
            }
            None => Err(crate::Error {
                kind: crate::ErrorKind::NotFound,
                message: format!("not found: {}", from),
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

type EntryMap = Arc<Mutex<BTreeMap<String, MockEntry>>>;

struct MockWriter {
    buf: Vec<u8>,
    /// Canonical wire-string key of the target path.
    path: String,
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
    entries: BTreeMap<String, MockEntry>,
    failures: Vec<FailureSpec>,
    config: MockVfsConfig,
}

impl MockVfsBuilder {
    pub fn new() -> Self {
        let mut entries = BTreeMap::new();
        entries.insert(
            PathBuf::root().as_wire_str().to_string(),
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

    pub fn dir(mut self, path: &str) -> Self {
        let path = PathBuf::from_wire_str(path);
        self.ensure_parents(&path);
        self.entries.insert(
            path.as_wire_str().to_string(),
            MockEntry::Directory {
                mode: 0o755,
                uid: 1000,
                gid: 1000,
            },
        );
        self
    }

    pub fn dir_with_mode(mut self, path: &str, mode: u32) -> Self {
        let path = PathBuf::from_wire_str(path);
        self.ensure_parents(&path);
        self.entries.insert(
            path.as_wire_str().to_string(),
            MockEntry::Directory {
                mode,
                uid: 1000,
                gid: 1000,
            },
        );
        self
    }

    pub fn dir_with_owner(mut self, path: &str, mode: u32, uid: u32, gid: u32) -> Self {
        let path = PathBuf::from_wire_str(path);
        self.ensure_parents(&path);
        self.entries.insert(
            path.as_wire_str().to_string(),
            MockEntry::Directory { mode, uid, gid },
        );
        self
    }

    pub fn file(mut self, path: &str, content: &[u8]) -> Self {
        let path = PathBuf::from_wire_str(path);
        self.ensure_parents(&path);
        self.entries.insert(
            path.as_wire_str().to_string(),
            MockEntry::File {
                content: content.to_vec(),
                mode: 0o644,
                uid: 1000,
                gid: 1000,
            },
        );
        self
    }

    pub fn file_with_mode(mut self, path: &str, content: &[u8], mode: u32) -> Self {
        let path = PathBuf::from_wire_str(path);
        self.ensure_parents(&path);
        self.entries.insert(
            path.as_wire_str().to_string(),
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
        path: &str,
        content: &[u8],
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Self {
        let path = PathBuf::from_wire_str(path);
        self.ensure_parents(&path);
        self.entries.insert(
            path.as_wire_str().to_string(),
            MockEntry::File {
                content: content.to_vec(),
                mode,
                uid,
                gid,
            },
        );
        self
    }

    pub fn symlink(mut self, link: &str, target: &str) -> Self {
        let link = PathBuf::from_wire_str(link);
        self.ensure_parents(&link);
        self.entries.insert(
            link.as_wire_str().to_string(),
            MockEntry::Symlink {
                target: target.to_string(),
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
            if p.is_root() || self.entries.contains_key(p.as_wire_str()) {
                break;
            }
            self.entries.insert(
                p.as_wire_str().to_string(),
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
        let strict_range_reads = self.config.strict_range_reads;
        let descriptor: &'static dyn VfsDescriptor = Box::leak(Box::new(MockVfsDescriptor {
            config: self.config,
        }));

        Arc::new(MockVfs {
            entries: Arc::new(Mutex::new(self.entries)),
            failures: Mutex::new(self.failures),
            descriptor,
            strict_range_reads,
            trashed: Mutex::new(Vec::new()),
        })
    }
}
