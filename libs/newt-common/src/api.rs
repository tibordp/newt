use std::path::PathBuf;

use crate::{
    filesystem::{Filesystem, ListFilesOptions},
    rpc::{Api, Dispatcher},
    terminal::TerminalClient,
    Error,
};

pub const API_POLL_CHANGES: Api = Api(0);
pub const API_LIST_FILES: Api = Api(1);
pub const API_RENAME: Api = Api(2);
pub const API_CREATE_DIRECTORY: Api = Api(3);
pub const API_DELETE_ALL: Api = Api(4);

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
            _ => return Ok(None),
        };

        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        match api {
            _ => Ok(false),
        }
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
                eprintln!("ret: {:?}", ret);

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

    async fn notify(&self, api: Api, _req: bytes::Bytes) -> Result<bool, Error> {
        match api {
            _ => Ok(false),
        }
    }
}
