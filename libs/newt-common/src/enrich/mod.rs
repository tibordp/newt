//! Enrichers — background annotation of directory listings.
//!
//! An enricher computes extra information about the entries of a
//! directory (git status per file, recursive directory sizes, …) and
//! streams it out as [`Annotation`]s keyed by entry key, plus
//! per-location [`ContextBadge`]s (branch indicator, directory total).
//! Results overlay the pane's listing on the host; they are anchored to
//! the pane's history cursor and never persisted.
//!
//! The subsystem is symmetric across the host↔agent boundary, following
//! the operations template: an [`Enrichers`] registry lives next to the
//! `VfsRegistry` (host-side in local sessions, agent-side in remote
//! ones), fronted by an [`EnricherClient`] with [`Local`] and [`Remote`]
//! impls. Remote streams events via `Notify(API_ENRICHMENT_EVENT)`
//! correlated by [`EnrichmentId`], mirroring streaming `list_files`.
//!
//! Cancellation is by drop: dropping the `enrich` future (directly in
//! local mode, via transport-level `InvokeCancel` in remote mode) must
//! stop all work. Enrichers therefore never detach tasks — child
//! processes use `kill_on_drop`, concurrency uses in-future combinators.

pub mod git;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::Error;
use crate::rpc::Communicator;
use crate::vfs::{VfsPath, VfsRegistry};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Correlation id for a streaming enrichment request, allocated by the
/// requesting side (mirrors `StreamId` for listings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnrichmentId(pub u64);

/// Per-entry annotation payload. Open taxonomy — one variant per
/// annotation kind. The pane treats these as opaque (a generic
/// per-entry overlay merged into `FileView.annotations`); only the
/// frontend interprets the kinds it knows how to render.
///
/// Default (external) enum tagging only: this crosses the bincode RPC
/// boundary, and bincode cannot deserialize `tag`/`content`/`untagged`
/// representations. JSON shape is `{"git": "modified"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum Annotation {
    Git(GitEntryStatus),
}

/// Git working-tree status of a listed entry. For directories this is a
/// rollup of everything beneath them (VSCode-style), with precedence
/// `Conflicted > Modified > Renamed > Added > Untracked`; `Ignored` is
/// only ever direct (a fully-ignored entry), never rolled up.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum GitEntryStatus {
    Ignored,
    Untracked,
    Added,
    Renamed,
    Modified,
    Conflicted,
}

/// Per-location badge (pane header / status bar), replace-by-kind per
/// enricher. External tagging for the same bincode reason as
/// [`Annotation`]; JSON shape is `{"git_branch": {...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ContextBadge {
    GitBranch {
        /// Branch name, or short commit id when detached.
        name: String,
        detached: bool,
        /// Ahead/behind upstream; both 0 when there is no upstream.
        ahead: u64,
        behind: u64,
        /// Whether the repository has any uncommitted changes at all
        /// (repo-wide, not just the listed directory).
        dirty: bool,
    },
}

/// Which entries a request is about. Automatic enrichment covers the
/// whole listing; manual triggers (future du keybinds) name entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnrichScope {
    AllEntries,
    Entries(Vec<String>),
}

/// One flush of an enricher's pending results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentBatch {
    pub enricher: String,
    /// First batch of a run: the consumer drops this enricher's previous
    /// annotations/badges before applying, so a rerun supersedes the
    /// prior generation wholesale (entries that stopped matching don't
    /// linger). Always sent at least once per run, even empty.
    pub reset: bool,
    /// `(entry_key, annotation)` — replace-by-key within the enricher.
    pub entries: Vec<(String, Annotation)>,
    pub badges: Vec<ContextBadge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnrichmentEvent {
    /// An applicable enricher began work; `activity` is the status-bar
    /// label to show while it runs.
    Started {
        enricher: String,
        activity: String,
    },
    Batch(EnrichmentBatch),
    Finished {
        enricher: String,
    },
}

// ---------------------------------------------------------------------------
// Sink — buffers emits, flushes as throttled batches
// ---------------------------------------------------------------------------

/// Producer-side batching at the same cadence as streaming listings and
/// operation progress.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Buffering sink handed to a running enricher. `emit_*` are sync and
/// non-blocking; batches go out on [`maybe_flush`](Self::maybe_flush)
/// (rate-limited) and [`finish`](Self::finish). Burst-style enrichers
/// (git) can just emit everything and rely on the final flush;
/// streaming ones (du) call `maybe_flush` as they go.
pub struct EnrichSink {
    enricher: &'static str,
    tx: mpsc::Sender<EnrichmentEvent>,
    state: Mutex<SinkState>,
}

