//! Free-schema progress reporting for VFSes (and adjacent background
//! work). Producers — e.g. a SearchVfs walker — hold an
//! `Arc<dyn ProgressReporter>` scoped to *their own* VfsId at mount
//! time, and just call `report()` with a `VfsProgress` payload (or
//! `None` to clear). The mount manager closes over the VfsId, so the
//! producer never has to know it.
//!
//! The consumer side is the host: a `LocalProgressSink` writes into
//! `MainWindowState.vfs_progress` and triggers a publish, so the
//! frontend pane status bar sees the live values. In remote sessions
//! the producer is the agent and a `RemoteProgressSink` forwards
//! reports over the RPC notification channel
//! (`API_VFS_PROGRESS`) to the same `LocalProgressSink` on the host —
//! transparent to the producer.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::VfsId;
use crate::rpc::{Message, Outbox};

// ---------------------------------------------------------------------------
// Wire type
// ---------------------------------------------------------------------------

/// Free-schema progress snapshot. The frontend renders this as a short
/// inline status line — `"<stage> · <processed>/<total> · <extra>"` —
/// in the active pane's status bar.
#[derive(Clone, Debug, Default, Serialize, Deserialize, specta::Type)]
pub struct VfsProgress {
    /// Short verb-phrase like "Searching" or "Indexing".
    pub stage: String,
    /// Dominant counter — running count when `total` is `None`,
    /// determinate progress when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Sidecar values (e.g. `"hits" → "42"`). Surface as
    /// `" · key: value"` suffixes. The producer chooses keys; nothing
    /// in the pipeline parses them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// VFS-facing trait — already scoped to a specific VfsId by the manager
// ---------------------------------------------------------------------------

/// Push a progress update for the VFS the manager scoped this reporter
/// to. `Some(p)` posts; `None` clears (call this on completion or
/// cancellation). `report` is sync, non-blocking, and advisory —
/// implementations may drop reports under contention without affecting
/// correctness.
pub trait ProgressReporter: Send + Sync {
    fn report(&self, progress: Option<VfsProgress>);
}

// ---------------------------------------------------------------------------
// Sink trait — VfsId-aware, used by the manager + RPC dispatchers
// ---------------------------------------------------------------------------

/// The "consumer side" of progress. Implemented by whatever actually
/// gets the report into user-visible state — for the host that's a
/// publisher-backed map; for the agent that's an RPC forwarder. The
/// manager creates a per-VFS `ScopedReporter` that calls into this with
/// the right `vfs_id` baked in.
pub trait VfsProgressSink: Send + Sync {
    fn report(&self, vfs_id: VfsId, progress: Option<VfsProgress>);
}

/// Default no-op sink. Used as the placeholder when no real sink has
/// been plumbed (tests, isolated VFS construction, etc.).
pub struct NoopProgressSink;

impl VfsProgressSink for NoopProgressSink {
    fn report(&self, _vfs_id: VfsId, _progress: Option<VfsProgress>) {}
}

/// Per-VFS adapter. Holds an `Arc` to the underlying sink and the
/// `VfsId` the producer was assigned at mount time; from the
/// producer's perspective there's just `fn report(&self, …)`.
pub struct ScopedReporter {
    sink: Arc<dyn VfsProgressSink>,
    vfs_id: VfsId,
}

impl ScopedReporter {
    pub fn new(sink: Arc<dyn VfsProgressSink>, vfs_id: VfsId) -> Self {
        Self { sink, vfs_id }
    }
}

impl ProgressReporter for ScopedReporter {
    fn report(&self, progress: Option<VfsProgress>) {
        self.sink.report(self.vfs_id, progress);
    }
}

// ---------------------------------------------------------------------------
// Remote sink — agent side. Forwards reports as RPC notifications.
// ---------------------------------------------------------------------------

/// Agent-side sink. `report` enqueues onto an unbounded mpsc; a
/// background forwarder task drains it and emits
/// `Message::Notify(API_VFS_PROGRESS, ...)` via the supplied outbox.
/// Decoupling the producer (sync `report`) from RPC throughput keeps
/// `report` non-blocking; unbounded is fine because callers throttle
/// themselves (e.g. ≤5×/sec).
pub struct RemoteProgressSink {
    tx: mpsc::UnboundedSender<(VfsId, Option<VfsProgress>)>,
}

impl RemoteProgressSink {
    /// Spawn the forwarder task and return a sink that pushes into it.
    pub fn new(outbox: Outbox) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<(VfsId, Option<VfsProgress>)>();
        tokio::spawn(async move {
            while let Some((vfs_id, progress)) = rx.recv().await {
                let payload = match bincode::serialize(&(vfs_id, progress)) {
                    Ok(b) => b,
                    Err(e) => {
                        log::warn!("vfs progress: failed to encode report: {}", e);
                        continue;
                    }
                };
                if outbox
                    .send(Message::Notify(
                        crate::api::API_VFS_PROGRESS,
                        payload.into(),
                    ))
                    .await
                    .is_err()
                {
                    // RPC channel closed — agent is shutting down. Drain
                    // and stop.
                    break;
                }
            }
        });
        Self { tx }
    }
}

impl VfsProgressSink for RemoteProgressSink {
    fn report(&self, vfs_id: VfsId, progress: Option<VfsProgress>) {
        // Send-error here means the forwarder task is gone (agent
        // shutting down). Nothing to be done — drop the report.
        let _ = self.tx.send((vfs_id, progress));
    }
}
