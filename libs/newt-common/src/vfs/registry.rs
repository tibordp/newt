//! Session-wide VFS bookkeeping: the id → mounted-VFS map with the
//! inventory-collected descriptor registry, and `VfsRegistryFs` — the
//! `Filesystem` implementation that resolves each `VfsPath` through the
//! map, follows synthetic-VFS redirects, and adapts the streaming
//! listing protocol.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use log::{debug, info};
use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::Error;
use crate::filesystem::{Filesystem, ListFilesOptions};
use crate::find::{self, SearchMatch, SearchPattern};
use crate::vfs::properties::PropertySheet;

use super::path::{PathBuf, VfsPath};
use super::{
    File, FileChunk, FileDetails, FileList, FsStats, RevalidationOutcome, Vfs, VfsDescriptor,
    VfsId, VfsRandomReader,
};

// Auto-registration via inventory
pub struct RegisteredDescriptor(pub &'static dyn VfsDescriptor);
inventory::collect!(RegisteredDescriptor);

pub fn lookup_descriptor(type_name: &str) -> Option<&'static dyn VfsDescriptor> {
    inventory::iter::<RegisteredDescriptor>()
        .find(|r| r.0.type_name() == type_name)
        .map(|r| r.0)
}

pub fn all_descriptors() -> impl Iterator<Item = &'static dyn VfsDescriptor> {
    inventory::iter::<RegisteredDescriptor>().map(|r| r.0)
}

// ---------------------------------------------------------------------------
// VfsRegistry
// ---------------------------------------------------------------------------

pub struct VfsRegistry {
    vfs_map: RwLock<HashMap<VfsId, Arc<dyn Vfs>>>,
    next_id: AtomicU32,
}

impl VfsRegistry {
    pub fn with_root(root: Arc<dyn Vfs>) -> Self {
        let mut map = HashMap::new();
        map.insert(VfsId::ROOT, root);
        Self {
            vfs_map: RwLock::new(map),
            next_id: AtomicU32::new(1),
        }
    }

    pub fn get(&self, id: VfsId) -> Option<Arc<dyn Vfs>> {
        self.vfs_map.read().get(&id).cloned()
    }

    pub fn resolve(&self, vfs_path: &VfsPath) -> Result<(Arc<dyn Vfs>, PathBuf), Error> {
        let vfs = self
            .get(vfs_path.vfs_id)
            .ok_or_else(|| Error::custom(format!("VFS {} not found", vfs_path.vfs_id)))?;
        Ok((vfs, vfs_path.path.clone()))
    }

    /// Follow `Vfs::redirect_target` once: if the VFS at `vfs_path.vfs_id`
    /// reports a redirect for `vfs_path`, return the source path; else
    /// return the input unchanged. Used by `VfsRegistryFs` to make leaf
    /// operations transparent across synthetic VFSes (flat search
    /// results, etc.).
    pub async fn dereference(&self, vfs_path: &VfsPath) -> VfsPath {
        let Some(vfs) = self.get(vfs_path.vfs_id) else {
            return vfs_path.clone();
        };
        match vfs.redirect_target(&vfs_path.path).await {
            Some(target) => target,
            None => vfs_path.clone(),
        }
    }

    /// Reserve a fresh `VfsId` without inserting anything. Used by the
    /// manager when a VFS needs to know its id at construction time —
    /// allocate first, hand the id to the VFS (via a scoped progress
    /// reporter etc.), then `insert`.
    pub fn allocate_id(&self) -> VfsId {
        VfsId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Insert a freshly-constructed VFS under a previously-allocated id.
    /// Panics if the id is already taken (programmer error — allocate
    /// always returns a fresh id).
    pub fn insert(&self, id: VfsId, vfs: Arc<dyn Vfs>) {
        info!("vfs: mount id={} type={}", id, vfs.descriptor().type_name());
        let prev = self.vfs_map.write().insert(id, vfs);
        assert!(prev.is_none(), "vfs_id {} already taken", id);
    }

    /// Convenience: allocate + insert in one shot. Use when the VFS
    /// doesn't need to know its id at construction.
    pub fn mount(&self, vfs: Arc<dyn Vfs>) -> VfsId {
        let id = self.allocate_id();
        self.insert(id, vfs);
        id
    }

    pub fn unmount(&self, id: VfsId) -> Option<Arc<dyn Vfs>> {
        if id == VfsId::ROOT {
            return None; // refuse to unmount ROOT
        }
        info!("vfs: unmount id={}", id);
        self.vfs_map.write().remove(&id)
    }
}

// ---------------------------------------------------------------------------
// VfsRegistryFs — implements Filesystem by dispatching through VfsRegistry
// ---------------------------------------------------------------------------

pub struct VfsRegistryFs {
    registry: Arc<VfsRegistry>,
}

impl VfsRegistryFs {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl Filesystem for VfsRegistryFs {
    async fn poll_changes(&self, path: VfsPath) -> Result<(), Error> {
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.poll_changes(&local_path).await
    }

