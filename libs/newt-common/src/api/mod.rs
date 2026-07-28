use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use std::sync::atomic::AtomicU64;

use crate::{
    Error,
    filesystem::{Filesystem, ListFilesOptions, StreamId},
    hot_paths::HotPathsProvider,
    operation::{self, OperationHandle, OperationId, ResolveIssueRequest, StartOperationRequest},
    rpc::{Api, Dispatcher, Message, Outbox},
    shell::ShellService,
    terminal::TerminalClient,
    vfs::{FileList, MountRequest, VfsId, VfsManager, VfsPath},
};

pub const API_POLL_CHANGES: Api = Api(0);
pub const API_LIST_FILES: Api = Api(1);
// api 2 retired (rename — now OperationRequest::Rename)
pub const API_CREATE_DIRECTORY: Api = Api(3);
// api 4 missing
pub const API_TOUCH: Api = Api(5);
pub const API_SHELL_EXPAND: Api = Api(6);
pub const API_LIST_FILES_STREAMING: Api = Api(7);
pub const API_LIST_FILES_BATCH: Api = Api(8);
pub const API_REVALIDATE: Api = Api(9);
pub const API_FS_STATS: Api = Api(10);

pub const API_START_OPERATION: Api = Api(200);
pub const API_CANCEL_OPERATION: Api = Api(201);
pub const API_OPERATION_PROGRESS: Api = Api(202);
pub const API_RESOLVE_ISSUE: Api = Api(203);

pub const API_TERMINAL_CREATE: Api = Api(100);
pub const API_TERMINAL_KILL: Api = Api(101);
pub const API_TERMINAL_RESIZE: Api = Api(102);
pub const API_TERMINAL_INPUT: Api = Api(103);
pub const API_TERMINAL_READ: Api = Api(104);
pub const API_TERMINAL_WAIT: Api = Api(105);

pub const API_FILE_DETAILS: Api = Api(300);
pub const API_READ_RANGE: Api = Api(301);
pub const API_READ_FILE: Api = Api(302);
pub const API_WRITE_FILE: Api = Api(303);
pub const API_FIND_IN_FILE: Api = Api(304);
pub const API_GET_PROPERTY_SHEET: Api = Api(305);
// Filesystem-level positioned-read handles — same open/read/close shape as
// the API_VFS_* triple, addressed by VfsPath instead of an in-VFS path.
pub const API_OPEN_READ_AT: Api = Api(306);
pub const API_READ_AT: Api = Api(307);
pub const API_READ_AT_CLOSE: Api = Api(308);

pub const API_MOUNT_VFS: Api = Api(400);
pub const API_UNMOUNT_VFS: Api = Api(401);
pub const API_VFS_PROGRESS: Api = Api(402);
pub const API_REMOUNT_VFS: Api = Api(403);

pub const API_SYSTEM_HOT_PATHS: Api = Api(500);

// Enrichers — long-lived streaming invoke; partial results ride
// API_ENRICHMENT_EVENT notifications correlated by EnrichmentId, and
// cancellation is transport-level (drop the invoke → InvokeCancel).
pub const API_START_ENRICHMENT: Api = Api(700);
pub const API_ENRICHMENT_EVENT: Api = Api(701);

// Connect-dialog discovery — runs on the session owner, so pane-scoped
// agent mounts list the targets they would actually reach.
pub const API_DISCOVER_SSH_HOSTS: Api = Api(510);
pub const API_DISCOVER_CONTAINERS: Api = Api(511);
pub const API_DISCOVER_KUBE_CONTEXTS: Api = Api(512);
pub const API_DISCOVER_KUBE_PODS: Api = Api(513);

