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
pub const API_RENAME: Api = Api(2);
pub const API_CREATE_DIRECTORY: Api = Api(3);
// api 4 missing
pub const API_TOUCH: Api = Api(5);
pub const API_SHELL_EXPAND: Api = Api(6);
pub const API_LIST_FILES_STREAMING: Api = Api(7);
pub const API_LIST_FILES_BATCH: Api = Api(8);

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

pub const API_MOUNT_VFS: Api = Api(400);
pub const API_UNMOUNT_VFS: Api = Api(401);

pub const API_SYSTEM_HOT_PATHS: Api = Api(500);

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

// Host UI APIs — invoked by the agent, handled by the Tauri host.
pub const API_HOST_ASKPASS: Api = Api(624);

// ---------------------------------------------------------------------------
// bincode helpers — propagate decode/encode failures as structured errors so
// a malformed payload (deliberately bad agent, version skew, …) doesn't crash
// the whole process.
// ---------------------------------------------------------------------------

fn decode<'a, T: serde::Deserialize<'a>>(req: &'a [u8]) -> Result<T, Error> {
    bincode::deserialize(req).map_err(|e| Error::custom(format!("RPC decode: {}", e)))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    bincode::serialize(value).map_err(|e| Error::custom(format!("RPC encode: {}", e)))
}

