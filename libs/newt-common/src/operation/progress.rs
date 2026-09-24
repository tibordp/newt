use super::*;

// --- IssueOutcome: result of handle_io_error ---

pub(super) enum IssueOutcome {
    Skip,
    Retry,
}

// --- ProgressReporter: async issue resolution + progress ---

/// Minimum interval between progress/scanning notifications. The host
/// throttles UI updates anyway; sending more is wasted work.
const PROGRESS_THROTTLE: std::time::Duration = std::time::Duration::from_millis(100);

pub(super) struct ProgressReporter {
    id: OperationId,
    progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>,
    last_report: Mutex<std::time::Instant>,
    issue_resolvers: IssueResolvers,
    next_issue_id: Arc<AtomicU64>,
    sticky_resolutions: HashMap<IssueKind, IssueAction>,
    cancel: CancellationToken,
}

impl ProgressReporter {
    pub(super) fn new(
        id: OperationId,
        progress_tx: tokio::sync::mpsc::UnboundedSender<OperationProgress>,
        issue_resolvers: IssueResolvers,
        next_issue_id: Arc<AtomicU64>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            id,
            progress_tx,
            last_report: Mutex::new(std::time::Instant::now()),
            issue_resolvers,
            next_issue_id,
            sticky_resolutions: HashMap::new(),
            cancel,
        }
    }

    pub(super) fn id(&self) -> OperationId {
        self.id
    }

    pub(super) fn send(&self, progress: OperationProgress) {
        let _ = self.progress_tx.send(progress);
    }

    pub(super) fn send_prepared(&self, total_bytes: u64, total_items: u64) {
        self.send(OperationProgress::Prepared {
            id: self.id(),
            total_bytes,
            total_items,
        });
    }

    /// Rate-limited to `PROGRESS_THROTTLE`; returns without sending inside
    /// the window.
    pub(super) fn maybe_send_progress(&self, bytes_done: u64, items_done: u64, current_item: &str) {
        let now = std::time::Instant::now();
        let mut last = self.last_report.lock();
        if now.duration_since(*last) >= PROGRESS_THROTTLE {
            *last = now;
            drop(last);
            self.send(OperationProgress::Progress {
                id: self.id,
                bytes_done,
                items_done,
                current_item: current_item.to_string(),
            });
        }
    }

    pub(super) fn maybe_send_scanning(&self, items_found: u64, bytes_found: u64) {
        let now = std::time::Instant::now();
        let mut last = self.last_report.lock();
        if now.duration_since(*last) >= PROGRESS_THROTTLE {
            *last = now;
            drop(last);
            self.send(OperationProgress::Scanning {
                id: self.id,
                items_found,
                bytes_found,
            });
        }
    }

    pub(super) fn send_completed(&self) {
        self.send(OperationProgress::Completed { id: self.id() });
    }

    pub(super) fn send_failed(&self, error: String) {
        self.send(OperationProgress::Failed {
            id: self.id(),
            error,
        });
    }

    pub(super) fn send_cancelled(&self) {
        self.send(OperationProgress::Cancelled { id: self.id() });
    }

    /// Answer issues of `kind` up front, as "apply to all" would.
    pub(super) fn preset(&mut self, kind: IssueKind, action: Option<IssueAction>) {
        if let Some(action) = action {
            self.sticky_resolutions.insert(kind, action);
        }
    }

    pub(super) async fn raise_issue(
        &mut self,
        kind: IssueKind,
        message: String,
        detail: Option<String>,
        actions: Vec<IssueAction>,
    ) -> Result<IssueAction, crate::Error> {
        if self.cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        // A sticky answer covers later issues of the kind that offer it; a
        // type mismatch after "Overwrite all" still asks.
        if let Some(&action) = self.sticky_resolutions.get(&kind)
            && actions.contains(&action)
        {
            return Ok(action);
        }

        let issue_id = self.next_issue_id.fetch_add(1, Ordering::Relaxed);

        let (tx, rx) = oneshot::channel();
        self.issue_resolvers.lock().insert(issue_id, tx);

        self.send(OperationProgress::Issue {
            id: self.id(),
            issue: OperationIssue {
                issue_id,
                kind: kind.clone(),
                message,
                detail,
                actions,
            },
        });

        tokio::select! {
            result = rx => {
                match result {
                    Ok(response) => {
                        if response.apply_to_all && response.action != IssueAction::Retry {
                            self.sticky_resolutions.insert(kind, response.action);
                        }
                        Ok(response.action)
                    }
                    Err(_) => Err(crate::Error::cancelled()),
                }
            }
            _ = self.cancel.cancelled() => {
                Err(crate::Error::cancelled())
            }
        }
    }

    /// Asks about a file in the way of another; true to replace it.
    pub(super) async fn replace_existing(
        &mut self,
        dest: &PathBuf,
        source: &File,
        existing: &File,
    ) -> Result<bool, crate::Error> {
        let action = self
            .raise_issue(
                IssueKind::AlreadyExists,
                format!("File already exists: {}", dest),
                None,
                vec![
                    IssueAction::Skip,
                    IssueAction::Overwrite,
                    IssueAction::OverwriteIfNewer,
                    IssueAction::OverwriteIfSizeDiffers,
                    IssueAction::OverwriteIfSizeOrDateDiffers,
                ],
            )
            .await?;
        Ok(match action {
            IssueAction::Skip => false,
            IssueAction::Overwrite => true,
            IssueAction::OverwriteIfNewer => matches!(
                (source.modified, existing.modified),
                (Some(s), Some(e)) if s > e
            ),
            IssueAction::OverwriteIfSizeDiffers => !same(source.size, existing.size),
            IssueAction::OverwriteIfSizeOrDateDiffers => {
                !same(source.size, existing.size) || !same(source.modified, existing.modified)
            }
            IssueAction::Retry => unreachable!("not offered"),
        })
    }

    pub(super) async fn handle_io_error(
        &mut self,
        error: crate::Error,
        context: &str,
        detail: Option<String>,
        cancel: &CancellationToken,
        allow_retry: bool,
    ) -> Result<IssueOutcome, crate::Error> {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        warn!("operation {}: {} — {}", self.id(), context, error);
        // `IssueKind::AlreadyExists` means a destination conflict, which a
        // preset answers; an I/O error saying "already exists" is not one.
        let kind = match error.kind {
            crate::ErrorKind::PermissionDenied => IssueKind::PermissionDenied,
            _ => IssueKind::IoError,
        };
        let mut actions = vec![IssueAction::Skip];
        if allow_retry {
            actions.push(IssueAction::Retry);
        }

        match self
            .raise_issue(kind, format!("{}: {}", context, error), detail, actions)
            .await?
        {
            IssueAction::Skip => Ok(IssueOutcome::Skip),
            IssueAction::Retry => Ok(IssueOutcome::Retry),
            _ => unreachable!("not offered"),
        }
    }
}

/// Unknown on either side is never the same: an unknown size or time
/// counts as a difference.
fn same<T: PartialEq>(a: Option<T>, b: Option<T>) -> bool {
    a.is_some() && a == b
}