// Remote VFS APIs — `RemoteVfs` (the caller) drives a real VFS living on the
// other end of a connection. Direction is symmetric: the caller may run on the
// agent and reach the Tauri host's VFS, or run on the host and reach a spawned
// FS-only sub-agent's VFS (an agent mount).
pub const API_VFS_LIST_FILES: Api = Api(600);
pub const API_VFS_POLL_CHANGES: Api = Api(601);
pub const API_VFS_FS_STATS: Api = Api(602);
pub const API_VFS_OPEN_READ_ASYNC: Api = Api(603);
pub const API_VFS_READ_CHUNK: Api = Api(621);
pub const API_VFS_READ_RANGE: Api = Api(604);
pub const API_VFS_FILE_DETAILS: Api = Api(605);
pub const API_VFS_FILE_INFO: Api = Api(606);
pub const API_VFS_OVERWRITE_ASYNC_BEGIN: Api = Api(607);
pub const API_VFS_WRITE_CHUNK: Api = Api(622);
pub const API_VFS_OVERWRITE_ASYNC_FINISH: Api = Api(623);
pub const API_VFS_OVERWRITE_ASYNC_ABORT: Api = Api(629);
pub const API_VFS_CREATE_DIRECTORY: Api = Api(608);
pub const API_VFS_CREATE_SYMLINK: Api = Api(609);
pub const API_VFS_TOUCH: Api = Api(610);
pub const API_VFS_TRUNCATE: Api = Api(611);
pub const API_VFS_REMOVE_FILE: Api = Api(612);
pub const API_VFS_REMOVE_DIR: Api = Api(613);
pub const API_VFS_REMOVE_TREE: Api = Api(614);
pub const API_VFS_GET_METADATA: Api = Api(615);
pub const API_VFS_SET_METADATA: Api = Api(616);
pub const API_VFS_AVAILABLE_SPACE: Api = Api(617);
pub const API_VFS_RENAME: Api = Api(618);
pub const API_VFS_COPY_WITHIN: Api = Api(619);
pub const API_VFS_HARD_LINK: Api = Api(620);
pub const API_VFS_TRASH_ITEM: Api = Api(628);
pub const API_VFS_SAME_FILE: Api = Api(632);
// Positioned-read handles: OPEN_READ_AT mints a server-held reader,
// READ_AT is request/response per chunk, READ_AT_CLOSE (notify, sent on
// proxy drop) reaps it.
pub const API_VFS_OPEN_READ_AT: Api = Api(633);
pub const API_VFS_READ_AT: Api = Api(634);
pub const API_VFS_READ_AT_CLOSE: Api = Api(635);

// Host UI APIs — invoked by the agent, handled by the Tauri host.
pub const API_HOST_ASKPASS: Api = Api(624);
// Shell-integration control plane: the agent's `newt` CLI server forwards
// verbs to the host's session state (payload: shell_control::ControlRequest,
// reply: shell_control::ControlResult).
pub const API_HOST_SHELL_CONTROL: Api = Api(631);

// Agent-binary provisioning — invoked by the agent (nested spawns for
// pane-scoped agent mounts), served from the host's agents dir.
pub const API_HOST_AGENT_HASH: Api = Api(625);
pub const API_HOST_FETCH_AGENT: Api = Api(626);
pub const API_HOST_FETCH_AGENT_CHUNK: Api = Api(627);
pub const API_HOST_FETCH_AGENT_CANCEL: Api = Api(630);

mod agent_fetch;
mod read_at;
mod vfs;
pub use agent_fetch::{AgentFetchDispatcher, AgentFetchHeader};
pub(crate) use read_at::{ReadAtSessions, RemoteRandomReader};
pub use vfs::{PendingVfsReadStreams, ReadStream, VfsDispatcher, VfsReadChunkDispatcher};

// bincode helpers — propagate decode/encode failures as structured errors so
// a malformed payload (bad agent, version skew, …) doesn't crash the process.

pub fn decode<'a, T: serde::Deserialize<'a>>(req: &'a [u8]) -> Result<T, Error> {
    bincode::deserialize(req).map_err(|e| Error::custom(format!("RPC decode: {}", e)))
}

pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    bincode::serialize(value).map_err(|e| Error::custom(format!("RPC encode: {}", e)))
}

/// Best-effort encode used by streaming notifications: there's no Result to
/// propagate from a spawned task, so failures (which never happen in
/// practice for these types) are logged and the notification is dropped.
pub(super) fn try_encode<T: serde::Serialize>(value: &T) -> Option<Vec<u8>> {
    match bincode::serialize(value) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            log::error!("RPC streaming encode: {}", e);
            None
        }
    }
}

/// How a chunk stream ends when a read fails mid-stream.
#[derive(Clone, Copy)]
enum OnReadError {
    /// Stop without the EOF sentinel — the error travels back on the
    /// invoke response and the consumer tears the stream down from there.
    Abort,
    /// Send the sentinel anyway, ending the stream short — the consumer
    /// detects the shortfall against the announced size.
    Truncate,
}

/// Pump `reader` to the outbox as sequenced `(stream_id, seq, bytes)`
/// notifications on `api`, ending with the empty EOF sentinel. The read
/// error (if any) is returned either way; `on_read_error` picks the
/// protocol for how the stream itself ends.
async fn send_chunk_stream(
    outbox: &Outbox,
    api: Api,
    stream_id: StreamId,
    reader: &mut (dyn tokio::io::AsyncRead + Send + Unpin),
    on_read_error: OnReadError,
) -> Result<(), Error> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; crate::vfs::VFS_READ_CHUNK_SIZE];
    let mut seq: u64 = 0;
    let mut read_error = None;
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                // serde_bytes for all chunk payloads: bincode's serde path
                // walks Vec<u8> per byte (~30x slower than memcpy) and only
                // `serialize_bytes` / `deserialize_byte_buf` hit its fast
                // path. The wire format is identical.
                let chunk = serde_bytes::Bytes::new(&buf[..n]);
                if let Some(bytes) = try_encode(&(stream_id, seq, chunk)) {
                    outbox
                        .send(Message::Notify(api, bytes.into()))
                        .await
                        .map_err(|_| Error::connection())?;
                }
                seq += 1;
            }
            Err(e) => match on_read_error {
                OnReadError::Abort => return Err(e.into()),
                OnReadError::Truncate => {
                    read_error = Some(e.into());
                    break;
                }
            },
        }
    }
    if let Some(bytes) = try_encode(&(stream_id, seq, serde_bytes::Bytes::new(&[]))) {
        outbox
            .send(Message::Notify(api, bytes.into()))
            .await
            .map_err(|_| Error::connection())?;
    }
    match read_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub struct FilesystemDispatcher {
    filesystem: Box<dyn Filesystem>,
    outbox: Outbox,
    read_at_sessions: ReadAtSessions,
}

