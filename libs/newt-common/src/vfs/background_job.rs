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
//!     spawn closure again. The owner decides what the new run starts
//!     from; the archive indexers park their engine on cancellation and
//!     the next run picks it up where it stopped.
//!
//! SearchVfs uses `Sticky`; the tar and compressed-file archive VFSes
//! use `Resettable`.
//!
//! Every transition (acquire, last-guard release, natural completion)
//! happens under one lock, so a consumer arriving as the last one leaves
//! either keeps the run alive or starts the next one — never lands on a
//! run that is being cancelled.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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
    /// Read lock-free by `status()`; written only while holding `run`.
    status: AtomicU8,
    run: Mutex<Run>,
    policy: RestartPolicy,
}

struct Run {
    consumers: usize,
    /// Whether the spawn closure has been invoked for the current run.
    started: bool,
    /// The current run's token. Replaced on a `Resettable` restart; each
    /// run's `JobHandle` keeps its own.
    cancel: CancellationToken,
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
                run: Mutex::new(Run {
                    consumers: 0,
                    started: false,
                    cancel: CancellationToken::new(),
                }),
                policy,
            }),
        }
    }

    pub fn status(&self) -> JobStatus {
        JobStatus::from_u8(self.inner.status.load(Ordering::Acquire))
    }

    /// The *current* run's cancellation token; a `Resettable` restart
    /// replaces it. A task observes its own run through its `JobHandle`.
    pub fn cancel_token(&self) -> CancellationToken {
        self.inner.run.lock().cancel.clone()
    }

    /// Acquire a consumer slot, spawning the task via `spawn` if this
    /// is the first slot for the current run (or the first after a
    /// `Resettable` reset).
    ///
    /// The closure receives a `JobHandle` from which the task observes
    /// its run's cancellation and reports natural completion via
    /// `mark_done`.
    pub fn acquire(&self, spawn: impl FnOnce(JobHandle)) -> ConsumerGuard {
        let spawn_with = {
            let mut run = self.inner.run.lock();
            run.consumers += 1;
            match (self.status(), self.inner.policy) {
                (JobStatus::Running, _) if !run.started => {
                    run.started = true;
                    Some(run.cancel.clone())
                }
                (JobStatus::Cancelled, RestartPolicy::Resettable) => {
                    run.cancel = CancellationToken::new();
                    self.inner.status.store(STATUS_RUNNING, Ordering::Release);
                    Some(run.cancel.clone())
                }
                _ => None,
            }
        };

        if let Some(cancel) = spawn_with {
            spawn(JobHandle {
                inner: self.inner.clone(),
                cancel,
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
        self.inner.run.lock().cancel.cancel();
    }
}

// ---------------------------------------------------------------------------
// JobHandle — the task's view of the job
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct JobHandle {
    inner: Arc<Inner>,
    cancel: CancellationToken,
}

impl JobHandle {
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Called by the task when it completes naturally. A no-op once this
    /// run has been cancelled, so a run that finishes as it's cancelled
    /// can't mark the run restarted after it done.
    pub fn mark_done(&self) {
        let _run = self.inner.run.lock();
        if !self.cancel.is_cancelled() {
            let _ = self.inner.status.compare_exchange(
                STATUS_RUNNING,
                STATUS_DONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
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
        let mut run = self.inner.run.lock();
        run.consumers -= 1;
        // The last observer leaving cancels a live task. A `Done` that
        // landed first stays `Done`.
        if run.consumers == 0
            && self
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
            run.cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_counter(job: &BackgroundJob, spawns: &mut Vec<JobHandle>) -> ConsumerGuard {
        job.acquire(|handle| spawns.push(handle))
    }

    #[test]
    fn spawns_once_per_run_and_cancels_with_the_last_consumer() {
        let job = BackgroundJob::new(RestartPolicy::Sticky);
        let mut spawns = Vec::new();
        let a = spawn_counter(&job, &mut spawns);
        let b = spawn_counter(&job, &mut spawns);
        assert_eq!(spawns.len(), 1);
        drop(a);
        assert!(!spawns[0].is_cancelled());
        drop(b);
        assert!(spawns[0].is_cancelled());
        assert_eq!(job.status(), JobStatus::Cancelled);

        let _c = spawn_counter(&job, &mut spawns);
        assert_eq!(spawns.len(), 1, "sticky never restarts");
    }

    #[test]
    fn resettable_restart_keeps_runs_apart() {
        let job = BackgroundJob::new(RestartPolicy::Resettable);
        let mut spawns = Vec::new();
        drop(spawn_counter(&job, &mut spawns));
        let _second = spawn_counter(&job, &mut spawns);
        assert_eq!(spawns.len(), 2);
        assert_eq!(job.status(), JobStatus::Running);

        // The first run still sees its own cancellation after the restart,
        // and finishing late doesn't mark the second run done.
        assert!(spawns[0].is_cancelled());
        assert!(!spawns[1].is_cancelled());
        spawns[0].mark_done();
        assert_eq!(job.status(), JobStatus::Running);
        spawns[1].mark_done();
        assert_eq!(job.status(), JobStatus::Done);
    }

    #[test]
    fn done_survives_the_last_consumer_leaving() {
        let job = BackgroundJob::new(RestartPolicy::Resettable);
        let mut spawns = Vec::new();
        let guard = spawn_counter(&job, &mut spawns);
        spawns[0].mark_done();
        drop(guard);
        assert_eq!(job.status(), JobStatus::Done);
        assert!(!spawns[0].is_cancelled());
        let _again = spawn_counter(&job, &mut spawns);
        assert_eq!(spawns.len(), 1);
    }
}
