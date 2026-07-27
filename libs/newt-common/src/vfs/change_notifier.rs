use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use super::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// VfsChangeNotifier — reusable self-notification for VFS implementations
// ---------------------------------------------------------------------------

type WatcherList = Vec<(u64, PathBuf, tokio::sync::oneshot::Sender<()>)>;

/// Allows VFS implementations to signal their own panes when they mutate
/// objects.  Call [`watch`] from `poll_changes` and [`notify`] after any
/// mutation.  Watchers whose prefix matches the modified path are signalled.
#[derive(Clone)]
pub struct VfsChangeNotifier {
    watchers: Arc<Mutex<WatcherList>>,
    next_id: Arc<AtomicU64>,
}

impl VfsChangeNotifier {
    pub fn new() -> Self {
        Self {
            watchers: Arc::new(Mutex::new(Vec::new())),
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Register a watcher for `path` and wait until a matching mutation is
    /// notified.  The watcher is automatically removed if the future is
    /// dropped (e.g. the pane navigates away).
    pub async fn watch(&self, path: &Path) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.watchers.lock().push((id, path.to_owned(), tx));
        let _guard = WatcherGuard {
            id,
            watchers: self.watchers.clone(),
        };
        let _ = rx.await;
    }

    /// Signal all watchers whose watched prefix is a parent of
    /// `modified_path`.
    pub fn notify(&self, modified_path: &Path) {
        let mut guard = self.watchers.lock();
        let old = std::mem::take(&mut *guard);
        for (id, prefix, sender) in old {
            if modified_path.starts_with(&prefix) {
                let _ = sender.send(());
            } else {
                guard.push((id, prefix, sender));
            }
        }
    }
}

impl Default for VfsChangeNotifier {
    fn default() -> Self {
        Self::new()
    }
}

struct WatcherGuard {
    id: u64,
    watchers: Arc<Mutex<WatcherList>>,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.watchers.lock().retain(|(id, _, _)| *id != self.id);
    }
}