impl FilesystemDispatcher {
    pub fn new<F: Filesystem + 'static>(filesystem: F, outbox: Outbox) -> Self {
        Self {
            filesystem: Box::new(filesystem),
            outbox,
            read_at_sessions: ReadAtSessions::new(),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for FilesystemDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_POLL_CHANGES => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.filesystem.poll_changes(path).await;

                encode(&ret)?
            }
            API_LIST_FILES => {
                let args: (VfsPath, ListFilesOptions) = decode(&req[..])?;
                let ret = self.filesystem.list_files(args.0, args.1, None).await;

                encode(&ret)?
            }
            API_LIST_FILES_STREAMING => {
                let (path, opts, stream_id): (VfsPath, ListFilesOptions, StreamId) =
                    decode(&req[..])?;

                let (batch_tx, mut batch_rx) = tokio::sync::mpsc::channel::<FileList>(
                    crate::filesystem::LIST_BATCH_CHANNEL_CAPACITY,
                );

                let list = self.filesystem.list_files(path, opts, Some(batch_tx));
                tokio::pin!(list);

                // Keep the producer and forwarder in this RPC task. Dropping
                // the invoke now drops batch_rx immediately, which propagates
                // cancellation through every bounded bridge to LocalVFS.
                let ret = loop {
                    tokio::select! {
                        ret = &mut list => break ret,
                        batch = batch_rx.recv() => {
                            let Some(file_list) = batch else {
                                break (&mut list).await;
                            };
                            if let Some(bytes) = try_encode(&(stream_id, file_list)) {
                                self.outbox
                                    .send(Message::Notify(API_LIST_FILES_BATCH, bytes.into()))
                                    .await
                                    .map_err(|_| Error::connection())?;
                            }
                        }
                    }
                };

                // The listing is complete and has dropped its sender; preserve
                // notification-before-response ordering by draining the queue.
                while let Some(file_list) = batch_rx.recv().await {
                    if let Some(bytes) = try_encode(&(stream_id, file_list)) {
                        self.outbox
                            .send(Message::Notify(API_LIST_FILES_BATCH, bytes.into()))
                            .await
                            .map_err(|_| Error::connection())?;
                    }
                }

                encode(&ret)?
            }
            API_TOUCH => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.filesystem.touch(path).await;

