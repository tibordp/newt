use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use std::sync::atomic::AtomicU64;

use crate::{
    file_reader::FileReader,
    filesystem::{FileList, Filesystem, ListFilesOptions, ShellService, StreamId},
    operation::{self, OperationHandle, OperationId, ResolveIssueRequest, StartOperationRequest},
    rpc::{Api, Dispatcher, Message},
    terminal::TerminalClient,
    vfs::{MountRequest, MountResponse, Vfs, VfsId, VfsManager, VfsPath, VfsRegistry},
    Error,
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

pub const API_MOUNT_VFS: Api = Api(400);
pub const API_UNMOUNT_VFS: Api = Api(401);

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

                let (batch_tx, mut batch_rx) = tokio::sync::mpsc::unbounded_channel::<FileList>();

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
                if let Some(handle) = self.operations.lock().get(&request.operation_id) {
                    if let Some(sender) = handle.issue_resolvers.lock().remove(&request.issue_id) {
                        let _ = sender.send(request.response);
                    }
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
}

impl VfsRegistryManager {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl VfsManager for VfsRegistryManager {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error> {
        match request {
            MountRequest::S3 { region } => {
                let sdk_config = aws_config::from_env()
                    .region(aws_config::Region::new(
                        region.unwrap_or_else(|| "us-east-1".to_string()),
                    ))
                    .load()
                    .await;
                let client = aws_sdk_s3::Client::new(&sdk_config);
                let vfs = Arc::new(crate::vfs::S3Vfs::new(client, sdk_config));
                let mount_meta = vfs.mount_meta();
                let type_name = vfs.descriptor().type_name().to_string();
                let vfs_id = self.registry.mount(vfs);
                Ok(MountResponse {
                    vfs_id,
                    type_name,
                    mount_meta,
                })
            }
        }
    }

    async fn unmount(&self, vfs_id: VfsId) -> Result<(), Error> {
        self.registry
            .unmount(vfs_id)
            .map(|_| ())
            .ok_or_else(|| Error::Custom(format!("cannot unmount VFS {}", vfs_id)))
    }
}

pub struct VfsDispatcher {
    vfs_manager: Box<dyn VfsManager>,
}

impl VfsDispatcher {
    pub fn new<V: VfsManager + 'static>(vfs_manager: V) -> Self {
        Self {
            vfs_manager: Box::new(vfs_manager),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsDispatcher {
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
