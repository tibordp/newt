//! Agent mounts: a spawn-style connection (SSH, docker, podman, kubectl,
//! custom) mounted as a VFS in a pane, instead of remoting a whole session.
//! The sub-agent runs FS-only (`--serve-vfs`): it serves the VFS API over
//! its local filesystem and nothing else, so it cannot mount archives or
//! further nested agents on its end. See DESIGN_AGENT_VFS_MOUNTS.md.
//!
//! The proxy side reuses `RemoteVfs` — the same `Vfs`-over-`Communicator`
//! impl, pointed at the sub-agent connection instead of the host.

use std::sync::Arc;

use crate::Error;
use crate::api::{MountContext, PendingVfsReadStreams, VfsReadChunkDispatcher};
use crate::askpass::{AskpassProvider, AskpassRequest, AskpassResponse};
use crate::connect::{AgentMode, ConnectLog, SpawnSpec};
use crate::rpc::Communicator;
use crate::vfs::path::{Path, PathBuf};

use super::{
    Breadcrumb, DisplayPathMatch, PathStyle, RegisteredDescriptor, RemoteVfs, Vfs, VfsDescriptor,
    VfsProgress, encode_mount_meta_labeled, mount_meta_label,
};

// ---------------------------------------------------------------------------
// AgentVfsDescriptor — same shape as RemoteVfsDescriptor (both proxy a
// LocalVfs over RPC), but a distinct type_name: "remote" is the
// client-local FS with direction-flipped display names and hairpin
// treatment in remote sessions, which an agent mount must not inherit.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AgentVfsDescriptor;

