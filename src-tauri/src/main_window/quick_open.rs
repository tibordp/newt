//! Go to File: the palette's run, owned by its modal.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use newt_common::quick_open::{QuickOpenOptions, QuickOpenUpdate};
use newt_common::vfs::VfsPath;
use tokio::sync::{mpsc, watch};

use super::{MainWindowContext, ModalData, ModalDataKind};
use crate::common::Error;

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// The walk and ranking behind an open Go to File palette. It lives in the
/// modal's data, so whatever closes or replaces the modal drops it, and
/// dropping it stops the run.
#[derive(Clone)]
pub struct QuickOpenRun(Arc<Run>);

struct Run {
    id: u64,
    query: watch::Sender<String>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl Drop for Run {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl QuickOpenRun {
    /// Walk `root` and keep the modal's results ranked against the query.
    pub fn start(
        ctx: &MainWindowContext,
        root: VfsPath,
        options: QuickOpenOptions,
    ) -> Result<Self, Error> {
        let client = ctx.quick_open_client()?;
        let id = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
        let (query, query_rx) = watch::channel(String::new());
        let ctx = ctx.clone();
        let task = tauri::async_runtime::spawn(async move {
            let (tx, mut rx) = mpsc::channel(4);
            let run = client.run(root, options, query_rx, tx);
            tokio::pin!(run);
            loop {
                tokio::select! {
                    result = &mut run => {
                        if let Err(e) = result {
                            show(&ctx, id, failed(e.to_string()));
                        }
                        return;
                    }
                    Some(update) = rx.recv() => show(&ctx, id, update),
                }
            }
        });
        Ok(Self(Arc::new(Run { id, query, task })))
    }

    pub fn set_query(&self, query: String) {
        self.0.query.send_replace(query);
    }
}

/// Put `update` in the modal, if the modal is still run `id`'s.
fn show(ctx: &MainWindowContext, id: u64, new: QuickOpenUpdate) {
    let _ = ctx.with_update(|gs| {
        if let Some(ModalData {
            kind: ModalDataKind::QuickOpen { run, update, .. },
            ..
        }) = gs.modal.0.write().as_mut()
            && run.0.id == id
        {
            *update = Some(new);
        }
        Ok(())
    });
}

fn failed(error: String) -> QuickOpenUpdate {
    QuickOpenUpdate {
        query: String::new(),
        results: Vec::new(),
        matched: 0,
        walked: 0,
        walking: false,
        unreadable: 0,
        truncated: false,
        error: Some(error),
    }
}
