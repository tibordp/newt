//! Shared plumbing for positioned-read handles over RPC. Both remoted read
//! surfaces — the VFS verbs (`API_VFS_OPEN_READ_AT`/…) and the Filesystem
//! verbs (`API_OPEN_READ_AT`/…) — speak the same open/read/close shape:
//! the server parks the reader in a session map keyed by `StreamId`, the
//! proxy is a `VfsRandomReader` that invokes per read and reaps the
//! session on drop.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::Error;
use crate::filesystem::StreamId;
use crate::rpc::{Api, Communicator};
use crate::vfs::VfsRandomReader;

/// The async mutex serializes reads per handle and keeps the reader alive
/// across a cancelled invoke (the aborted handler drops its guard, not the
/// reader).
type Session = Arc<tokio::sync::Mutex<Box<dyn VfsRandomReader>>>;

/// Server-held positioned-read handles.
pub(crate) struct ReadAtSessions {
    sessions: Mutex<HashMap<StreamId, Session>>,
    next_id: AtomicU64,
}

impl ReadAtSessions {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn open(&self, reader: Box<dyn VfsRandomReader>) -> StreamId {
        let id = StreamId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.sessions
            .lock()
            .insert(id, Arc::new(tokio::sync::Mutex::new(reader)));
        id
    }

    pub(crate) async fn read_at(
        &self,
        id: StreamId,
        offset: u64,
        len: u64,
    ) -> Result<serde_bytes::ByteBuf, Error> {
        // The proxy's `&mut self` makes read-after-close impossible; an
        // unknown id here is a broken protocol invariant.
        let reader = self
            .sessions
            .lock()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| panic!("READ_AT on unknown stream {:?}", id));
        let mut guard = reader.lock().await;
        guard
            .read_at(offset, len)
            .await
            .map(serde_bytes::ByteBuf::from)
    }

    /// A read still running against its Arc clone finishes harmlessly
    /// after this.
    pub(crate) fn close(&self, id: StreamId) {
        self.sessions.lock().remove(&id);
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, id: StreamId) -> bool {
        self.sessions.lock().contains_key(&id)
    }
}

/// Proxy side: `read_at` invokes per chunk; drop signals close.
pub(crate) struct RemoteRandomReader {
    pub(crate) stream_id: StreamId,
    pub(crate) communicator: Communicator,
    pub(crate) read_api: Api,
    pub(crate) close_api: Api,
}

impl Drop for RemoteRandomReader {
    fn drop(&mut self) {
        let _ = self.communicator.signal(self.close_api, &self.stream_id);
    }
}

#[async_trait::async_trait]
impl VfsRandomReader for RemoteRandomReader {
    async fn read_at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Error> {
        let ret: Result<serde_bytes::ByteBuf, Error> = self
            .communicator
            .invoke(self.read_api, &(self.stream_id, offset, len))
            .await?;
        Ok(ret?.into_vec())
    }
}
