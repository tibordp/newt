use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use std::sync::atomic::AtomicU64;

use crate::{
    Error,
    filesystem::{Filesystem, ListFilesOptions},
    hot_paths::HotPathsProvider,
    operation::{self, OperationHandle, OperationId, ResolveIssueRequest, StartOperationRequest},
    rpc::{Api, Dispatcher, Outbox, StreamId},
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
pub const API_REVALIDATE: Api = Api(9);
pub const API_FS_STATS: Api = Api(10);

pub const API_START_OPERATION: Api = Api(200);
pub const API_CANCEL_OPERATION: Api = Api(201);
pub const API_OPERATION_PROGRESS: Api = Api(202);
pub const API_RESOLVE_ISSUE: Api = Api(203);

pub const API_TERMINAL_CREATE: Api = Api(100);
pub const API_TERMINAL_KILL: Api = Api(101);
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
pub const API_RESOLVE_LINK: Api = Api(309);

pub const API_MOUNT_VFS: Api = Api(400);
pub const API_UNMOUNT_VFS: Api = Api(401);
pub const API_VFS_PROGRESS: Api = Api(402);
pub const API_REMOUNT_VFS: Api = Api(403);

pub const API_SYSTEM_HOT_PATHS: Api = Api(500);

// Enrichers — long-lived invoke; partial results ride a stream the caller
// opens, and cancellation is transport-level (drop the invoke → InvokeCancel).
pub const API_START_ENRICHMENT: Api = Api(700);

// Quick Open — long-lived invoke; updates ride a stream the caller opens,
// and query changes come as API_QUICK_OPEN_QUERY signals naming that stream.
pub const API_QUICK_OPEN: Api = Api(710);
pub const API_QUICK_OPEN_QUERY: Api = Api(712);

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
pub const API_VFS_READ_RANGE: Api = Api(604);
pub const API_VFS_FILE_DETAILS: Api = Api(605);
pub const API_VFS_FILE_INFO: Api = Api(606);
pub const API_VFS_OVERWRITE_ASYNC_BEGIN: Api = Api(607);
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
pub const API_VFS_READ_ATTRIBUTE: Api = Api(636);
pub const API_VFS_WRITE_ATTRIBUTE: Api = Api(637);
pub const API_VFS_STREAM_PATH: Api = Api(638);
pub const API_VFS_RESOLVE_LINK: Api = Api(639);
pub const API_VFS_READ_ORDER: Api = Api(640);
pub const API_VFS_WRITE_RANGE: Api = Api(641);
pub const API_VFS_LIST_RECURSIVE: Api = Api(642);
pub const API_VFS_RENAME_NO_REPLACE: Api = Api(643);
pub const API_VFS_EXCHANGE: Api = Api(644);
pub const API_VFS_IS_PACKAGE: Api = Api(645);

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

mod agent_fetch;
mod read_at;
mod vfs;
pub use agent_fetch::{AgentFetchDispatcher, AgentFetchHeader};
pub(crate) use read_at::{ReadAtId, ReadAtSessions, RemoteRandomReader};
pub use vfs::VfsDispatcher;

// bincode helpers. Both ends are built from the same source, so a payload
// that doesn't round-trip is a bug.

pub fn decode<'a, T: serde::Deserialize<'a>>(req: &'a [u8]) -> T {
    bincode::deserialize(req).expect("RPC decode")
}

pub fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    bincode::serialize(value).expect("RPC encode")
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
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        let ret = match api {
            API_POLL_CHANGES => {
                let path: VfsPath = decode(&req[..]);
                let ret = self.filesystem.poll_changes(path).await;

                encode(&ret)
            }
            API_LIST_FILES => {
                let args: (VfsPath, ListFilesOptions) = decode(&req[..]);
                let ret = self.filesystem.list_files(args.0, args.1, None).await;

                encode(&ret)
            }
            API_LIST_FILES_STREAMING => {
                let (path, opts, stream_id): (VfsPath, ListFilesOptions, StreamId) =
                    decode(&req[..]);
                let (batch_tx, batch_rx) = tokio::sync::mpsc::channel::<FileList>(
                    crate::filesystem::LIST_BATCH_CHANNEL_CAPACITY,
                );
                // Dropping the invoke drops batch_rx with it, which cancels
                // the listing through every bounded bridge down to the VFS.
                let listing = self.filesystem.list_files(path, opts, Some(batch_tx));
                let ret = self
                    .outbox
                    .stream_sender(stream_id)
                    .forward(batch_rx, listing)
                    .await;

                encode(&ret.and_then(|ret| ret))
            }
            API_TOUCH => {
                let path: VfsPath = decode(&req[..]);
                let ret = self.filesystem.touch(path).await;

                encode(&ret)
            }
            API_CREATE_DIRECTORY => {
                let path: VfsPath = decode(&req[..]);
                let ret = self.filesystem.create_directory(path).await;

                encode(&ret)
            }
            API_REVALIDATE => {
                let vfs_id: VfsId = decode(&req[..]);
                let ret = self.filesystem.revalidate(vfs_id).await;
                encode(&ret)
            }
            API_FS_STATS => {
                let path: VfsPath = decode(&req[..]);
                let ret = self.filesystem.fs_stats(path).await;
                encode(&ret)
            }
            API_FILE_DETAILS => {
                let path: VfsPath = decode(&req[..]);
                let ret = self.filesystem.file_details(path).await;

                encode(&ret)
            }
            API_RESOLVE_LINK => {
                let path: VfsPath = decode(&req[..]);
                encode(&self.filesystem.resolve_link(path).await)
            }
            API_GET_PROPERTY_SHEET => {
                let path: VfsPath = decode(&req[..]);
                let ret = self.filesystem.get_property_sheet(path).await;

                encode(&ret)
            }
            API_READ_RANGE => {
                let (path, offset, length): (VfsPath, u64, u64) = decode(&req[..]);
                let ret = self.filesystem.read_range(path, offset, length).await;

                encode(&ret)
            }
            API_OPEN_READ_AT => {
                let path: VfsPath = decode(&req[..]);
                let ret: Result<ReadAtId, Error> = self
                    .filesystem
                    .open_read_at(path)
                    .await
                    .map(|reader| self.read_at_sessions.open(reader));

                encode(&ret)
            }
            API_READ_AT => {
                let (id, offset, len): (ReadAtId, u64, u64) = decode(&req[..]);
                let ret = self.read_at_sessions.read_at(id, offset, len).await;

                encode(&ret)
            }
            API_READ_FILE => {
                let (path, max_size): (VfsPath, u64) = decode(&req[..]);
                let ret = self.filesystem.read_file(path, max_size).await;

                encode(&ret.map(serde_bytes::ByteBuf::from))
            }
            API_WRITE_FILE => {
                let (path, data): (VfsPath, serde_bytes::ByteBuf) = decode(&req[..]);
                let ret = self.filesystem.write_file(path, data.into_vec()).await;

                encode(&ret)
            }
            API_FIND_IN_FILE => {
                let (path, offset, pattern, max_length): (
                    VfsPath,
                    u64,
                    crate::find::SearchPattern,
                    u64,
                ) = decode(&req[..]);
                let ret = self
                    .filesystem
                    .find_in_file(path, offset, pattern, max_length)
                    .await;

                encode(&ret)
            }
            _ => return None,
        };

        Some(ret.into())
    }

    fn notify(&self, api: Api, req: bytes::Bytes) -> bool {
        if api == API_READ_AT_CLOSE {
            let id: ReadAtId = decode(&req[..]);
            self.read_at_sessions.close(id);
            true
        } else {
            false
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
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        let ret = match api {
            API_SHELL_EXPAND => {
                let input: String = decode(&req[..]);
                let ret = self.shell_service.shell_expand(input).await;
                encode(&ret)
            }
            _ => return None,
        };

        Some(ret.into())
    }
}

