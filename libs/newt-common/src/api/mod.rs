use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use std::sync::atomic::AtomicU64;

use crate::{
    Error,
    file_reader::FileReader,
    filesystem::{FileList, Filesystem, ListFilesOptions, ShellService, StreamId},
    hot_paths::HotPathsProvider,
    operation::{self, OperationHandle, OperationId, ResolveIssueRequest, StartOperationRequest},
    rpc::{Api, Dispatcher, Message, Outbox},
    terminal::TerminalClient,
    vfs::{MountRequest, MountResponse, Vfs, VfsId, VfsManager, VfsPath, VfsRegistry},
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

pub const API_MOUNT_VFS: Api = Api(400);
pub const API_UNMOUNT_VFS: Api = Api(401);
pub const API_VFS_PROGRESS: Api = Api(402);

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

// Host VFS APIs — invoked by the agent, handled by the Tauri host.
// Used by RemoteVfs to access the client-local filesystem.
pub const API_HOST_VFS_LIST_FILES: Api = Api(600);
pub const API_HOST_VFS_POLL_CHANGES: Api = Api(601);
pub const API_HOST_VFS_FS_STATS: Api = Api(602);
pub const API_HOST_VFS_OPEN_READ_ASYNC: Api = Api(603);
pub const API_HOST_VFS_READ_CHUNK: Api = Api(621);
pub const API_HOST_VFS_READ_RANGE: Api = Api(604);
pub const API_HOST_VFS_FILE_DETAILS: Api = Api(605);
pub const API_HOST_VFS_FILE_INFO: Api = Api(606);
pub const API_HOST_VFS_OVERWRITE_ASYNC_BEGIN: Api = Api(607);
pub const API_HOST_VFS_WRITE_CHUNK: Api = Api(622);
pub const API_HOST_VFS_OVERWRITE_ASYNC_FINISH: Api = Api(623);
pub const API_HOST_VFS_OVERWRITE_ASYNC_ABORT: Api = Api(629);
pub const API_HOST_VFS_CREATE_DIRECTORY: Api = Api(608);
pub const API_HOST_VFS_CREATE_SYMLINK: Api = Api(609);
pub const API_HOST_VFS_TOUCH: Api = Api(610);
pub const API_HOST_VFS_TRUNCATE: Api = Api(611);
pub const API_HOST_VFS_REMOVE_FILE: Api = Api(612);
pub const API_HOST_VFS_REMOVE_DIR: Api = Api(613);
pub const API_HOST_VFS_REMOVE_TREE: Api = Api(614);
pub const API_HOST_VFS_GET_METADATA: Api = Api(615);
pub const API_HOST_VFS_SET_METADATA: Api = Api(616);
pub const API_HOST_VFS_AVAILABLE_SPACE: Api = Api(617);
pub const API_HOST_VFS_RENAME: Api = Api(618);
pub const API_HOST_VFS_COPY_WITHIN: Api = Api(619);
pub const API_HOST_VFS_HARD_LINK: Api = Api(620);
pub const API_HOST_VFS_TRASH_ITEM: Api = Api(628);

// Host UI APIs — invoked by the agent, handled by the Tauri host.
pub const API_HOST_ASKPASS: Api = Api(624);

// Agent-binary provisioning — invoked by the agent (nested spawns for
// pane-scoped agent mounts), served from the host's agents dir.
pub const API_HOST_AGENT_HASH: Api = Api(625);
pub const API_HOST_FETCH_AGENT: Api = Api(626);
pub const API_HOST_FETCH_AGENT_CHUNK: Api = Api(627);
pub const API_HOST_FETCH_AGENT_CANCEL: Api = Api(630);

mod vfs;
pub use vfs::{VfsDispatcher, VfsReadChunkDispatcher};

// bincode helpers — propagate decode/encode failures as structured errors so
// a malformed payload (bad agent, version skew, …) doesn't crash the process.

pub(super) fn decode<'a, T: serde::Deserialize<'a>>(req: &'a [u8]) -> Result<T, Error> {
    bincode::deserialize(req).map_err(|e| Error::custom(format!("RPC decode: {}", e)))
}

pub(super) fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Error> {
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

pub struct FilesystemDispatcher {
    filesystem: Box<dyn Filesystem>,
    outbox: Outbox,
}

impl FilesystemDispatcher {
    pub fn new<F: Filesystem + 'static>(filesystem: F, outbox: Outbox) -> Self {
        Self {
            filesystem: Box::new(filesystem),
            outbox,
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
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
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

pub struct FileReaderDispatcher {
    file_reader: Box<dyn FileReader>,
}

impl FileReaderDispatcher {
    pub fn new<F: FileReader + 'static>(file_reader: F) -> Self {
        Self {
            file_reader: Box::new(file_reader),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for FileReaderDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_FILE_DETAILS => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.file_reader.file_details(path).await;

                encode(&ret)?
            }
            API_GET_PROPERTY_SHEET => {
                let path: VfsPath = decode(&req[..])?;
                let ret = self.file_reader.get_property_sheet(path).await;

                encode(&ret)?
            }
            API_READ_RANGE => {
                let (path, offset, length): (VfsPath, u64, u64) = decode(&req[..])?;
                let ret = self.file_reader.read_range(path, offset, length).await;

                encode(&ret)?
            }
            API_READ_FILE => {
                let (path, max_size): (VfsPath, u64) = decode(&req[..])?;
                let ret = self.file_reader.read_file(path, max_size).await;

                encode(&ret.map(serde_bytes::ByteBuf::from))?
            }
            API_WRITE_FILE => {
                let (path, data): (VfsPath, serde_bytes::ByteBuf) = decode(&req[..])?;
                let ret = self.file_reader.write_file(path, data.into_vec()).await;

                encode(&ret)?
            }
            API_FIND_IN_FILE => {
                let (path, offset, pattern, max_length): (
                    VfsPath,
                    u64,
                    crate::file_reader::SearchPattern,
                    u64,
                ) = decode(&req[..])?;
                let ret = self
                    .file_reader
                    .find_in_file(path, offset, pattern, max_length)
                    .await;

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

// ---------------------------------------------------------------------------
// VfsRegistryManager — local VfsManager backed by a VfsRegistry
// ---------------------------------------------------------------------------

pub struct ReadStream {
    pub tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub expected_seq: u64,
}

pub type PendingVfsReadStreams = Arc<parking_lot::Mutex<HashMap<StreamId, ReadStream>>>;

/// Response header for `API_HOST_FETCH_AGENT`. The bytes follow as
/// sequenced `API_HOST_FETCH_AGENT_CHUNK` notifications (empty sentinel =
/// EOF); the consumer validates the received byte count against `size`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentFetchHeader {
    pub size: u64,
    pub raw_size: u64,
    pub encoding: crate::agent_resolver::AgentEncoding,
}

/// Serves the host's agent binaries to a session agent: the content hash
/// (`API_HOST_AGENT_HASH`) and streamed binaries for nested spawns
/// (`API_HOST_FETCH_AGENT`).
pub struct AgentFetchDispatcher {
    resolver: Arc<dyn crate::agent_resolver::AgentResolver>,
    outbox: Outbox,
    fetches: Arc<Mutex<HashMap<StreamId, CancellationToken>>>,
}

impl AgentFetchDispatcher {
    pub fn new(resolver: Arc<dyn crate::agent_resolver::AgentResolver>, outbox: Outbox) -> Self {
        Self {
            resolver,
            outbox,
            fetches: Default::default(),
        }
    }
}

struct FetchRegistration {
    stream_id: StreamId,
    fetches: Arc<Mutex<HashMap<StreamId, CancellationToken>>>,
}

impl Drop for FetchRegistration {
    fn drop(&mut self) {
        self.fetches.lock().remove(&self.stream_id);
    }
}

#[async_trait::async_trait]
impl Dispatcher for AgentFetchDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_HOST_AGENT_HASH => {
                let ret = self.resolver.agent_hash().await;
                encode(&ret)?
            }
            API_HOST_FETCH_AGENT => {
                let (triple, accept_gzip, stream_id): (String, bool, StreamId) = decode(&req[..])?;
                // Register before opening the stream: a caller can cancel the
                // FETCH invoke while open_agent_binary is still pending.
                let cancel = CancellationToken::new();
                self.fetches.lock().insert(stream_id, cancel.clone());
                let registration = FetchRegistration {
                    stream_id,
                    fetches: self.fetches.clone(),
                };
                let ret: Result<AgentFetchHeader, Error> = match self
                    .resolver
                    .open_agent_binary(&triple, accept_gzip)
                    .await
                {
                    Ok(mut stream) => {
                        let header = AgentFetchHeader {
                            size: stream.size,
                            raw_size: stream.raw_size,
                            encoding: stream.encoding,
                        };
                        let outbox = self.outbox.clone();
                        tokio::spawn(async move {
                            let _registration = registration;
                            use tokio::io::AsyncReadExt;
                            let mut seq: u64 = 0;
                            let mut buf = vec![0u8; crate::vfs::VFS_READ_CHUNK_SIZE];
                            loop {
                                let read = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => return,
                                    read = stream.reader.read(&mut buf) => read,
                                };
                                match read {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        if let Some(bytes) = try_encode(&(
                                            stream_id,
                                            seq,
                                            serde_bytes::Bytes::new(&buf[..n]),
                                        )) {
                                            let send = outbox.send(Message::Notify(
                                                API_HOST_FETCH_AGENT_CHUNK,
                                                bytes.into(),
                                            ));
                                            tokio::select! {
                                                biased;
                                                _ = cancel.cancelled() => return,
                                                result = send => {
                                                    if result.is_err() {
                                                        return;
                                                    }
                                                }
                                            }
                                        }
                                        seq += 1;
                                    }
                                    Err(e) => {
                                        // Cut the stream short; the
                                        // consumer's size check turns
                                        // this into a hard error.
                                        log::error!("agent fetch read failed: {}", e);
                                        break;
                                    }
                                }
                            }
                            if cancel.is_cancelled() {
                                return;
                            }
                            if let Some(bytes) =
                                try_encode(&(stream_id, seq, serde_bytes::Bytes::new(&[])))
                            {
                                let _ = outbox
                                    .send(Message::Notify(API_HOST_FETCH_AGENT_CHUNK, bytes.into()))
                                    .await;
                            }
                        });
                        Ok(header)
                    }
                    Err(e) => {
                        drop(registration);
                        Err(e)
                    }
                };
                encode(&ret)?
            }
            _ => return Ok(None),
        };
        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == API_HOST_FETCH_AGENT_CANCEL {
            let stream_id: StreamId = decode(&req[..])?;
            if let Some(cancel) = self.fetches.lock().remove(&stream_id) {
                cancel.cancel();
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Shared state passed to per-VFS `mount` helpers. Bundles the registry
/// (needed by archive mounts to resolve their upstream), the host
/// communicator and pending-stream map (needed by the Remote VFS), the
/// SFTP askpass configuration (binary path + provider, used by SFTP),
/// and a generic askpass provider (used by encrypted-archive mounts).
/// Any VFS may ignore fields it doesn't need.
pub struct MountContext<'a> {
    pub registry: &'a VfsRegistry,
    pub host_communicator: &'a std::sync::OnceLock<crate::rpc::Communicator>,
    pub pending_read_streams: &'a PendingVfsReadStreams,
    pub sftp_askpass: Option<&'a SftpAskpass>,
    pub askpass_provider: Option<&'a Arc<dyn crate::askpass::AskpassProvider>>,
    /// Resolves agent binaries for spawn-style agent mounts (`None` ⇒
    /// such mounts are rejected).
    pub agent_resolver: Option<&'a Arc<dyn crate::agent_resolver::AgentResolver>>,
    /// Extra PATH entries for transport binary resolution on agent mounts.
    pub extra_path: &'a [String],
    /// Per-mount progress reporter, scoped to the `VfsId` the manager
    /// is about to assign to this mount. VFSes that report progress
    /// (e.g. SearchVfs) clone the inner `Arc` and call `report()`
    /// without ever needing to know their own id.
    pub progress_reporter: &'a Arc<dyn crate::vfs::ProgressReporter>,
}

/// Askpass configuration used by SFTP (and any future SSH-spawning VFS).
#[derive(Clone)]
pub struct SftpAskpass {
    /// Path to the agent binary to set as `SSH_ASKPASS` (its
    /// `NEWT_ASKPASS_SOCK` mode connects to the listener spawned for
    /// `provider`).
    pub askpass_binary: std::path::PathBuf,
    pub provider: Arc<dyn crate::askpass::AskpassProvider>,
}

pub struct VfsRegistryManager {
    registry: Arc<VfsRegistry>,
    /// When set, allows mounting a Remote VFS that proxies calls back to
    /// the host. Used by the agent in remote sessions.
    host_communicator: Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
    /// Shared map for routing read-chunk notifications to the correct stream.
    pending_read_streams: PendingVfsReadStreams,
    /// SFTP askpass configuration. When `None`, SFTP mounts inherit the
    /// process environment with no special password handling.
    sftp_askpass: Option<SftpAskpass>,
    /// Generic askpass provider used for prompts that aren't tied to
    /// SFTP's `SSH_ASKPASS` plumbing — currently encrypted-archive
    /// passwords. When set with `with_sftp_askpass`, this is also
    /// populated from the SFTP askpass's provider.
    askpass_provider: Option<Arc<dyn crate::askpass::AskpassProvider>>,
    /// Sink used to build a per-mount `ScopedReporter`. Defaults to a
    /// no-op so manager construction outside of a real session (tests,
    /// agent boot before the outbox is wired, etc.) keeps working.
    progress_sink: Arc<dyn crate::vfs::VfsProgressSink>,
    /// Resolves agent binaries for spawn-style agent mounts. When `None`,
    /// `MountRequest::Agent` is rejected.
    agent_resolver: Option<Arc<dyn crate::agent_resolver::AgentResolver>>,
    /// Extra PATH entries for resolving transport binaries (docker, ssh, …)
    /// on spawn-style agent mounts. Host sessions populate this from
    /// preferences; the agent's ambient PATH is used otherwise.
    extra_path: Vec<String>,
}

impl VfsRegistryManager {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self {
            registry,
            host_communicator: Arc::new(std::sync::OnceLock::new()),
            pending_read_streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            sftp_askpass: None,
            askpass_provider: None,
            progress_sink: Arc::new(crate::vfs::NoopProgressSink),
            agent_resolver: None,
            extra_path: Vec::new(),
        }
    }

    pub fn new_with_host_communicator(
        registry: Arc<VfsRegistry>,
        host_communicator: Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
        pending_read_streams: PendingVfsReadStreams,
    ) -> Self {
        Self {
            registry,
            host_communicator,
            pending_read_streams,
            sftp_askpass: None,
            askpass_provider: None,
            progress_sink: Arc::new(crate::vfs::NoopProgressSink),
            agent_resolver: None,
            extra_path: Vec::new(),
        }
    }

    pub fn with_sftp_askpass(mut self, askpass: SftpAskpass) -> Self {
        // Mirror the provider into the generic slot so encrypted-archive
        // mounts get an askpass for free wherever SFTP already has one.
        if self.askpass_provider.is_none() {
            self.askpass_provider = Some(askpass.provider.clone());
        }
        self.sftp_askpass = Some(askpass);
        self
    }

    pub fn with_askpass_provider(
        mut self,
        provider: Arc<dyn crate::askpass::AskpassProvider>,
    ) -> Self {
        self.askpass_provider = Some(provider);
        self
    }

    pub fn with_progress_sink(mut self, sink: Arc<dyn crate::vfs::VfsProgressSink>) -> Self {
        self.progress_sink = sink;
        self
    }

    pub fn with_agent_resolver(
        mut self,
        resolver: Arc<dyn crate::agent_resolver::AgentResolver>,
    ) -> Self {
        self.agent_resolver = Some(resolver);
        self
    }

    pub fn with_extra_path(mut self, extra_path: Vec<String>) -> Self {
        self.extra_path = extra_path;
        self
    }
}

#[async_trait::async_trait]
impl VfsManager for VfsRegistryManager {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error> {
        // Allocate the id up front so the mount gets a progress reporter
        // already scoped to its final VfsId. The id isn't visible until
        // `insert` below, so a failed mount just leaves it unused.
        let vfs_id = self.registry.allocate_id();
        let progress_reporter: Arc<dyn crate::vfs::ProgressReporter> = Arc::new(
            crate::vfs::ScopedReporter::new(self.progress_sink.clone(), vfs_id),
        );
        let ctx = MountContext {
            registry: &self.registry,
            host_communicator: &self.host_communicator,
            pending_read_streams: &self.pending_read_streams,
            sftp_askpass: self.sftp_askpass.as_ref(),
            askpass_provider: self.askpass_provider.as_ref(),
            agent_resolver: self.agent_resolver.as_ref(),
            extra_path: &self.extra_path,
            progress_reporter: &progress_reporter,
        };

        let vfs: Arc<dyn Vfs> = match request {
            MountRequest::S3 {
                region,
                bucket,
                credentials,
            } => crate::vfs::S3Vfs::mount(region, bucket, credentials, &ctx).await?,
            MountRequest::Sftp { host } => crate::vfs::SftpVfs::mount(host, &ctx).await?,
            MountRequest::Kubernetes { context } => {
                crate::vfs::K8sVfs::mount(context, &ctx).await?
            }
            MountRequest::Remote => crate::vfs::RemoteVfs::mount(&ctx)?,
            MountRequest::Agent { spec, kind, label } => {
                crate::vfs::agent::mount(spec, kind, label, &ctx).await?
            }
            MountRequest::Archive { origin } => crate::vfs::archive::mount(origin, &ctx).await?,
            MountRequest::Disc { origin } => crate::vfs::disc::mount(origin, &ctx).await?,
            MountRequest::Search { root, params } => {
                // Content matching needs a FileReader; use the
                // registry-backed reader so search inside a SearchVfs's
                // source follows registry redirects.
                let file_reader: std::sync::Arc<dyn crate::file_reader::FileReader> =
                    std::sync::Arc::new(crate::vfs::VfsRegistryFileReader::new(
                        self.registry.clone(),
                    ));
                crate::vfs::search::mount(root, params, file_reader, &ctx).await?
            }
        };

        let mount_meta = vfs.mount_meta();
        let type_name = vfs.descriptor().type_name().to_string();
        let origin = vfs.origin().cloned();
        self.registry.insert(vfs_id, vfs);
        log::info!("mounted {} VFS as vfs_id={:?}", type_name, vfs_id);

        Ok(MountResponse {
            vfs_id,
            type_name,
            mount_meta,
            origin,
        })
    }

    async fn unmount(&self, vfs_id: VfsId) -> Result<(), Error> {
        self.registry
            .unmount(vfs_id)
            .map(|_| ())
            .ok_or_else(|| Error::custom(format!("cannot unmount VFS {}", vfs_id)))
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
mod cancellation_tests {
    use super::*;
    use crate::rpc::Communicator;

    struct EndlessListing {
        started: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        stopped: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    #[async_trait::async_trait]
    impl Filesystem for EndlessListing {
        async fn poll_changes(&self, _path: VfsPath) -> Result<(), Error> {
            Ok(())
        }

        async fn list_files(
            &self,
            path: VfsPath,
            _options: ListFilesOptions,
            batch_tx: Option<tokio::sync::mpsc::Sender<FileList>>,
        ) -> Result<FileList, Error> {
            let tx = batch_tx.expect("streaming listing must provide a sender");
            let stopped = self.stopped.lock().take();
            tokio::task::spawn_blocking(move || {
                loop {
                    if tx
                        .blocking_send(FileList::new(path.clone(), Vec::new(), None))
                        .is_err()
                    {
                        if let Some(stopped) = stopped {
                            let _ = stopped.send(());
                        }
                        return;
                    }
                }
            });
            if let Some(started) = self.started.lock().take() {
                let _ = started.send(());
            }
            std::future::pending().await
        }

        async fn touch(&self, _path: VfsPath) -> Result<(), Error> {
            Ok(())
        }

        async fn create_directory(&self, _path: VfsPath) -> Result<(), Error> {
            Ok(())
        }

        async fn revalidate(
            &self,
            _vfs_id: VfsId,
        ) -> Result<crate::vfs::RevalidationOutcome, Error> {
            Ok(crate::vfs::RevalidationOutcome::Fresh)
        }
    }

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
