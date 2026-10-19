//! `VfsDispatcher`: the API_VFS_* surface that lets a `RemoteVfs` on one
//! side of a connection drive a real VFS on the other. Direction is
//! symmetric — the caller may be the agent reaching the host's VFS, or the
//! host reaching a spawned sub-agent's VFS (an agent mount).
//!
//! - reads: `OPEN_READ_ASYNC` names a stream the caller opened, answers once
//!   the file is open, and pumps the file into the stream.
//! - writes: `OVERWRITE_ASYNC_BEGIN` names a stream the caller opened for
//!   the result, and answers with a stream it opened for the data. The
//!   writer commits at the end of the data and reports on the result stream.
//! - positioned reads: `OPEN_READ_AT` mints a server-held
//!   `VfsRandomReader`, `READ_AT` is plain request/response per chunk, and
//!   `READ_AT_CLOSE` (sent on proxy drop) reaps the handle.

use std::sync::Arc;

use super::{
    API_VFS_AVAILABLE_SPACE, API_VFS_COPY_WITHIN, API_VFS_CREATE_DIRECTORY, API_VFS_CREATE_SYMLINK,
    API_VFS_FILE_INFO, API_VFS_FS_STATS, API_VFS_GET_METADATA, API_VFS_HARD_LINK,
    API_VFS_LIST_FILES, API_VFS_OPEN_READ_ASYNC, API_VFS_OPEN_READ_AT,
    API_VFS_OVERWRITE_ASYNC_BEGIN, API_VFS_POLL_CHANGES, API_VFS_READ_AT, API_VFS_READ_AT_CLOSE,
    API_VFS_READ_ATTRIBUTE, API_VFS_READ_RANGE, API_VFS_REMOVE_DIR, API_VFS_REMOVE_FILE,
    API_VFS_REMOVE_TREE, API_VFS_RENAME, API_VFS_SAME_FILE, API_VFS_SET_METADATA,
    API_VFS_STREAM_PATH, API_VFS_TOUCH, API_VFS_TRASH_ITEM, API_VFS_TRUNCATE,
    API_VFS_WRITE_ATTRIBUTE, decode, encode,
};
use crate::Error;
use crate::rpc::{Api, Dispatcher, Outbox, StreamId, StreamReceiver, StreamSender};
use crate::vfs::{Vfs, VfsAsyncWriter};

/// Write what arrives on `data` into `writer`, commit it at the end of the
/// stream, and report the outcome on `result`. A failed stream, or a caller
/// that stops listening for the result, leaves `writer` uncommitted.
async fn write_from_stream(
    mut writer: Box<dyn VfsAsyncWriter>,
    mut data: StreamReceiver,
    mut result: StreamSender,
) {
    let ret = async {
        while let Some(chunk) = data.recv().await {
            let chunk = chunk?;
            let mut offset = 0;
            while offset < chunk.len() {
                let written = tokio::select! {
                    biased;
                    _ = result.cancelled() => return Err(Error::cancelled()),
                    written = writer.write(&chunk[offset..]) => written?,
                };
                if written == 0 {
                    return Err(Error::custom("write made no progress"));
                }
                offset += written;
            }
        }
        tokio::select! {
            biased;
            _ = result.cancelled() => Err(Error::cancelled()),
            ret = writer.finish() => ret,
        }
    }
    .await;
    // Report before letting go of `data`: dropping it fails the caller's
    // next write, which then reads the result to learn why.
    if result.send_item(&ret).await.is_ok() {
        let _ = result.finish().await;
    }
    drop(data);
}

pub struct VfsDispatcher {
    vfs: Arc<dyn Vfs>,
    outbox: Outbox,
    read_at_sessions: super::ReadAtSessions,
}

