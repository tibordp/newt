//! `BackgroundJob` — reusable consumer-counted lifecycle primitive for
//! VFSes (and similar) whose backing work runs on a background task
//! and only needs to run while at least one observer is around.
//!
//! Shape:
//!
//! - **Lazy spawn**: the task isn't started until the first consumer
//!   `acquire`s the job. A search VFS that the user cancels before
//!   the first batch lands never starts its walker.
//! - **Consumer-counted lifetime**: each call to `acquire` returns a
//!   `ConsumerGuard` RAII handle. When the last guard drops *while the
//!   task is still running*, the cancellation token fires; the task is
//!   expected to honor the token and exit.
//! - **Status surfacing**: `JobStatus` is `Running | Done | Cancelled`.
//!   The task calls [`JobHandle::mark_done`] on natural completion
//!   (compare-and-swap from `Running`); cancellation is owned by the
//!   guard-drop logic.
//! - **Restart policy** decides what `acquire` does when the job has
//!   already been `Cancelled`:
//!   - [`RestartPolicy::Sticky`] — stays cancelled. The owner's
//!     partial state remains visible to new consumers (search results,
//!     tar's incremental directory tree). Re-running requires unmount.
//!   - [`RestartPolicy::Resettable`] — the next `acquire` resets to
//!     `Running`, mints a fresh cancellation token, and invokes the
//!     spawn closure again. Used when partial state is meaningless
//!     (zip central directory — it's at EOF, either all parsed or
//!     none).
//!
//! Both SearchVfs and the tar archive VFS use `Sticky`; the zip
//! archive VFS uses `Resettable`. Other future synthetic VFSes that
//! fit this pattern (recursive size, content indexing, …) can adopt
//! it with one struct field swap.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// JobStatus
// ---------------------------------------------------------------------------

const STATUS_RUNNING: u8 = 0;
const STATUS_DONE: u8 = 1;
const STATUS_CANCELLED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// The job either hasn't been started yet, or its task is alive.
    /// In both cases consumers should expect more state to land.
    Running,
    /// The task completed naturally — final state is whatever the
    /// owning struct accumulated.
    Done,
    /// The task was cancelled because the last consumer left (or
    /// because the job was explicitly cancelled). Whether new
    /// consumers re-spawn depends on `RestartPolicy`.
    Cancelled,
}

impl JobStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            STATUS_DONE => Self::Done,
            STATUS_CANCELLED => Self::Cancelled,
            _ => Self::Running,
        }
    }
}

// ---------------------------------------------------------------------------
// RestartPolicy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Once cancelled, the job stays cancelled. Subsequent `acquire`
    /// calls hand out guards but do not re-spawn the task. Use when
    /// partial accumulated state is meaningful and the owner wants
    /// it served as-is to future observers.
    Sticky,
    /// On the next `acquire` after a `Cancelled` transition, reset
    /// status to `Running`, mint a fresh cancellation token, and
    /// invoke the spawn closure again. Owner is responsible for
    /// clearing any partial state of its own *inside* the closure or
    /// just before calling `acquire` — `BackgroundJob` doesn't know
    /// what state belongs to it.
    Resettable,
}

// ---------------------------------------------------------------------------
// Inner state
// ---------------------------------------------------------------------------

struct Inner {
    status: AtomicU8,
    consumer_count: AtomicUsize,
    /// Whether the spawn closure has been invoked for the *current*
    /// run. Resettable policy clears this on transition out of
    /// `Cancelled` so the closure can be invoked again.
    started: Mutex<bool>,
    /// Current cancellation token. Swapped out for a fresh one on
    /// `Resettable` restart; captured per-task at spawn time.
    cancel: Mutex<CancellationToken>,
    policy: RestartPolicy,
}

// ---------------------------------------------------------------------------
// BackgroundJob
// ---------------------------------------------------------------------------

pub struct BackgroundJob {
    inner: Arc<Inner>,
}