pub struct TerminalDispatcher {
    terminal: Arc<dyn TerminalClient>,
    outbox: Outbox,
}

impl TerminalDispatcher {
    pub fn new<T: TerminalClient + 'static>(terminal: T, outbox: Outbox) -> Self {
        Self {
            terminal: Arc::new(terminal),
            outbox,
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for TerminalDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        let ret = match api {
            API_TERMINAL_CREATE => {
                let (options, output): (crate::terminal::TerminalOptions, StreamId) =
                    decode(&req[..]);
                let ret = self.terminal.create(options).await.map(|handle| {
                    let (input_id, input) = self.outbox.open_stream();
                    crate::terminal::serve(
                        self.terminal.clone(),
                        handle,
                        self.outbox.stream_sender(output),
                        input,
                    );
                    (handle, input_id)
                });

                encode(&ret)
            }
            API_TERMINAL_KILL => {
                let handle: crate::terminal::TerminalHandle = decode(&req[..]);
                let ret = self.terminal.kill(handle).await;

                encode(&ret)
            }
            API_TERMINAL_WAIT => {
                let handle: crate::terminal::TerminalHandle = decode(&req[..]);
                let ret = self.terminal.wait(handle).await;

                encode(&ret)
            }
            _ => return None,
        };

        Some(ret.into())
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
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        match api {
            API_START_OPERATION => {
                let request: StartOperationRequest = decode(&req[..]);
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
                        let _ = outbox_for_bridge
                            .notify(API_OPERATION_PROGRESS, &progress)
                            .await;
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
                Some(encode(&ret).into())
            }
            API_CANCEL_OPERATION => {
                let id: OperationId = decode(&req[..]);
                if let Some(handle) = self.operations.lock().get(&id) {
                    handle.cancel.cancel();
                }

                let ret: Result<(), Error> = Ok(());
                Some(encode(&ret).into())
            }
            API_RESOLVE_ISSUE => {
                let request: ResolveIssueRequest = decode(&req[..]);
                if let Some(handle) = self.operations.lock().get(&request.operation_id)
                    && let Some(sender) = handle.issue_resolvers.lock().remove(&request.issue_id)
                {
                    let _ = sender.send(request.response);
                }

                let ret: Result<(), Error> = Ok(());
                Some(encode(&ret).into())
            }
            _ => None,
        }
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
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        match api {
            API_START_ENRICHMENT => {
                let (stream_id, path, scope, enrichers): (
                    StreamId,
                    VfsPath,
                    crate::enrich::EnrichScope,
                    Vec<String>,
                ) = decode(&req[..]);

                let (tx, rx) = tokio::sync::mpsc::channel::<crate::enrich::EnrichmentEvent>(16);
                let enrichment = self.enrichers.enrich(path, scope, enrichers, tx);
                let ret = self
                    .outbox
                    .stream_sender(stream_id)
                    .forward(rx, enrichment)
                    .await;

                Some(encode(&ret.and_then(|ret| ret)).into())
            }
            _ => None,
        }
    }
}