                encode(&ret)?
            }
            API_CREATE_DIRECTORY => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.filesystem.create_directory(path).await;

                encode(&ret)?
            }
            API_REVALIDATE => {
                let vfs_id: VfsId = decode(&req[..])?;
                let ret = self.filesystem.revalidate(vfs_id).await;
                encode(&ret)?
            }
            API_FS_STATS => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.filesystem.fs_stats(path).await;
                encode(&ret)?
            }
            API_FILE_DETAILS => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.filesystem.file_details(path).await;

                encode(&ret)?
            }
            API_GET_PROPERTY_SHEET => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.filesystem.get_property_sheet(path).await;

                encode(&ret)?
            }
            API_READ_RANGE => {
                let (path, offset, length): (VfsPath, u64, u64) = decode(&req[..])?;
                let ret = self.filesystem.read_range(path, offset, length).await;

                encode(&ret)?
            }
            API_OPEN_READ_AT => {
                let path: VfsPath = decode(&req[..])?;
                let ret: Result<StreamId, Error> = self
                    .filesystem
                    .open_read_at(path)
                    .await
                    .map(|reader| self.read_at_sessions.open(reader));

                encode(&ret)?
            }
            API_READ_AT => {
                let (stream_id, offset, len): (StreamId, u64, u64) = decode(&req[..])?;
                let ret = self.read_at_sessions.read_at(stream_id, offset, len).await;

                encode(&ret)?
            }
            API_READ_FILE => {
                let (path, max_size): (VfsPath, u64) = decode(&req[..])?;
                let ret = self.filesystem.read_file(path, max_size).await;

                encode(&ret.map(serde_bytes::ByteBuf::from))?
            }
            API_WRITE_FILE => {
                let (path, data): (VfsPath, serde_bytes::ByteBuf) = decode(&req[..])?;
                let ret = self.filesystem.write_file(path, data.into_vec()).await;

                encode(&ret)?
            }
            API_FIND_IN_FILE => {
                let (path, offset, pattern, max_length): (
                    VfsPath,
                    u64,
                    crate::find::SearchPattern,
                    u64,
                ) = decode(&req[..])?;
                let ret = self
                    .filesystem
                    .find_in_file(path, offset, pattern, max_length)
                    .await;

                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == API_READ_AT_CLOSE {
            let stream_id: StreamId = decode(&req[..])?;
            self.read_at_sessions.close(stream_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub struct ShellServiceDispatcher {
    shell_service: Box<dyn ShellService>,
}

impl ShellServiceDispatcher {
    pub fn new<S: ShellService + 'static>(shell_service: S) -> Self {
        Self {
            shell_service: Box::new(shell_service),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for ShellServiceDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_SHELL_EXPAND => {
                let input: String = decode(&req[..])?;
                let ret = self.shell_service.shell_expand(input).await;
                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}

pub struct TerminalDispatcher {
    terminal: Box<dyn TerminalClient>,
}

impl TerminalDispatcher {
    pub fn new<T: TerminalClient + 'static>(terminal: T) -> Self {
        Self {
            terminal: Box::new(terminal),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for TerminalDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_TERMINAL_CREATE => {
                let options: crate::terminal::TerminalOptions = decode(&req[..])?;
                let ret = self.terminal.create(options).await;

                encode(&ret)?
            }
            API_TERMINAL_KILL => {
                let handle: crate::terminal::TerminalHandle = decode(&req[..])?;
                let ret = self.terminal.kill(handle).await;

                encode(&ret)?
            }
            API_TERMINAL_READ => {
                let handle: crate::terminal::TerminalHandle = decode(&req[..])?;
                let ret = self.terminal.read(handle).await;

                encode(&ret.map(|data| data.map(serde_bytes::ByteBuf::from)))?
            }
            API_TERMINAL_WAIT => {
                let handle: crate::terminal::TerminalHandle = decode(&req[..])?;
                let ret = self.terminal.wait(handle).await;

                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        match api {
            API_TERMINAL_INPUT => {
                let (handle, input): (crate::terminal::TerminalHandle, serde_bytes::ByteBuf) =
                    decode(&req[..])?;
                if let Err(e) = self.terminal.input(handle, input.into_vec()).await {
                    log::error!("terminal input failed: {}", e);
                }
                Ok(true)
            }
            API_TERMINAL_RESIZE => {
                let (handle, cols, rows): (crate::terminal::TerminalHandle, u16, u16) =
                    decode(&req[..])?;
                if let Err(e) = self.terminal.resize(handle, cols, rows).await {
                    log::error!("terminal resize failed: {}", e);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

pub struct OperationDispatcher {
    outbox: Outbox,
    operations: Arc<Mutex<HashMap<OperationId, OperationHandle>>>,
    next_issue_id: Arc<AtomicU64>,
    context: Arc<operation::OperationContext>,
}

impl OperationDispatcher {
    pub fn new(outbox: Outbox, context: Arc<operation::OperationContext>) -> Self {
        Self {
            outbox,
            operations: Arc::new(Mutex::new(HashMap::new())),
            next_issue_id: Arc::new(AtomicU64::new(1)),
            context,
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for OperationDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        match api {
            API_START_OPERATION => {
                let request: StartOperationRequest = decode(&req[..])?;
                let handle = OperationHandle {
                    cancel: CancellationToken::new(),
                    issue_resolvers: Arc::new(Mutex::new(HashMap::new())),
                };
                let cancel = handle.cancel.clone();
                let issue_resolvers = handle.issue_resolvers.clone();
                self.operations.lock().insert(request.id, handle);

                let outbox = self.outbox.clone();
                let operations = self.operations.clone();
                let next_issue_id = self.next_issue_id.clone();
                let id = request.id;

                // Bridge the progress channel to the RPC outbox.
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::unbounded_channel::<operation::OperationProgress>();

                let outbox_for_bridge = outbox.clone();
                tokio::spawn(async move {
                    while let Some(progress) = progress_rx.recv().await {
                        if let Some(bytes) = try_encode(&progress) {
                            let _ = outbox_for_bridge
                                .send(Message::Notify(API_OPERATION_PROGRESS, bytes.into()))
                                .await;
                        }
                    }
                });

                let context = self.context.clone();
                tokio::spawn(async move {
                    operation::execute_operation(
                        id,
                        request.request,
                        progress_tx,
                        cancel,
                        issue_resolvers,
                        next_issue_id,
                        context,
                    )
                    .await;
                    operations.lock().remove(&id);
                });

                let ret: Result<(), Error> = Ok(());
                Ok(Some(encode(&ret)?.into()))
            }
            API_CANCEL_OPERATION => {
                let id: OperationId = decode(&req[..])?;
                if let Some(handle) = self.operations.lock().get(&id) {
                    handle.cancel.cancel();
                }

                let ret: Result<(), Error> = Ok(());
                Ok(Some(encode(&ret)?.into()))
            }
            API_RESOLVE_ISSUE => {
                let request: ResolveIssueRequest = decode(&req[..])?;
                if let Some(handle) = self.operations.lock().get(&request.operation_id)
                    && let Some(sender) = handle.issue_resolvers.lock().remove(&request.issue_id)
                {
                    let _ = sender.send(request.response);
                }

                let ret: Result<(), Error> = Ok(());
                Ok(Some(encode(&ret)?.into()))
            }
            _ => Ok(None),
        }
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}

pub struct EnricherDispatcher {
    enrichers: Arc<crate::enrich::Enrichers>,
    outbox: Outbox,
}

impl EnricherDispatcher {
    pub fn new(outbox: Outbox, enrichers: Arc<crate::enrich::Enrichers>) -> Self {
        Self { enrichers, outbox }
    }
}

#[async_trait::async_trait]
impl Dispatcher for EnricherDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        match api {
            API_START_ENRICHMENT => {
                let (id, path, scope, enrichers): (
                    crate::enrich::EnrichmentId,
                    VfsPath,
                    crate::enrich::EnrichScope,
                    Vec<String>,
                ) = decode(&req[..])?;

                let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::enrich::EnrichmentEvent>(16);
                let enrichment = self.enrichers.enrich(path, scope, enrichers, tx);
                tokio::pin!(enrichment);
                let ret = loop {
                    tokio::select! {
                        ret = &mut enrichment => break ret,
                        event = rx.recv() => {
                            let Some(event) = event else {
                                break (&mut enrichment).await;
                            };
                            if let Some(bytes) = try_encode(&(id, event)) {
                                self.outbox
                                    .send(Message::Notify(API_ENRICHMENT_EVENT, bytes.into()))
                                    .await
                                    .map_err(|_| Error::connection())?;
                            }
                        }
                    }
                };
                while let Some(event) = rx.recv().await {
                    if let Some(bytes) = try_encode(&(id, event)) {
                        self.outbox
                            .send(Message::Notify(API_ENRICHMENT_EVENT, bytes.into()))
                            .await
                            .map_err(|_| Error::connection())?;
                    }
                }

                Ok(Some(encode(&ret)?.into()))
            }
            _ => Ok(None),
        }
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}

pub struct VfsMountDispatcher {
    vfs_manager: Box<dyn VfsManager>,
}

impl VfsMountDispatcher {
    pub fn new<V: VfsManager + 'static>(vfs_manager: V) -> Self {
        Self {
            vfs_manager: Box::new(vfs_manager),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsMountDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_MOUNT_VFS => {
                let request: MountRequest = decode(&req[..])?;
                let ret = self.vfs_manager.mount(request).await;
                encode(&ret)?
            }
            API_UNMOUNT_VFS => {
                let vfs_id: VfsId = decode(&req[..])?;
                let ret = self.vfs_manager.unmount(vfs_id).await;
                encode(&ret)?
            }
            API_REMOUNT_VFS => {
                let (vfs_id, mount_meta): (VfsId, Option<Vec<u8>>) = decode(&req[..])?;
                let ret = self.vfs_manager.remount(vfs_id, mount_meta).await;
                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}

pub struct DiscoveryDispatcher {
    provider: Box<dyn crate::discovery::DiscoveryProvider>,
}

impl DiscoveryDispatcher {
    pub fn new<P: crate::discovery::DiscoveryProvider + 'static>(provider: P) -> Self {
        Self {
            provider: Box::new(provider),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for DiscoveryDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_DISCOVER_SSH_HOSTS => {
                let _: () = decode(&req[..])?;
                encode(&self.provider.ssh_hosts().await)?
            }
            API_DISCOVER_CONTAINERS => {
                let engine: String = decode(&req[..])?;
                encode(&self.provider.containers(engine).await)?
            }
            API_DISCOVER_KUBE_CONTEXTS => {
                let _: () = decode(&req[..])?;
                encode(&self.provider.kube_contexts().await)?
            }
            API_DISCOVER_KUBE_PODS => {
                let (context, namespace): (Option<String>, Option<String>) = decode(&req[..])?;
                encode(&self.provider.kube_pods(context, namespace).await)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}

pub struct HotPathsDispatcher {
    provider: Box<dyn HotPathsProvider>,
}

impl HotPathsDispatcher {
    pub fn new<P: HotPathsProvider + 'static>(provider: P) -> Self {
        Self {
            provider: Box::new(provider),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for HotPathsDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_SYSTEM_HOT_PATHS => {
                let _: () = decode(&req[..])?;
                let ret = self.provider.system_hot_paths().await;
                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}

#[cfg(test)]
mod filesystem_read_at_tests {
    use super::*;
    use crate::rpc::{Communicator, Dispatcher};
    use crate::test_support::mock_vfs::MockVfs;
    use crate::vfs::VfsRegistry;

    #[tokio::test]
    async fn dispatcher_serves_read_at_sessions_and_close_reaps() {
        let registry = Arc::new(VfsRegistry::with_root(
            MockVfs::builder().file("/f", b"hello world").build(),
        ));
        let (outbox, _outbox_rx) = Communicator::create_outbox();
        let dispatcher =
            FilesystemDispatcher::new(crate::vfs::VfsRegistryFs::new(registry), outbox);

        let path = VfsPath::from_wire_str(VfsId::ROOT, "/f");
        let response = dispatcher
            .invoke(API_OPEN_READ_AT, encode(&path).unwrap().into())
            .await
            .unwrap()
            .unwrap();
        let stream_id: Result<StreamId, Error> = decode(&response).unwrap();
        let stream_id = stream_id.unwrap();
        assert!(dispatcher.read_at_sessions.contains(stream_id));

        let response = dispatcher
            .invoke(
                API_READ_AT,
                encode(&(stream_id, 6u64, 5u64)).unwrap().into(),
            )
            .await
            .unwrap()
            .unwrap();
        let data: Result<serde_bytes::ByteBuf, Error> = decode(&response).unwrap();
        assert_eq!(data.unwrap().as_slice(), b"world");

        dispatcher
            .notify(API_READ_AT_CLOSE, encode(&stream_id).unwrap().into())
            .await
            .unwrap();
        assert!(!dispatcher.read_at_sessions.contains(stream_id));
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::rpc::Communicator;
    use crate::test_support::endless_listing::EndlessListing;

    #[tokio::test]
    async fn cancelling_streaming_listing_drops_blocking_producer_receiver() {
        let (stopped_tx, stopped_rx) = tokio::sync::oneshot::channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (outbox, _outbox_rx) = Communicator::create_outbox();
        let dispatcher = FilesystemDispatcher::new(
            EndlessListing {
                started: parking_lot::Mutex::new(Some(started_tx)),
                stopped: parking_lot::Mutex::new(Some(stopped_tx)),
            },
            outbox,
        );
        let path = VfsPath::root(VfsId(0));
        let request = encode(&(path, ListFilesOptions { strict: true }, StreamId(1))).unwrap();

        let invoke = tokio::spawn(async move {
            dispatcher
                .invoke(API_LIST_FILES_STREAMING, request.into())
                .await
        });
        started_rx.await.unwrap();
        invoke.abort();

        tokio::time::timeout(std::time::Duration::from_secs(1), stopped_rx)
            .await
            .expect("blocking listing producer survived invoke cancellation")
            .unwrap();
    }
}