struct SinkState {
    entries: HashMap<String, Annotation>,
    badges: Vec<ContextBadge>,
    last_flush: Instant,
    flushed_once: bool,
}

impl EnrichSink {
    fn new(enricher: &'static str, tx: mpsc::Sender<EnrichmentEvent>) -> Self {
        Self {
            enricher,
            tx,
            state: Mutex::new(SinkState {
                entries: HashMap::new(),
                badges: Vec::new(),
                last_flush: Instant::now(),
                flushed_once: false,
            }),
        }
    }

    /// Annotate `key` (replaces any pending or previously-flushed
    /// annotation for the same key from this enricher).
    pub fn emit_entry(&self, key: String, annotation: Annotation) {
        self.state.lock().entries.insert(key, annotation);
    }

    /// Emit a context badge (replaces any previous badge of the same
    /// kind from this enricher).
    pub fn emit_badge(&self, badge: ContextBadge) {
        let mut state = self.state.lock();
        state
            .badges
            .retain(|b| std::mem::discriminant(b) != std::mem::discriminant(&badge));
        state.badges.push(badge);
    }

    /// Flush pending results if the throttle window has elapsed.
    pub async fn maybe_flush(&self) {
        let batch = {
            let mut state = self.state.lock();
            if state.last_flush.elapsed() < FLUSH_INTERVAL
                || (state.entries.is_empty() && state.badges.is_empty())
            {
                return;
            }
            self.take_batch(&mut state)
        };
        let _ = self.tx.send(EnrichmentEvent::Batch(batch)).await;
    }

    /// Flush whatever is pending. Always sends at least one batch per
    /// run so the `reset` semantics clear the previous generation even
    /// when the run produced nothing (e.g. a repo that became clean).
    async fn finish(&self) {
        let batch = {
            let mut state = self.state.lock();
            if state.flushed_once && state.entries.is_empty() && state.badges.is_empty() {
                return;
            }
            self.take_batch(&mut state)
        };
        let _ = self.tx.send(EnrichmentEvent::Batch(batch)).await;
    }

    fn take_batch(&self, state: &mut SinkState) -> EnrichmentBatch {
        let batch = EnrichmentBatch {
            enricher: self.enricher.to_string(),
            reset: !state.flushed_once,
            entries: state.entries.drain().collect(),
            badges: std::mem::take(&mut state.badges),
        };
        state.flushed_once = true;
        state.last_flush = Instant::now();
        batch
    }
}

// ---------------------------------------------------------------------------
// Enricher descriptor + trait + registry
// ---------------------------------------------------------------------------

/// Static, side-independent metadata for an enricher — the analogue of
/// `VfsDescriptor`. Host and agent link the same set (inventory-
/// collected), so the requesting side decides which enrichers to run
/// for a pane (descriptor gate × preferences) without a round-trip and
/// names them explicitly in the request. Data-dependent applicability
/// (is this directory inside a repo?) is the enricher's own business
/// inside [`Enricher::enrich`].
pub trait EnricherDescriptor: Send + Sync {
    fn id(&self) -> &'static str;
    /// Status-bar label shown while this enricher runs.
    fn activity(&self) -> &'static str;
    /// Automatic enrichers run on every navigation/refresh; manual ones
    /// only via explicit triggers (future du keybinds).
    fn automatic(&self) -> bool;
    /// Static gate: can this enricher run on a VFS of this type at all
    /// (e.g. git only on the real local filesystem)?
    fn applies_to_vfs(&self, vfs: &dyn crate::vfs::VfsDescriptor) -> bool;
}

pub struct RegisteredEnricher(pub &'static dyn EnricherDescriptor);
inventory::collect!(RegisteredEnricher);

pub fn all_enricher_descriptors() -> impl Iterator<Item = &'static dyn EnricherDescriptor> {
    inventory::iter::<RegisteredEnricher>().map(|r| r.0)
}

#[async_trait::async_trait]
pub trait Enricher: Send + Sync {
    fn descriptor(&self) -> &'static dyn EnricherDescriptor;
    /// Compute and push results into `sink`. Cancellation is by drop —
    /// implementations must not detach work that outlives this future.
    async fn enrich(
        &self,
        registry: &VfsRegistry,
        path: &VfsPath,
        scope: &EnrichScope,
        sink: &EnrichSink,
    ) -> Result<(), Error>;
}