/// Serves Quick Open runs; the side of the session that owns the
/// filesystem.
pub struct QuickOpenDispatcher {
    quick_open: Arc<crate::quick_open::QuickOpen>,
    outbox: Outbox,
    /// Each run's query, by the run's update stream, for the signals that
    /// change it.
    queries: Mutex<HashMap<StreamId, tokio::sync::watch::Sender<String>>>,
}

impl QuickOpenDispatcher {
    pub fn new(outbox: Outbox, quick_open: Arc<crate::quick_open::QuickOpen>) -> Self {
        Self {
            quick_open,
            outbox,
            queries: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl Dispatcher for QuickOpenDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        use crate::quick_open::{QuickOpenOptions, QuickOpenUpdate};
        if api != API_QUICK_OPEN {
            return None;
        }
        let (stream_id, root, options, query): (StreamId, VfsPath, QuickOpenOptions, String) =
            decode(&req[..]);
        let (query_tx, query_rx) = tokio::sync::watch::channel(query);
        self.queries.lock().insert(stream_id, query_tx);
        struct Guard<'a> {
            id: StreamId,
            queries: &'a Mutex<HashMap<StreamId, tokio::sync::watch::Sender<String>>>,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.queries.lock().remove(&self.id);
            }
        }
        let _guard = Guard {
            id: stream_id,
            queries: &self.queries,
        };

        let (tx, rx) = tokio::sync::mpsc::channel::<QuickOpenUpdate>(4);
        let run = self.quick_open.run(root, options, query_rx, tx);
        let ret = self.outbox.stream_sender(stream_id).forward(rx, run).await;
        Some(encode(&ret.and_then(|ret| ret)).into())
    }

    fn notify(&self, api: Api, req: bytes::Bytes) -> bool {
        if api != API_QUICK_OPEN_QUERY {
            return false;
        }
        let (stream_id, query): (StreamId, String) = decode(&req[..]);
        // A run that has ended may still be sent the last keystrokes.
        if let Some(tx) = self.queries.lock().get(&stream_id) {
            tx.send_replace(query);
        }
        true
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
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        let ret = match api {
            API_MOUNT_VFS => {
                let request: MountRequest = decode(&req[..]);
                let ret = self.vfs_manager.mount(request).await;
                encode(&ret)
            }
            API_UNMOUNT_VFS => {
                let vfs_id: VfsId = decode(&req[..]);
                let ret = self.vfs_manager.unmount(vfs_id).await;
                encode(&ret)
            }
            API_REMOUNT_VFS => {
                let (vfs_id, mount_meta): (VfsId, Option<Vec<u8>>) = decode(&req[..]);
                let ret = self.vfs_manager.remount(vfs_id, mount_meta).await;
                encode(&ret)
            }
            _ => return None,
        };

        Some(ret.into())
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
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        let ret = match api {
            API_DISCOVER_SSH_HOSTS => {
                let _: () = decode(&req[..]);
                encode(&self.provider.ssh_hosts().await)
            }
            API_DISCOVER_CONTAINERS => {
                let engine: String = decode(&req[..]);
                encode(&self.provider.containers(engine).await)
            }
            API_DISCOVER_KUBE_CONTEXTS => {
                let _: () = decode(&req[..]);
                encode(&self.provider.kube_contexts().await)
            }
            API_DISCOVER_KUBE_PODS => {
                let (context, namespace): (Option<String>, Option<String>) = decode(&req[..]);
                encode(&self.provider.kube_pods(context, namespace).await)
            }
            _ => return None,
        };

        Some(ret.into())
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
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        let ret = match api {
            API_SYSTEM_HOT_PATHS => {
                let _: () = decode(&req[..]);
                let ret = self.provider.system_hot_paths().await;
                encode(&ret)
            }
            _ => return None,
        };

        Some(ret.into())
    }
}

