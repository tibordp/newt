//! `Filesystem` implemented over the `VfsRegistry`: resolves each
//! `VfsPath` to its mounted VFS, follows synthetic-VFS redirects, and
//! adapts the streaming listing protocol.

use std::sync::Arc;

use log::debug;
use tokio::sync::mpsc;

use crate::Error;
use crate::filesystem::{Filesystem, ListFilesOptions};
use crate::vfs::properties::PropertySheet;

use super::path::VfsPath;
use super::{
    File, FileChunk, FileDetails, FileList, FsStats, RevalidationOutcome, SearchMatch,
    SearchPattern, VfsId, VfsRandomReader, VfsRegistry, find,
};

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