    async fn list_files(
        &self,
        path: VfsPath,
        options: ListFilesOptions,
        batch_tx: Option<mpsc::Sender<FileList>>,
    ) -> Result<FileList, Error> {
        let vfs = self
            .registry
            .get(path.vfs_id)
            .ok_or_else(|| Error::custom(format!("VFS {} not found", path.vfs_id)))?;
        let mut current = path;
        loop {
            let fs_stats = if vfs.descriptor().can_fs_stats() {
                vfs.fs_stats(&current.path).await.unwrap_or(None)
            } else {
                None
            };

            let result = if let Some(ref outer_tx) = batch_tx {
                let (tx, mut rx) =
                    mpsc::channel::<Vec<File>>(crate::filesystem::LIST_BATCH_CHANNEL_CAPACITY);
                let outer_tx = outer_tx.clone();
                let vfs_path = current.clone();
                let fs_stats = fs_stats.clone();
                let list = vfs.list_files(&current.path, Some(tx));
                tokio::pin!(list);
                let result = loop {
                    tokio::select! {
                        result = &mut list => break result,
                        files = rx.recv() => {
                            let Some(files) = files else {
                                break (&mut list).await;
                            };
                            let batch = FileList::new(
                                vfs_path.clone(),
                                files,
                                fs_stats.clone(),
                            );
                            if outer_tx.send(batch).await.is_err() {
                                return Err(Error::cancelled());
                            }
                        }
                    }
                };
                while let Some(files) = rx.recv().await {
                    let batch = FileList::new(vfs_path.clone(), files, fs_stats.clone());
                    if outer_tx.send(batch).await.is_err() {
                        return Err(Error::cancelled());
                    }
                }
                result
            } else {
                vfs.list_files(&current.path, None).await
            };

            match result {
                Ok(result) => {
                    return Ok(
                        FileList::new(current, result.files, fs_stats).with_partial(result.partial)
                    );
                }
                Err(e)
                    if matches!(
                        (e.kind, options.strict),
                        (crate::ErrorKind::NotFound, false) | (crate::ErrorKind::NotADirectory, _)
                    ) =>
                {
                    if !current.path.pop() {
                        return Err(e);
                    }
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
    }

    async fn fs_stats(&self, path: VfsPath) -> Result<Option<FsStats>, Error> {
        let vfs = self
            .registry
            .get(path.vfs_id)
            .ok_or_else(|| Error::custom(format!("VFS {} not found", path.vfs_id)))?;
        if !vfs.descriptor().can_fs_stats() {
            return Ok(None);
        }
        vfs.fs_stats(&path.path).await
    }

    async fn touch(&self, path: VfsPath) -> Result<(), Error> {
        debug!("vfs_registry_fs: touch {}", path);
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.touch(&local_path).await
    }

    async fn create_directory(&self, path: VfsPath) -> Result<(), Error> {
        debug!("vfs_registry_fs: create_directory {}", path);
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.create_directory(&local_path).await
    }

    async fn revalidate(&self, vfs_id: VfsId) -> Result<RevalidationOutcome, Error> {
        let vfs = self
            .registry
            .get(vfs_id)
            .ok_or_else(|| Error::custom(format!("unknown VFS id: {}", vfs_id)))?;
        // Mirror the descriptor capability gate: if the VFS doesn't claim
        // to support revalidation, treat it as a no-op rather than dispatching
        // and getting a `not_supported` back. This is the host-local short-
        // circuit; remote callers gate on the descriptor *before* the RPC.
        if !vfs.descriptor().can_revalidate() {
            return Ok(RevalidationOutcome::Fresh);
        }
        vfs.revalidate().await
    }

    async fn file_details(&self, path: VfsPath) -> Result<FileDetails, Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.file_details(&local_path).await
    }

    async fn get_property_sheet(&self, path: VfsPath) -> Result<PropertySheet, Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.get_property_sheet(&local_path).await
    }

    async fn read_range(
        &self,
        path: VfsPath,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.read_range(&local_path, offset, length).await
    }

    async fn open_read_at(&self, path: VfsPath) -> Result<Box<dyn VfsRandomReader>, Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        vfs.open_read_at(&local_path).await
    }