/// Registry of enrichers, instantiated next to the `VfsRegistry` on
/// whichever side owns the filesystem.
pub struct Enrichers {
    registry: Arc<VfsRegistry>,
    enrichers: Vec<Arc<dyn Enricher>>,
}

impl Enrichers {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self {
            registry,
            enrichers: Vec::new(),
        }
    }

    pub fn with(mut self, enricher: Arc<dyn Enricher>) -> Self {
        self.enrichers.push(enricher);
        self
    }

    /// Run the named enrichers for `path`, streaming events into `tx`.
    /// The requesting side selects ids host-side (descriptor gate ×
    /// preferences); unknown ids are skipped. Returns when every
    /// requested enricher has finished. Cancellation is by dropping the
    /// returned future.
    pub async fn enrich(
        &self,
        path: VfsPath,
        scope: EnrichScope,
        enrichers: Vec<String>,
        tx: mpsc::Sender<EnrichmentEvent>,
    ) -> Result<(), Error> {
        for id in &enrichers {
            let Some(enricher) = self.enrichers.iter().find(|e| e.descriptor().id() == *id) else {
                log::debug!("enrich: unknown enricher {:?} requested", id);
                continue;
            };
            let descriptor = enricher.descriptor();
            let _ = tx
                .send(EnrichmentEvent::Started {
                    enricher: descriptor.id().to_string(),
                    activity: descriptor.activity().to_string(),
                })
                .await;
            let sink = EnrichSink::new(descriptor.id(), tx.clone());
            if let Err(e) = enricher.enrich(&self.registry, &path, &scope, &sink).await {
                log::debug!("enricher {} failed for {}: {}", descriptor.id(), path, e);
            }
            sink.finish().await;
            let _ = tx
                .send(EnrichmentEvent::Finished {
                    enricher: descriptor.id().to_string(),
                })
                .await;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// EnricherClient — Local / Remote
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait EnricherClient: Send + Sync {
    /// Stream enrichment events for `path` into `tx`; resolves when the
    /// request completes. Cancel by dropping the future (transport-level
    /// `InvokeCancel` aborts the agent-side run in remote sessions).
    async fn enrich(
        &self,
        path: VfsPath,
        scope: EnrichScope,
        enrichers: Vec<String>,
        tx: mpsc::Sender<EnrichmentEvent>,
    ) -> Result<(), Error>;
}

pub struct Local {
    enrichers: Arc<Enrichers>,
}

impl Local {
    pub fn new(enrichers: Arc<Enrichers>) -> Self {
        Self { enrichers }
    }
}

#[async_trait::async_trait]
impl EnricherClient for Local {
    async fn enrich(
        &self,
        path: VfsPath,
        scope: EnrichScope,
        enrichers: Vec<String>,
        tx: mpsc::Sender<EnrichmentEvent>,
    ) -> Result<(), Error> {
        self.enrichers.enrich(path, scope, enrichers, tx).await
    }
}

pub type PendingEnrichments = Arc<Mutex<HashMap<EnrichmentId, mpsc::Sender<EnrichmentEvent>>>>;

pub struct Remote {
    communicator: Communicator,
    pending: PendingEnrichments,
    next_id: std::sync::atomic::AtomicU64,
}

impl Remote {
    pub fn new(communicator: Communicator, pending: PendingEnrichments) -> Self {
        Self {
            communicator,
            pending,
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }
}

#[async_trait::async_trait]
impl EnricherClient for Remote {
    async fn enrich(
        &self,
        path: VfsPath,
        scope: EnrichScope,
        enrichers: Vec<String>,
        tx: mpsc::Sender<EnrichmentEvent>,
    ) -> Result<(), Error> {
        let id = EnrichmentId(
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );
        self.pending.lock().insert(id, tx);

        struct Guard {
            id: EnrichmentId,
            pending: PendingEnrichments,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                self.pending.lock().remove(&self.id);
            }
        }
        let _guard = Guard {
            id,
            pending: self.pending.clone(),
        };

        let ret: Result<(), Error> = self
            .communicator
            .invoke(
                crate::api::API_START_ENRICHMENT,
                &(id, path, scope, enrichers),
            )
            .await?;
        ret
    }
}

#[cfg(test)]
#[path = "enrich_tests.rs"]
mod tests;