impl VfsDescriptor for AgentVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "agent"
    }
    fn display_name(&self) -> &'static str {
        "Remote"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read_sync(&self) -> bool {
        false
    }
    fn can_read_async(&self) -> bool {
        true
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        true
    }
    fn can_create_directory(&self) -> bool {
        true
    }
    fn can_create_symlink(&self) -> bool {
        true
    }
    fn can_touch(&self) -> bool {
        true
    }
    fn can_truncate(&self) -> bool {
        true
    }
    fn can_set_metadata(&self) -> bool {
        true
    }
    fn can_remove(&self) -> bool {
        true
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_stat_directories(&self) -> bool {
        true
    }
    fn can_fs_stats(&self) -> bool {
        true
    }
    fn can_rename(&self) -> bool {
        true
    }
    fn can_copy_within(&self) -> bool {
        true
    }
    fn can_hard_link(&self) -> bool {
        true
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        super::local::local_display_path(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        super::local::local_breadcrumbs(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn navigable_parent(&self, path: &Path, mount_meta: &[u8]) -> Option<PathBuf> {
        super::local::navigable_parent(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        mount_meta_label(mount_meta)
    }

    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        None
    }
}

pub static AGENT_VFS_DESCRIPTOR: AgentVfsDescriptor = AgentVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&AGENT_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// Connection lifetime
// ---------------------------------------------------------------------------

/// Owns the sub-agent process (and its askpass listener) for the lifetime
/// of the mount. A reaper task waits on the child, so an agent that dies on
/// its own is collected immediately; dropping the guard (unmount) kills it.
pub struct AgentConnectionGuard {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl AgentConnectionGuard {
    pub fn new(
        mut child: tokio::process::Child,
        askpass: Option<crate::askpass::listener::AskpassListener>,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _askpass = askpass;
            tokio::select! {
                status = child.wait() => {
                    log::info!("agent mount subprocess exited: {:?}", status);
                }
                _ = shutdown_rx => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    log::info!("agent mount subprocess terminated on unmount");
                }
            }
        });
        Self {
            shutdown: Some(shutdown_tx),
        }
    }
}

impl Drop for AgentConnectionGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// Mount
// ---------------------------------------------------------------------------

/// Bridge spawn-progress lines into the mount's VFS progress channel (so
/// bootstrap/upload status shows where every other mount reports progress)
/// while keeping a transcript — a failed mount attaches it to the error, so
/// the dialog shows *why* instead of a bare failure.
struct MountConnectLog {
    reporter: Arc<dyn super::ProgressReporter>,
    lines: parking_lot::Mutex<Vec<String>>,
}

impl MountConnectLog {
    fn transcript(&self) -> String {
        self.lines.lock().join("\n")
    }
}

impl ConnectLog for MountConnectLog {
    fn log(&self, line: String) {
        log::info!("agent mount: {}", line);
        self.lines.lock().push(line.clone());
        self.reporter.report(Some(VfsProgress {
            stage: line,
            processed: None,
            total: None,
            extra: Default::default(),
        }));
    }
}

/// Fallback provider when the session has no askpass channel: every prompt
/// is answered as cancelled, so a password-requiring transport fails fast
/// instead of hanging.
struct CancelAskpass;

#[async_trait::async_trait]
impl AskpassProvider for CancelAskpass {
    async fn prompt(&self, _req: AskpassRequest) -> AskpassResponse {
        AskpassResponse(None)
    }
}

pub async fn mount(
    spec: SpawnSpec,
    kind: String,
    label: String,
    ctx: &MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    let log = Arc::new(MountConnectLog {
        reporter: ctx.progress_reporter.clone(),
        lines: parking_lot::Mutex::new(Vec::new()),
    });
    let result = mount_inner(spec, &kind, &label, log.clone(), ctx).await;
    // The spawn-progress lines above are one-shot; clear them regardless of
    // outcome so a failed mount doesn't leave a stale progress entry.
    ctx.progress_reporter.report(None);
    result.map_err(|e| {
        let transcript = log.transcript();
        if transcript.is_empty() {
            e
        } else {
            Error {
                kind: e.kind,
                message: format!("{}\n\nConnection log:\n{}", e.message, transcript),
            }
        }
    })
}

async fn mount_inner(
    spec: SpawnSpec,
    kind: &str,
    label: &str,
    log: Arc<MountConnectLog>,
    ctx: &MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    let resolver = ctx
        .agent_resolver
        .ok_or_else(|| Error::custom("agent mounts are not available in this session"))?;
    let askpass_provider: Arc<dyn AskpassProvider> = match ctx.askpass_provider {
        Some(p) => p.clone(),
        None => Arc::new(CancelAskpass),
    };
    let connect_log: Arc<dyn ConnectLog> = log.clone();

    let mut spawned = crate::connect::spawn(
        &spec,
        AgentMode::ServeVfs,
        ctx.extra_path,
        resolver.as_ref(),
        askpass_provider,
        connect_log.clone(),
    )
    .await?;

    // The direct-exec transports hand back an unread stderr pipe (the
    // bootstrap path consumes stderr during its handshake). Keep reading it —
    // it is where in-container failures like "exec format error" surface.
    if let Some(stderr) = spawned.stderr.take() {
        crate::connect::spawn_stderr_reader(stderr, connect_log.clone());
    }

    // Parent side of the sub-agent connection only routes read-chunk
    // notifications; an FS-only agent never invokes anything else on us.
    let pending_read_streams: PendingVfsReadStreams = Default::default();
    let (outbox, inbox) = Communicator::create_outbox();
    let dispatcher = VfsReadChunkDispatcher::new(pending_read_streams.clone());
    let communicator =
        Communicator::with_dispatcher_and_outbox(dispatcher, spawned.stream, outbox, inbox);

    let guard = AgentConnectionGuard::new(spawned.child, spawned.askpass);

    // Sub-agents are Unix-shaped today: bootstrap and direct-copy only
    // target linux/darwin triples (`triple_from_os_arch`).
    let mount_meta = encode_mount_meta_labeled(PathStyle::Unix, &[], Some(kind), Some(label));

    let vfs = Arc::new(RemoteVfs::for_agent(
        communicator.clone(),
        pending_read_streams,
        mount_meta,
        guard,
    ));

    // Startup probe. The bootstrap transports handshake before RPC, but the
    // direct-exec ones don't — without this, an agent that dies on exec
    // (wrong arch, missing interpreter, …) would still "mount" and produce
    // a VFS that fails every operation. Racing against connection close
    // turns that into a mount error carrying the stderr transcript; a beat
    // for the stderr reader lets the actual diagnostic land in it first.
    log.log("Verifying agent connection...".to_string());
    let probe_path = PathBuf::root();
    tokio::select! {
        r = vfs.get_metadata(&probe_path) => {
            r.map_err(|e| Error::custom(format!("agent startup probe failed: {}", e)))?;
        }
        _ = communicator.closed() => {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            return Err(Error::custom(
                "agent exited during startup (see connection log)",
            ));
        }
    }
    log.log("Connected".to_string());

    Ok(vfs)
}