    async fn read_file(&self, path: VfsPath, max_size: u64) -> Result<Vec<u8>, Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        let details = vfs.file_details(&local_path).await?;
        if details.size > max_size {
            return Err(Error::custom(format!(
                "File is too large to edit ({} bytes, limit is {} bytes)",
                details.size, max_size
            )));
        }
        use tokio::io::AsyncReadExt;
        let mut reader = vfs.open_read_async(&local_path).await?;
        let mut data = Vec::with_capacity(details.size as usize);
        reader.read_to_end(&mut data).await?;
        Ok(data)
    }

    async fn write_file(&self, path: VfsPath, data: Vec<u8>) -> Result<(), Error> {
        let path = self.registry.dereference(&path).await;
        let (vfs, local_path) = self.registry.resolve(&path)?;
        let mut writer = vfs.overwrite_async(&local_path).await?;
        writer.write(&data).await?;
        writer.finish().await?;
        Ok(())
    }

    async fn find_in_file(
        &self,
        path: VfsPath,
        offset: u64,
        pattern: SearchPattern,
        max_length: u64,
    ) -> Result<Option<SearchMatch>, Error> {
        let compiled = find::compile_regex(&pattern)?;
        let overlap = find::compute_overlap(&pattern);
        let mut carry: Vec<u8> = Vec::new();
        let mut pos = offset;
        let end = offset.saturating_add(max_length);

        let mut reader = self.open_read_at(path).await?;
        while pos < end {
            let chunk_len = std::cmp::min(find::SEARCH_CHUNK_SIZE as u64, end - pos);
            let data = reader.read_at(pos, chunk_len).await?;
            if data.is_empty() {
                break;
            }

            let carry_len = carry.len();
            carry.extend_from_slice(&data);

            if let Some((match_pos, match_len)) =
                find::find_in_buffer(&carry, &pattern, compiled.as_ref())
            {
                let abs_offset = pos - carry_len as u64 + match_pos as u64;
                return Ok(Some(SearchMatch {
                    offset: abs_offset,
                    length: match_len as u64,
                }));
            }

            pos += data.len() as u64;

            // Keep overlap bytes for next iteration
            if carry.len() > overlap {
                let start = carry.len() - overlap;
                carry.drain(..start);
            }

            if data.len() < chunk_len as usize {
                break; // EOF
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::vfs::s3::S3VfsDescriptor;
    use crate::vfs::{VfsDescriptor, VfsId, VfsPath, VfsRegistry};

    use super::super::path::Path;

    // Minimal mock Vfs for registry tests
    struct DummyVfs;

    #[async_trait::async_trait]
    impl crate::vfs::Vfs for DummyVfs {
        fn descriptor(&self) -> &'static dyn VfsDescriptor {
            &S3VfsDescriptor // reuse; descriptor type doesn't matter for registry tests
        }
        async fn list_files(
            &self,
            _path: &Path,
            _batch_tx: Option<tokio::sync::mpsc::Sender<Vec<crate::vfs::File>>>,
        ) -> Result<crate::vfs::VfsFileList, crate::Error> {
            Ok(crate::vfs::VfsFileList::default())
        }
        async fn poll_changes(&self, _path: &Path) -> Result<(), crate::Error> {
            Ok(())
        }
        async fn fs_stats(
            &self,
            _path: &Path,
        ) -> Result<Option<crate::vfs::FsStats>, crate::Error> {
            Ok(None)
        }
    }

    #[test]
    fn registry_mount_returns_incrementing_ids() {
        let registry = VfsRegistry::with_root(Arc::new(DummyVfs));

        let id1 = registry.mount(Arc::new(DummyVfs));
        let id2 = registry.mount(Arc::new(DummyVfs));
        let id3 = registry.mount(Arc::new(DummyVfs));

        assert_eq!(id1, VfsId(1));
        assert_eq!(id2, VfsId(2));
        assert_eq!(id3, VfsId(3));
    }

    #[test]
    fn registry_get_returns_mounted_vfs() {
        let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
        assert!(registry.get(VfsId::ROOT).is_some());
        assert!(registry.get(VfsId(99)).is_none());

        let id = registry.mount(Arc::new(DummyVfs));
        assert!(registry.get(id).is_some());
    }

    #[test]
    fn registry_unmount_removes_vfs() {
        let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
        let id = registry.mount(Arc::new(DummyVfs));
        assert!(registry.get(id).is_some());

        registry.unmount(id);
        assert!(registry.get(id).is_none());
    }

    #[test]
    fn registry_cannot_unmount_root() {
        let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
        let result = registry.unmount(VfsId::ROOT);
        assert!(result.is_none()); // refused
        assert!(registry.get(VfsId::ROOT).is_some()); // still there
    }

    #[test]
    fn registry_resolve_returns_error_for_missing_vfs() {
        let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
        let result = registry.resolve(&VfsPath::root(VfsId(999)));
        assert!(result.is_err());
    }
}