impl BackgroundJob {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            inner: Arc::new(Inner {
                status: AtomicU8::new(STATUS_RUNNING),
                consumer_count: AtomicUsize::new(0),
                started: Mutex::new(false),
                cancel: Mutex::new(CancellationToken::new()),
                policy,
            }),
        }
    }

    pub fn status(&self) -> JobStatus {
        JobStatus::from_u8(self.inner.status.load(Ordering::Acquire))
    }

    /// Returns the *current* cancellation token. Note that the token
    /// can change across `Resettable` restarts — capture it freshly
    /// (or use `JobHandle::cancel_token`) at the point you actually
    /// need it.
    pub fn cancel_token(&self) -> CancellationToken {
        self.inner.cancel.lock().clone()
    }

    /// Acquire a consumer slot. Increments the consumer count; spawns
    /// the task via `spawn` if this is the first slot for the current
    /// run (or the first slot after a `Resettable` reset).
    ///
    /// The closure receives a `JobHandle` from which it observes the
    /// cancellation token and reports natural completion via
    /// `mark_done`.
    pub fn acquire(&self, spawn: impl FnOnce(JobHandle)) -> ConsumerGuard {
        self.inner.consumer_count.fetch_add(1, Ordering::AcqRel);

        let should_spawn = {
            let mut started = self.inner.started.lock();
            let status = JobStatus::from_u8(self.inner.status.load(Ordering::Acquire));
            match (status, self.inner.policy) {
                (JobStatus::Running, _) => {
                    if *started {
                        false
                    } else {
                        *started = true;
                        true
                    }
                }
                (JobStatus::Done, _) => false,
                (JobStatus::Cancelled, RestartPolicy::Sticky) => false,
                (JobStatus::Cancelled, RestartPolicy::Resettable) => {
                    // Mint a fresh token and reset status; the closure
                    // will see the new token via its JobHandle.
                    *self.inner.cancel.lock() = CancellationToken::new();
                    self.inner.status.store(STATUS_RUNNING, Ordering::Release);
                    *started = true;
                    true
                }
            }
        };

        if should_spawn {
            spawn(JobHandle {
                inner: self.inner.clone(),
            });
        }

        ConsumerGuard {
            inner: self.inner.clone(),
        }
    }
}

impl Drop for BackgroundJob {
    fn drop(&mut self) {
        // When the owning struct goes away (unmount, etc.), make sure
        // any outstanding task exits promptly. Cancelling an
        // already-cancelled token is a no-op.
        self.inner.cancel.lock().cancel();
    }
}

// ---------------------------------------------------------------------------
// JobHandle — the task's view of the job
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct JobHandle {
    inner: Arc<Inner>,
}

impl JobHandle {
    pub fn cancel_token(&self) -> CancellationToken {
        self.inner.cancel.lock().clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_token().is_cancelled()
    }

    /// Called by the task when it completes naturally (didn't observe
    /// a cancellation). Idempotent; never overwrites a `Cancelled`
    /// status.
    pub fn mark_done(&self) {
        let _ = self.inner.status.compare_exchange(
            STATUS_RUNNING,
            STATUS_DONE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

// ---------------------------------------------------------------------------
// ConsumerGuard — RAII handle for an active consumer
// ---------------------------------------------------------------------------

pub struct ConsumerGuard {
    inner: Arc<Inner>,
}

impl Drop for ConsumerGuard {
    fn drop(&mut self) {
        let prev = self.inner.consumer_count.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            // We were the last observer. If the task is still alive,
            // cancel it — the partial state it's accumulated remains
            // available to any future consumer (until policy decides
            // whether to restart).
            //
            // `compare_exchange` so we don't clobber a `Done` that
            // landed in the same window — losing-race semantics are
            // "task finished naturally before our cancel could fire,
            // which is fine, leave Done alone".
            if self
                .inner
                .status
                .compare_exchange(
                    STATUS_RUNNING,
                    STATUS_CANCELLED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.inner.cancel.lock().cancel();
            }
        }
    }
}
