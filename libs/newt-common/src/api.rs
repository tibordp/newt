use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use std::sync::atomic::AtomicU64;

use crate::{
    filesystem::{Filesystem, ListFilesOptions},
    operation::{self, OperationHandle, OperationId, ResolveIssueRequest, StartOperationRequest},
    rpc::{Api, Dispatcher, Message},
    terminal::TerminalClient,
    Error,
};

pub const API_POLL_CHANGES: Api = Api(0);
pub const API_LIST_FILES: Api = Api(1);
pub const API_RENAME: Api = Api(2);
pub const API_CREATE_DIRECTORY: Api = Api(3);
pub const API_DELETE_ALL: Api = Api(4);
pub const API_TOUCH: Api = Api(5);
pub const API_SHELL_EXPAND: Api = Api(6);

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

pub struct FilesystemDispatcher {
    filesystem: Box<dyn Filesystem>,
}

impl FilesystemDispatcher {
    pub fn new<F: Filesystem + 'static>(filesystem: F) -> Self {
        Self {
            filesystem: Box::new(filesystem),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for FilesystemDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_POLL_CHANGES => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.poll_changes(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_LIST_FILES => {
                let args: (PathBuf, ListFilesOptions) = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.list_files(args.0, args.1).await;

                bincode::serialize(&ret).unwrap()
            }
            API_RENAME => {
                let (old_path, new_path): (PathBuf, PathBuf) =
                    bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.rename(old_path, new_path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_TOUCH => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.touch(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_CREATE_DIRECTORY => {
                let path: PathBuf = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.create_directory(path).await;

                bincode::serialize(&ret).unwrap()
            }
            API_DELETE_ALL => {
                let paths: Vec<PathBuf> = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.delete_all(paths).await;

                bincode::serialize(&ret).unwrap()
            }
            API_SHELL_EXPAND => {
                let path: String = bincode::deserialize(&req[..]).unwrap();
                let ret = self.filesystem.shell_expand(path).await;

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

pub struct OperationDispatcher {
    outbox: tokio::sync::mpsc::UnboundedSender<Message>,
    operations: Arc<Mutex<HashMap<OperationId, OperationHandle>>>,
    next_issue_id: Arc<AtomicU64>,
}

impl OperationDispatcher {
    pub fn new(outbox: tokio::sync::mpsc::UnboundedSender<Message>) -> Self {
        Self {
            outbox,
            operations: Arc::new(Mutex::new(HashMap::new())),
            next_issue_id: Arc::new(AtomicU64::new(1)),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for OperationDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        match api {
            API_START_OPERATION => {
                let request: StartOperationRequest =
                    bincode::deserialize(&req[..]).unwrap();
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

                tokio::spawn(async move {
                    operation::execute_operation(
                        id,
                        request.request,
                        outbox,
                        cancel,
                        issue_resolvers,
                        next_issue_id,
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
                let request: ResolveIssueRequest =
                    bincode::deserialize(&req[..]).unwrap();
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
