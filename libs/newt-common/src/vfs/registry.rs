//! Session-wide VFS bookkeeping: the id → mounted-VFS map, plus the
//! inventory-collected descriptor registry.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use log::info;
use parking_lot::RwLock;

use crate::Error;

use super::path::{PathBuf, VfsPath};
use super::{Vfs, VfsDescriptor, VfsId};

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