#[cfg(test)]
mod filesystem_read_at_tests {
    use super::*;
    use crate::rpc::Dispatcher;
    use crate::test_support::mock_vfs::MockVfs;
    use crate::vfs::VfsRegistry;

    #[tokio::test]
    async fn dispatcher_serves_read_at_sessions_and_close_reaps() {
        let registry = Arc::new(VfsRegistry::with_root(
            MockVfs::builder().file("/f", b"hello world").build(),
        ));
        let (outbox, _outbox_rx) = crate::rpc::create_outbox();
        let dispatcher =
            FilesystemDispatcher::new(crate::vfs::VfsRegistryFs::new(registry), outbox);

        let path = VfsPath::from_wire_str(VfsId::ROOT, "/f");
        let response = dispatcher
            .invoke(API_OPEN_READ_AT, encode(&path).into())
            .await
            .unwrap();
        let id: Result<ReadAtId, Error> = decode(&response);
        let id = id.unwrap();
        assert!(dispatcher.read_at_sessions.contains(id));

        let response = dispatcher
            .invoke(API_READ_AT, encode(&(id, 6u64, 5u64)).into())
            .await
            .unwrap();
        let data: Result<serde_bytes::ByteBuf, Error> = decode(&response);
        assert_eq!(data.unwrap().as_slice(), b"world");

        assert!(dispatcher.notify(API_READ_AT_CLOSE, encode(&id).into()));
        assert!(!dispatcher.read_at_sessions.contains(id));
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::test_support::endless_listing::EndlessListing;

    #[tokio::test]
    async fn cancelling_streaming_listing_drops_blocking_producer_receiver() {
        let (stopped_tx, stopped_rx) = tokio::sync::oneshot::channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (outbox, _outbox_rx) = crate::rpc::create_outbox();
        let dispatcher = FilesystemDispatcher::new(
            EndlessListing {
                started: parking_lot::Mutex::new(Some(started_tx)),
                stopped: parking_lot::Mutex::new(Some(stopped_tx)),
            },
            outbox,
        );
        let path = VfsPath::root(VfsId(0));
        let request = encode(&(path, ListFilesOptions { strict: true }, StreamId(1)));

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

#[cfg(all(test, unix))]
mod resolve_link_tests {
    use std::sync::Arc;

    use super::*;
    use crate::rpc::Communicator;
    use crate::vfs::path::PathBuf;
    use crate::vfs::{LocalVfs, VfsRegistry, VfsRegistryFs};

    /// A relative link climbing out of its directory (`src/hub ->
    /// ../kubernetes/hub`) resolves where the OS takes it, across the RPC.
    #[tokio::test]
    async fn a_relative_link_resolves_over_the_rpc() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("kubernetes/hub")).unwrap();
        std::fs::create_dir(base.join("src")).unwrap();
        std::os::unix::fs::symlink("../kubernetes/hub", base.join("src/hub")).unwrap();

        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let registry = Arc::new(VfsRegistry::with_root(Arc::new(LocalVfs::new())));
        let _server = Communicator::with_dispatcher(server_stream, |outbox| {
            FilesystemDispatcher::new(VfsRegistryFs::new(registry), outbox.clone())
        });
        let remote = crate::filesystem::Remote::new(Communicator::new(client_stream));

        let link = VfsPath::new(VfsId::ROOT, PathBuf::from_native(&base.join("src/hub")));
        assert_eq!(
            remote.resolve_link(link).await.unwrap(),
            VfsPath::new(
                VfsId::ROOT,
                PathBuf::from_native(&base.join("kubernetes/hub"))
            )
        );
    }
}
