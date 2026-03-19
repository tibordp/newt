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
    rpc::{Api, Dispatcher, Message},
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
pub const API_HOST_VFS_OPEN_READ_SYNC: Api = Api(603);
pub const API_HOST_VFS_READ_RANGE: Api = Api(604);
pub const API_HOST_VFS_FILE_DETAILS: Api = Api(605);
pub const API_HOST_VFS_FILE_INFO: Api = Api(606);
pub const API_HOST_VFS_OVERWRITE_SYNC: Api = Api(607);
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

pub struct FilesystemDispatcher {
    filesystem: Box<dyn Filesystem>,
    outbox: tokio::sync::mpsc::UnboundedSender<Message>,
}

impl FilesystemDispatcher {
    pub fn new<F: Filesystem + 'static>(
        filesystem: F,
        outbox: tokio::sync::mpsc::UnboundedSender<Message>,
    ) -> Self {
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
                let path: VfsPath = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.poll_changes(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_LIST_FILES => {
                let args: (VfsPath, ListFilesOptions) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.list_files(args.0, args.1, None).await;

                bincode::serialize(&ret).unwrap()
            }
            API_LIST_FILES_STREAMING => {
                let (path, opts, stream_id): (VfsPath, ListFilesOptions, StreamId) =
                    bincode::deserialize(&req[..]).unwrap();

                let (batch_tx, mut batch_rx) = tokio::sync::mpsc::channel::<FileList>(
                    crate::filesystem::LIST_BATCH_CHANNEL_CAPACITY,
                );

                // Spawn a forwarder task: batches → Notify messages
                let outbox = self.outbox.clone();
                let forwarder = tokio::spawn(async move {
                    while let Some(file_list) = batch_rx.recv().await {
                        let bytes = bincode::serialize(&(stream_id, file_list)).unwrap();
                        let _ = outbox.send(Message::Notify(API_LIST_FILES_BATCH, bytes.into()));
                    }
                });

                let ret = self.filesystem.list_files(path, opts, Some(batch_tx)).await;

                // Ensure all batch notifications are sent before returning the response
                let _ = forwarder.await;

                bincode::serialize(&ret).unwrap()
            }
            API_RENAME => {
                let (old_path, new_path): (VfsPath, VfsPath) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.rename(old_path, new_path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_TOUCH => {
                let path: VfsPath = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.touch(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_CREATE_DIRECTORY => {
                let path: VfsPath = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.create_directory(path).await;

                bincode::serialize(&ret).unwrap()
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
                let input: String = bincode::deserialize(&req[..]).unwrap();
                let ret = self.shell_service.shell_expand(input).await;
                bincode::serialize(&ret).unwrap()
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
                let options: crate::terminal::TerminalOptions =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.terminal.create(options).await;

                bincode::serialize(&ret).unwrap()
            }
            API_TERMINAL_KILL => {
                let handle: crate::terminal::TerminalHandle =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.terminal.kill(handle).await;

                bincode::serialize(&ret).unwrap()
            }
            API_TERMINAL_RESIZE => {
                let (handle, cols, rows): (crate::terminal::TerminalHandle, u16, u16) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.terminal.resize(handle, cols, rows).await;

                bincode::serialize(&ret).unwrap()
            }
            API_TERMINAL_INPUT => {
                let (handle, input): (crate::terminal::TerminalHandle, Vec<u8>) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.terminal.input(handle, input).await;

                bincode::serialize(&ret).unwrap()
            }
            API_TERMINAL_READ => {
                let handle: crate::terminal::TerminalHandle =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.terminal.read(handle).await;

                bincode::serialize(&ret).unwrap()
            }
            API_TERMINAL_WAIT => {
                let handle: crate::terminal::TerminalHandle =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.terminal.wait(handle).await;

                bincode::serialize(&ret).unwrap()
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
                let path: VfsPath = bincode::deserialize(&req[..]).unwrap();
                let ret = self.file_reader.file_details(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_READ_RANGE => {
                let (path, offset, length): (VfsPath, u64, u64) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.file_reader.read_range(path, offset, length).await;

                bincode::serialize(&ret).unwrap()
            }
            API_READ_FILE => {
                let (path, max_size): (VfsPath, u64) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.file_reader.read_file(path, max_size).await;

                bincode::serialize(&ret).unwrap()
            }
            API_WRITE_FILE => {
                let (path, data): (VfsPath, Vec<u8>) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.file_reader.write_file(path, data).await;

                bincode::serialize(&ret).unwrap()
            }
            API_FIND_IN_FILE => {
                let (path, offset, pattern, max_length): (
                    VfsPath,
                    u64,
                    crate::file_reader::SearchPattern,
                    u64,
                ) = bincode::deserialize(&req[..]).unwrap();
                let ret = self
                    .file_reader
                    .find_in_file(path, offset, pattern, max_length)
                    .await;

                bincode::serialize(&ret).unwrap()
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
    outbox: tokio::sync::mpsc::UnboundedSender<Message>,
    operations: Arc<Mutex<HashMap<OperationId, OperationHandle>>>,
    next_issue_id: Arc<AtomicU64>,
    context: Arc<operation::OperationContext>,
}

impl OperationDispatcher {
    pub fn new(
        outbox: tokio::sync::mpsc::UnboundedSender<Message>,
        context: Arc<operation::OperationContext>,
    ) -> Self {
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
                let request: StartOperationRequest = bincode::deserialize(&req[..]).unwrap();
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
                        let bytes = bincode::serialize(&progress).unwrap();
                        let _ = outbox_for_bridge
                            .send(Message::Notify(API_OPERATION_PROGRESS, bytes.into()));
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
                Ok(Some(bincode::serialize(&ret).unwrap().into()))
            }
            API_CANCEL_OPERATION => {
                let id: OperationId = bincode::deserialize(&req[..]).unwrap();
                if let Some(handle) = self.operations.lock().get(&id) {
                    handle.cancel.cancel();
                }

                let ret: Result<(), Error> = Ok(());
                Ok(Some(bincode::serialize(&ret).unwrap().into()))
            }
            API_RESOLVE_ISSUE => {
                let request: ResolveIssueRequest = bincode::deserialize(&req[..]).unwrap();
                if let Some(handle) = self.operations.lock().get(&request.operation_id)
                    && let Some(sender) = handle.issue_resolvers.lock().remove(&request.issue_id)
                {
                    let _ = sender.send(request.response);
                }

                let ret: Result<(), Error> = Ok(());
                Ok(Some(bincode::serialize(&ret).unwrap().into()))
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

pub struct VfsRegistryManager {
    registry: Arc<VfsRegistry>,
    /// When set, allows mounting a Remote VFS that proxies calls back to
    /// the host. Used by the agent in remote sessions.
    host_communicator: Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
}

impl VfsRegistryManager {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self {
            registry,
            host_communicator: Arc::new(std::sync::OnceLock::new()),
        }
    }

    pub fn new_with_host_communicator(
        registry: Arc<VfsRegistry>,
        host_communicator: Arc<std::sync::OnceLock<crate::rpc::Communicator>>,
    ) -> Self {
        Self {
            registry,
            host_communicator,
        }
    }
}

#[async_trait::async_trait]
impl VfsManager for VfsRegistryManager {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error> {
        match request {
            MountRequest::S3 {
                region,
                bucket,
                credentials,
            } => {
                let region =
                    aws_config::Region::new(region.unwrap_or_else(|| "us-east-1".to_string()));

                let mut config_loader = aws_config::from_env().region(region.clone());

                // Custom endpoint for S3-compatible services
                if let Some(ref endpoint) = credentials.endpoint_url {
                    config_loader = config_loader.endpoint_url(endpoint);
                }

                // Use explicit profile if specified
                if let Some(ref profile) = credentials.profile {
                    config_loader = config_loader.profile_name(profile);
                }

                // Use explicit IAM credentials if provided
                if let (Some(access_key), Some(secret_key)) =
                    (&credentials.access_key_id, &credentials.secret_access_key)
                {
                    let creds = aws_sdk_s3::config::Credentials::new(
                        access_key,
                        secret_key,
                        credentials.session_token.clone(),
                        None,
                        "newt-explicit",
                    );
                    config_loader = config_loader.credentials_provider(creds);
                }

                let mut sdk_config = config_loader.load().await;

                // AssumeRole: use the resolved credentials to assume a role,
                // then rebuild the config with the temporary credentials.
                if let Some(ref role_arn) = credentials.role_arn {
                    let sts_client = aws_sdk_sts::Client::new(&sdk_config);
                    let mut assume = sts_client
                        .assume_role()
                        .role_arn(role_arn)
                        .role_session_name("newt-session");
                    if let Some(ref ext_id) = credentials.external_id {
                        assume = assume.external_id(ext_id);
                    }
                    let resp = assume.send().await.map_err(|e| Error {
                        kind: crate::ErrorKind::Other,
                        message: format!("AssumeRole failed: {}", e),
                    })?;
                    let sts_creds = resp.credentials().ok_or_else(|| Error {
                        kind: crate::ErrorKind::Other,
                        message: "AssumeRole returned no credentials".into(),
                    })?;
                    let temp_creds = aws_sdk_s3::config::Credentials::new(
                        sts_creds.access_key_id(),
                        sts_creds.secret_access_key(),
                        Some(sts_creds.session_token().to_string()),
                        None,
                        "newt-assume-role",
                    );
                    sdk_config = aws_config::from_env()
                        .region(region)
                        .credentials_provider(temp_creds)
                        .load()
                        .await;
                }

                let client = aws_sdk_s3::Client::new(&sdk_config);
                let vfs = Arc::new(crate::vfs::S3Vfs::new(client, sdk_config, bucket));
                let mount_meta = vfs.mount_meta();
                let type_name = vfs.descriptor().type_name().to_string();
                let vfs_id = self.registry.mount(vfs);
                Ok(MountResponse {
                    vfs_id,
                    type_name,
                    mount_meta,
                    origin: None,
                })
            }
            MountRequest::Sftp { host } => {
                log::info!("mounting SFTP VFS for host={}", host);
                let vfs = Arc::new(crate::vfs::SftpVfs::connect(&host).await?);
                let mount_meta = vfs.mount_meta();
                let type_name = vfs.descriptor().type_name().to_string();
                let vfs_id = self.registry.mount(vfs);
                log::info!("mounted SFTP VFS for host={} as vfs_id={:?}", host, vfs_id);
                Ok(MountResponse {
                    vfs_id,
                    type_name,
                    mount_meta,
                    origin: None,
                })
            }
            MountRequest::Remote => {
                let communicator = self
                    .host_communicator
                    .get()
                    .ok_or_else(|| Error::custom("host communicator not available"))?
                    .clone();
                let vfs = Arc::new(crate::vfs::RemoteVfs::new(communicator));
                let mount_meta = vfs.mount_meta();
                let type_name = vfs.descriptor().type_name().to_string();
                let vfs_id = self.registry.mount(vfs);
                log::info!("mounted remote VFS as vfs_id={:?}", vfs_id);
                Ok(MountResponse {
                    vfs_id,
                    type_name,
                    mount_meta,
                    origin: None,
                })
            }
            MountRequest::Archive { origin } => {
                log::info!("mounting archive VFS for origin={}", origin);
                let (upstream_vfs, archive_path) = self.registry.resolve(&origin)?;

                // Compute display path for mount_meta
                let upstream_desc = upstream_vfs.descriptor();
                let upstream_meta = upstream_vfs.mount_meta();
                let display_path = upstream_desc.format_path(&archive_path, &upstream_meta);
                let mount_meta = display_path.into_bytes();

                let vfs: Arc<dyn crate::vfs::Vfs> =
                    if crate::vfs::is_zip_name(&archive_path.to_string_lossy()) {
                        Arc::new(crate::vfs::ZipArchiveVfs::new(
                            upstream_vfs,
                            archive_path,
                            origin.clone(),
                            mount_meta.clone(),
                        ))
                    } else {
                        Arc::new(crate::vfs::TarArchiveVfs::new(
                            upstream_vfs,
                            archive_path,
                            origin.clone(),
                            mount_meta.clone(),
                        ))
                    };

                let type_name = vfs.descriptor().type_name().to_string();
                let vfs_id = self.registry.mount(vfs);
                log::info!("mounted archive VFS as vfs_id={:?}", vfs_id);
                Ok(MountResponse {
                    vfs_id,
                    type_name,
                    mount_meta,
                    origin: Some(origin),
                })
            }
        }
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
                let request: MountRequest = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs_manager.mount(request).await;
                bincode::serialize(&ret).unwrap()
            }
            API_UNMOUNT_VFS => {
                let vfs_id: VfsId = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs_manager.unmount(vfs_id).await;
                bincode::serialize(&ret).unwrap()
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
                let _: () = bincode::deserialize(&req[..]).unwrap();
                let ret = self.provider.system_hot_paths().await;
                bincode::serialize(&ret).unwrap()
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

pub struct VfsDispatcher {
    vfs: Arc<dyn Vfs>,
}

impl VfsDispatcher {
    pub fn new(vfs: Arc<dyn Vfs>) -> Self {
        Self { vfs }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        use std::io::Read;
        use std::path::PathBuf;

        let ret = match api {
            API_HOST_VFS_LIST_FILES => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.list_files(&path, None).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_POLL_CHANGES => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.poll_changes(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_FS_STATS => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.fs_stats(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_OPEN_READ_SYNC => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = match self.vfs.open_read_sync(&path).await {
                    Ok(mut reader) => {
                        let mut data = Vec::new();
                        match reader.read_to_end(&mut data) {
                            Ok(_) => Ok(data),
                            Err(e) => Err(e.into()),
                        }
                    }
                    Err(e) => Err(e),
                };
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_READ_RANGE => {
                let (path, offset, length): (PathBuf, u64, u64) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.read_range(&path, offset, length).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_FILE_DETAILS => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.file_details(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_FILE_INFO => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.file_info(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_OVERWRITE_SYNC => {
                let (path, data): (PathBuf, Vec<u8>) = bincode::deserialize(&req[..]).unwrap();
                let ret = match self.vfs.overwrite_sync(&path).await {
                    Ok(mut writer) => match std::io::Write::write_all(&mut writer, &data) {
                        Ok(()) => Ok(()),
                        Err(e) => Err(e.into()),
                    },
                    Err(e) => Err(e),
                };
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_CREATE_DIRECTORY => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.create_directory(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_CREATE_SYMLINK => {
                let (link, target): (PathBuf, PathBuf) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.create_symlink(&link, &target).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_TOUCH => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.touch(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_TRUNCATE => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.truncate(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_REMOVE_FILE => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.remove_file(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_REMOVE_DIR => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.remove_dir(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_REMOVE_TREE => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.remove_tree(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_GET_METADATA => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.get_metadata(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_SET_METADATA => {
                let (path, meta): (PathBuf, crate::vfs::VfsMetadata) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.set_metadata(&path, &meta).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_AVAILABLE_SPACE => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.available_space(&path).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_RENAME => {
                let (from, to): (PathBuf, PathBuf) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.rename(&from, &to).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_COPY_WITHIN => {
                let (from, to): (PathBuf, PathBuf) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.copy_within(&from, &to).await;
                bincode::serialize(&ret).unwrap()
            }
            API_HOST_VFS_HARD_LINK => {
                let (link, target): (PathBuf, PathBuf) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.vfs.hard_link(&link, &target).await;
                bincode::serialize(&ret).unwrap()
            }
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, _api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        Ok(false)
    }
}