impl VfsDispatcher {
    pub fn new(vfs: Arc<dyn Vfs>, outbox: Outbox) -> Self {
        Self {
            vfs,
            outbox,
            read_at_sessions: super::ReadAtSessions::new(),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for VfsDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        use crate::vfs::path::PathBuf;

        let ret = match api {
            API_VFS_LIST_FILES => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.list_files(&path, None).await;
                encode(&ret)
            }
            API_VFS_POLL_CHANGES => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.poll_changes(&path).await;
                encode(&ret)
            }
            API_VFS_FS_STATS => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.fs_stats(&path).await;
                encode(&ret)
            }
            API_VFS_OPEN_READ_ASYNC => {
                let (path, stream_id): (PathBuf, StreamId) = decode(&req[..]);
                let ret = self.vfs.open_read_async(&path).await.map(|mut reader| {
                    let data = self.outbox.stream_sender(stream_id);
                    tokio::spawn(async move {
                        let _ = crate::rpc::pump(reader.as_mut(), data).await;
                    });
                });
                encode(&ret)
            }
            API_VFS_READ_RANGE => {
                let (path, offset, length): (PathBuf, u64, u64) = decode(&req[..]);
                let ret = self.vfs.read_range(&path, offset, length).await;
                encode(&ret)
            }
            API_VFS_OPEN_READ_AT => {
                let path: PathBuf = decode(&req[..]);
                let ret: Result<super::ReadAtId, Error> =
                    crate::vfs::open_read_at(&self.vfs, &path)
                        .await
                        .map(|reader| self.read_at_sessions.open(reader));
                encode(&ret)
            }
            API_VFS_READ_AT => {
                let (id, offset, len): (super::ReadAtId, u64, u64) = decode(&req[..]);
                let ret = self.read_at_sessions.read_at(id, offset, len).await;
                encode(&ret)
            }
            API_VFS_FILE_INFO => {
                let (path, follow): (PathBuf, bool) = decode(&req[..]);
                let ret = self.vfs.file_info(&path, follow).await;
                encode(&ret)
            }
            API_VFS_OVERWRITE_ASYNC_BEGIN => {
                let (path, options, result_id): (
                    PathBuf,
                    crate::vfs::attributes::WriteOptions,
                    StreamId,
                ) = decode(&req[..]);
                let ret = self
                    .vfs
                    .overwrite_async(&path, &options)
                    .await
                    .map(|writer| {
                        let (data_id, data) = self.outbox.open_stream();
                        let result = self.outbox.stream_sender(result_id);
                        tokio::spawn(write_from_stream(writer, data, result));
                        data_id
                    });
                encode(&ret)
            }
            API_VFS_CREATE_DIRECTORY => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.create_directory(&path).await;
                encode(&ret)
            }
            API_VFS_CREATE_SYMLINK => {
                let (link, target): (PathBuf, String) = decode(&req[..]);
                let ret = self.vfs.create_symlink(&link, &target).await;
                encode(&ret)
            }
            API_VFS_TOUCH => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.touch(&path).await;
                encode(&ret)
            }
            API_VFS_TRUNCATE => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.truncate(&path).await;
                encode(&ret)
            }
            super::API_VFS_WRITE_RANGE => {
                let (path, offset, data): (PathBuf, u64, Vec<u8>) = decode(&req[..]);
                encode(&self.vfs.write_range(&path, offset, &data).await)
            }
            API_VFS_REMOVE_FILE => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.remove_file(&path).await;
                encode(&ret)
            }
            API_VFS_REMOVE_DIR => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.remove_dir(&path).await;
                encode(&ret)
            }
            API_VFS_REMOVE_TREE => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.remove_tree(&path).await;
                encode(&ret)
            }
            API_VFS_TRASH_ITEM => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.trash_item(&path).await;
                encode(&ret)
            }
            API_VFS_READ_ATTRIBUTE => {
                let (path, kind): (PathBuf, crate::vfs::attributes::AttributeKind) =
                    decode(&req[..]);
                encode(&self.vfs.read_attribute(&path, kind).await)
            }
            API_VFS_WRITE_ATTRIBUTE => {
                let (path, property): (PathBuf, crate::vfs::attributes::Attribute) =
                    decode(&req[..]);
                encode(&self.vfs.write_attribute(&path, &property).await)
            }
            super::API_VFS_RESOLVE_LINK => {
                let path: PathBuf = decode(&req[..]);
                encode(&self.vfs.resolve_link(&path).await)
            }
            super::API_VFS_READ_ORDER => {
                let paths: Vec<PathBuf> = decode(&req[..]);
                encode(&self.vfs.read_order(&paths).await)
            }
            super::API_VFS_LIST_RECURSIVE => {
                let prefix: PathBuf = decode(&req[..]);
                let (tx, mut rx) = tokio::sync::mpsc::channel(8);
                let (result, entries) = tokio::join!(self.vfs.list_recursive(&prefix, tx), async {
                    let mut entries = Vec::new();
                    while let Some(batch) = rx.recv().await {
                        entries.extend(batch);
                    }
                    entries
                });
                encode(&result.map(|()| entries))
            }
            API_VFS_STREAM_PATH => {
                let (path, name): (PathBuf, String) = decode(&req[..]);
                encode(&self.vfs.stream_path(&path, &name).await)
            }
            API_VFS_GET_METADATA => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.get_metadata(&path).await;
                encode(&ret)
            }
            API_VFS_SET_METADATA => {
                let (path, meta): (PathBuf, crate::vfs::VfsMetadata) = decode(&req[..]);
                let ret = self.vfs.set_metadata(&path, &meta).await;
                encode(&ret)
            }
            API_VFS_AVAILABLE_SPACE => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.available_space(&path).await;
                encode(&ret)
            }
            API_VFS_SAME_FILE => {
                let (a, b): (PathBuf, PathBuf) = decode(&req[..]);
                let ret = self.vfs.same_file(&a, &b).await;
                encode(&ret)
            }
            API_VFS_RENAME => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..]);
                let ret = self.vfs.rename(&from, &to).await;
                encode(&ret)
            }
            super::API_VFS_RENAME_NO_REPLACE => {
                let (from, to): (PathBuf, PathBuf) = decode(&req[..]);
                let ret = self.vfs.rename_no_replace(&from, &to).await;
                encode(&ret)
            }
            super::API_VFS_IS_PACKAGE => {
                let path: PathBuf = decode(&req[..]);
                let ret = self.vfs.is_package(&path).await;
                encode(&ret)
            }
            super::API_VFS_EXCHANGE => {
                let (a, b): (PathBuf, PathBuf) = decode(&req[..]);
                let ret = self.vfs.exchange(&a, &b).await;
                encode(&ret)
            }
            API_VFS_COPY_WITHIN => {
                let (from, to, options): (PathBuf, PathBuf, crate::vfs::attributes::WriteOptions) =
                    decode(&req[..]);
                let ret = self.vfs.copy_within(&from, &to, &options).await;
                encode(&ret)
            }
            API_VFS_HARD_LINK => {
                let (link, target): (PathBuf, PathBuf) = decode(&req[..]);
                let ret = self.vfs.hard_link(&link, &target).await;
                encode(&ret)
            }
            _ => return None,
        };

        Some(ret.into())
    }

    fn notify(&self, api: Api, req: bytes::Bytes) -> bool {
        if api == API_VFS_READ_AT_CLOSE {
            let id: super::ReadAtId = decode(&req[..]);
            self.read_at_sessions.close(id);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::rpc::Dispatcher;
    use crate::test_support::mock_vfs::MockVfs;
    use crate::vfs::path::PathBuf;

    #[tokio::test]
    async fn read_at_session_serves_reads_and_close_reaps_it() {
        let vfs = MockVfs::builder().file("/f", b"hello world").build();
        let (outbox, _outbox_rx) = crate::rpc::create_outbox();
        let dispatcher = super::VfsDispatcher::new(vfs, outbox);

        let response = dispatcher
            .invoke(
                super::API_VFS_OPEN_READ_AT,
                super::encode(&PathBuf::from_wire_str("/f")).into(),
            )
            .await
            .unwrap();
        let id: Result<super::super::ReadAtId, crate::Error> = super::decode(&response);
        let id = id.unwrap();
        assert!(dispatcher.read_at_sessions.contains(id));

        let response = dispatcher
            .invoke(
                super::API_VFS_READ_AT,
                super::encode(&(id, 6u64, 5u64)).into(),
            )
            .await
            .unwrap();
        let data: Result<serde_bytes::ByteBuf, crate::Error> = super::decode(&response);
        assert_eq!(data.unwrap().as_slice(), b"world");

        assert!(dispatcher.notify(super::API_VFS_READ_AT_CLOSE, super::encode(&id).into()));
        assert!(!dispatcher.read_at_sessions.contains(id));
    }
}