/// Best-effort encode used by streaming notifications: there's no Result to
/// propagate from a spawned task, so failures (which never happen in
/// practice for these types) are logged and the notification is dropped.
fn try_encode<T: serde::Serialize>(value: &T) -> Option<Vec<u8>> {
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

                // Spawn a forwarder task: batches → Notify messages
                let outbox = self.outbox.clone();
                let forwarder = tokio::spawn(async move {
                    while let Some(file_list) = batch_rx.recv().await {
                        if let Some(bytes) = try_encode(&(stream_id, file_list)) {
                            let _ = outbox
                                .send(Message::Notify(API_LIST_FILES_BATCH, bytes.into()))
                                .await;
                        }
                    }
                });

                let ret = self.filesystem.list_files(path, opts, Some(batch_tx)).await;

                // Ensure all batch notifications are sent before returning the response
                let _ = forwarder.await;

                encode(&ret)?
            }
            API_RENAME => {
                let (old_path, new_path): (VfsPath, VfsPath) = decode(&req[..])?;
                let ret = self.filesystem.rename(old_path, new_path).await;

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
            API_TERMINAL_RESIZE => {
                let (handle, cols, rows): (crate::terminal::TerminalHandle, u16, u16) =
                    decode(&req[..])?;
                let ret = self.terminal.resize(handle, cols, rows).await;

                encode(&ret)?
            }
            API_TERMINAL_INPUT => {
                let (handle, input): (crate::terminal::TerminalHandle, Vec<u8>) = decode(&req[..])?;
                let ret = self.terminal.input(handle, input).await;

                encode(&ret)?
            }
            API_TERMINAL_READ => {
                let handle: crate::terminal::TerminalHandle = decode(&req[..])?;
                let ret = self.terminal.read(handle).await;

                encode(&ret)?
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

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
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
            API_READ_RANGE => {
                let (path, offset, length): (VfsPath, u64, u64) = decode(&req[..])?;
                let ret = self.file_reader.read_range(path, offset, length).await;

                encode(&ret)?
            }
            API_READ_FILE => {
                let (path, max_size): (VfsPath, u64) = decode(&req[..])?;
                let ret = self.file_reader.read_file(path, max_size).await;

                encode(&ret)?
            }
            API_WRITE_FILE => {
                let (path, data): (VfsPath, Vec<u8>) = decode(&req[..])?;
                let ret = self.file_reader.write_file(path, data).await;

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

                // Create a progress channel and bridge it to the RPC outbox
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::unbounded_channel::<operation::OperationProgress>();

                // Spawn a task to forward progress to the RPC outbox
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

// ---------------------------------------------------------------------------
// VfsRegistryManager — local VfsManager backed by a VfsRegistry
// ---------------------------------------------------------------------------

pub struct ReadStream {
    pub tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub expected_seq: u64,
}

pub type PendingVfsReadStreams = Arc<parking_lot::Mutex<HashMap<StreamId, ReadStream>>>;

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
}

impl VfsRegistryManager {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self {
            registry,
            host_communicator: Arc::new(std::sync::OnceLock::new()),
            pending_read_streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            sftp_askpass: None,
            askpass_provider: None,
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
}

#[async_trait::async_trait]
impl VfsManager for VfsRegistryManager {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error> {
        let ctx = MountContext {
            registry: &self.registry,
            host_communicator: &self.host_communicator,
            pending_read_streams: &self.pending_read_streams,
            sftp_askpass: self.sftp_askpass.as_ref(),
            askpass_provider: self.askpass_provider.as_ref(),
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
            MountRequest::Archive { origin } => crate::vfs::archive::mount(origin, &ctx).await?,
        };

        let mount_meta = vfs.mount_meta();
        let type_name = vfs.descriptor().type_name().to_string();
        let origin = vfs.origin().cloned();
        let vfs_id = self.registry.mount(vfs);
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

// ---------------------------------------------------------------------------
// VfsDispatcher — handles API_HOST_VFS_* invoke requests from the agent
// ---------------------------------------------------------------------------

use crate::vfs::VFS_READ_CHUNK_SIZE;

struct WriteSession {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    expected_seq: u64,
}

type PendingVfsWriteSessions = Arc<parking_lot::Mutex<HashMap<StreamId, WriteSession>>>;

/// Shared state for write sessions, accessible from both invoke and notify
/// handlers. The JoinHandle map lets the FINISH invoke await the writer task.
type WriteTaskHandles =
    Arc<parking_lot::Mutex<HashMap<StreamId, tokio::task::JoinHandle<Result<(), Error>>>>>;

pub struct VfsDispatcher {
    vfs: Arc<dyn Vfs>,
    outbox: Outbox,
    write_sessions: PendingVfsWriteSessions,
    write_task_handles: WriteTaskHandles,
    next_stream_id: AtomicU64,
}

impl VfsDispatcher {
    pub fn new(vfs: Arc<dyn Vfs>, outbox: Outbox) -> Self {
        Self {
            vfs,
            outbox,
            write_sessions: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            write_task_handles: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            next_stream_id: AtomicU64::new(1),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        use std::path::PathBuf;

        let ret = match api {
            API_HOST_VFS_LIST_FILES => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.list_files(&path, None).await;
                encode(&ret)?
            }
            API_HOST_VFS_POLL_CHANGES => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.poll_changes(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_FS_STATS => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.fs_stats(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_OPEN_READ_ASYNC => {
                let (path, stream_id): (PathBuf, StreamId) = decode(&req[..])?;
                let descriptor = self.vfs.descriptor();
                let outbox = self.outbox.clone();

                let ret: Result<(), Error> = if descriptor.can_read_async() {
                    use tokio::io::AsyncReadExt;
                    let mut reader = self.vfs.open_read_async(&path).await?;
                    let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
                    let mut seq: u64 = 0;
                    loop {
                        let n = reader.read(&mut buf).await.map_err(Error::from)?;
                        if n == 0 {
                            break;
                        }
                        let chunk = buf[..n].to_vec();
                        if let Some(bytes) = try_encode(&(stream_id, seq, chunk)) {
                            let _ = outbox
                                .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                                .await;
                        }
                        seq += 1;
                    }
                    // Send empty sentinel to signal EOF.
                    if let Some(bytes) = try_encode(&(stream_id, seq, Vec::<u8>::new())) {
                        let _ = outbox
                            .send(Message::Notify(API_HOST_VFS_READ_CHUNK, bytes.into()))
                            .await;
                    }
                    Ok(())
                } else if descriptor.can_read_sync() {
                    let mut reader = self.vfs.open_read_sync(&path).await?;
                    let outbox = outbox.clone();
                    tokio::task::spawn_blocking(move || {
                        use std::io::Read;
                        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
                        let mut seq: u64 = 0;
                        loop {
                            let n = reader.read(&mut buf)?;
                            if n == 0 {
                                break;
                            }
                            let chunk = buf[..n].to_vec();
                            if let Some(bytes) = try_encode(&(stream_id, seq, chunk)) {
                                let _ = outbox.blocking_send_low(Message::Notify(
                                    API_HOST_VFS_READ_CHUNK,
                                    bytes.into(),
                                ));
                            }
                            seq += 1;
                        }
                        // Send empty sentinel to signal EOF.
                        if let Some(bytes) = try_encode(&(stream_id, seq, Vec::<u8>::new())) {
                            let _ = outbox.blocking_send_low(Message::Notify(
                                API_HOST_VFS_READ_CHUNK,
                                bytes.into(),
                            ));
                        }
                        Ok::<(), Error>(())
                    })
                    .await?
                } else {
                    Err(Error::not_supported())
                };

                encode(&ret)?
            }
            API_HOST_VFS_READ_RANGE => {
                let (path, offset, length): (PathBuf, u64, u64) = decode(&req[..])?;
                let ret = self.vfs.read_range(&path, offset, length).await;
                encode(&ret)?
            }
            API_HOST_VFS_FILE_DETAILS => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.file_details(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_FILE_INFO => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.file_info(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_OVERWRITE_ASYNC_BEGIN => {
                let path: PathBuf = decode(&req[..])?;
                let descriptor = self.vfs.descriptor();

                let ret: Result<StreamId, Error> = if descriptor.can_overwrite_async() {
                    let writer = self.vfs.overwrite_async(&path).await?;
                    let stream_id = StreamId(
                        self.next_stream_id
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                    );

                    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
                    self.write_sessions.lock().insert(
                        stream_id,
                        WriteSession {
                            tx: chunk_tx,
                            expected_seq: 0,
                        },
                    );

                    let write_task_handles = self.write_task_handles.clone();
                    let write_sessions = self.write_sessions.clone();
                    let handle = tokio::spawn(async move {
                        let mut writer = writer;
                        while let Some(data) = chunk_rx.recv().await {
                            writer.write(&data).await?;
                        }
                        writer.finish().await?;
                        write_sessions.lock().remove(&stream_id);
                        Ok(())
                    });
                    // Update abort handle and store JoinHandle for FINISH to await.
                    write_task_handles.lock().insert(stream_id, handle);

                    Ok(stream_id)
                } else if descriptor.can_overwrite_sync() {
                    let writer = self.vfs.overwrite_sync(&path).await?;
                    let stream_id = StreamId(
                        self.next_stream_id
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                    );

                    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
                    self.write_sessions.lock().insert(
                        stream_id,
                        WriteSession {
                            tx: chunk_tx,
                            expected_seq: 0,
                        },
                    );

                    let write_task_handles = self.write_task_handles.clone();
                    let write_sessions = self.write_sessions.clone();
                    let handle = tokio::task::spawn_blocking(move || {
                        use std::io::Write;
                        let mut writer = writer;
                        while let Some(data) =
                            tokio::runtime::Handle::current().block_on(chunk_rx.recv())
                        {
                            writer.write_all(&data)?;
                        }
                        drop(writer); // flush on drop
                        write_sessions.lock().remove(&stream_id);
                        Ok(())
                    });
                    write_task_handles.lock().insert(stream_id, handle);

                    Ok(stream_id)
                } else {
                    Err(Error::not_supported())
                };

                encode(&ret)?
            }
            API_HOST_VFS_OVERWRITE_ASYNC_FINISH => {
                let stream_id: StreamId = decode(&req[..])?;
                // The sentinel (empty chunk) already closed the data channel.
                // Wait for the writer task to finish and propagate its result.
                let handle = self.write_task_handles.lock().remove(&stream_id);
                let ret: Result<(), Error> = match handle {
                    Some(h) => match h.await {
                        Ok(r) => r,
                        Err(e) => Err(Error::custom(format!("writer task failed: {}", e))),
                    },
                    None => {
                        // Writer task already finished or was never started.
                        Ok(())
                    }
                };
                encode(&ret)?
            }
            API_HOST_VFS_CREATE_DIRECTORY => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.create_directory(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_CREATE_SYMLINK => {
                let (link, target): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.create_symlink(&link, &target).await;
                encode(&ret)?
            }
            API_HOST_VFS_TOUCH => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.touch(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_TRUNCATE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.truncate(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_REMOVE_FILE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_file(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_REMOVE_DIR => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_dir(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_REMOVE_TREE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.remove_tree(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_GET_METADATA => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.get_metadata(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_SET_METADATA => {
                let (path, meta): (PathBuf, crate::vfs::VfsMetadata) = decode(&req[..])?;
                let ret = self.vfs.set_metadata(&path, &meta).await;
                encode(&ret)?
            }
            API_HOST_VFS_AVAILABLE_SPACE => {
                let path: PathBuf = decode(&req[..])?;
                let ret = self.vfs.available_space(&path).await;
                encode(&ret)?
            }
            API_HOST_VFS_RENAME => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.rename(&from, &to).await;
                encode(&ret)?
            }
            API_HOST_VFS_COPY_WITHIN => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.copy_within(&from, &to).await;
                encode(&ret)?
            }
            API_HOST_VFS_HARD_LINK => {
                let (link, target): (PathBuf, PathBuf) = decode(&req[..])?;
                let ret = self.vfs.hard_link(&link, &target).await;
                encode(&ret)?
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == API_HOST_VFS_WRITE_CHUNK {
            let (stream_id, seq, data): (StreamId, u64, Vec<u8>) = decode(&req[..])?;

            let tx = {
                let mut sessions = self.write_sessions.lock();
                let session = sessions.get_mut(&stream_id);
                match session {
                    Some(session) => {
                        assert!(
                            seq == session.expected_seq,
                            "VFS write chunk out of order for stream {:?}: expected seq {}, got {}",
                            stream_id,
                            session.expected_seq,
                            seq,
                        );
                        session.expected_seq += 1;

                        if data.is_empty() {
                            // Sentinel — remove session to close the channel.
                            sessions.remove(&stream_id);
                            None
                        } else {
                            Some(session.tx.clone())
                        }
                    }
                    None => None,
                }
            };
            if let Some(tx) = tx {
                let _ = tx.send(data).await;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// VfsReadChunkDispatcher — agent-side: routes read-chunk notifications
// from the host into the correct RemoteVfs stream.
// ---------------------------------------------------------------------------

pub struct VfsReadChunkDispatcher {
    pending_read_streams: PendingVfsReadStreams,
}

impl VfsReadChunkDispatcher {
    pub fn new(pending_read_streams: PendingVfsReadStreams) -> Self {
        Self {
            pending_read_streams,
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsReadChunkDispatcher {
    async fn invoke(&self, _api: Api, _req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        Ok(None)
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == API_HOST_VFS_READ_CHUNK {
            let (stream_id, seq, data): (StreamId, u64, Vec<u8>) = decode(&req[..])?;

            let tx = {
                let mut streams = self.pending_read_streams.lock();
                let stream = streams.get_mut(&stream_id);
                match stream {
                    Some(stream) => {
                        assert!(
                            seq == stream.expected_seq,
                            "VFS read chunk out of order for stream {:?}: expected seq {}, got {}",
                            stream_id,
                            stream.expected_seq,
                            seq,
                        );
                        stream.expected_seq += 1;
                        let tx = stream.tx.clone();

                        if data.is_empty() {
                            // Sentinel — remove from map so the channel closes
                            // after this send (the tx clone is the last sender).
                            streams.remove(&stream_id);
                        }
                        Some(tx)
                    }
                    None => None,
                }
            };
            if let Some(tx) = tx {
                // Send the chunk (or empty sentinel) — the reader distinguishes.
                let _ = tx.send(data).await;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
