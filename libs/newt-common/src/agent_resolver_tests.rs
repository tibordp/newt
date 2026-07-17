//! The RPC-backed `Remote` resolver against a real `AgentFetchDispatcher`,
//! wired over an in-memory duplex — the same arrangement as a session agent
//! fetching a foreign-triple binary from the host.

use std::sync::Arc;

use tokio::io::AsyncReadExt;

use super::{AgentEncoding, AgentResolver, AgentStream, Remote, local_agent_triple};
use crate::Error;
use crate::api::{
    API_HOST_FETCH_AGENT_CHUNK, AgentFetchDispatcher, PendingVfsReadStreams, VfsReadChunkDispatcher,
};
use crate::rpc::Communicator;

const FAKE_TRIPLE: &str = "x86_64-unknown-fake";
const HOST_HASH: &str = "feedface00000000";

/// Host-side resolver serving one on-disk "binary" for `FAKE_TRIPLE`.
struct FileResolver {
    path: std::path::PathBuf,
}

#[async_trait::async_trait]
impl AgentResolver for FileResolver {
    async fn agent_hash(&self) -> Result<String, Error> {
        Ok(HOST_HASH.to_string())
    }
    fn find_agent_binary(&self, triple: &str) -> Result<std::path::PathBuf, Error> {
        if triple == FAKE_TRIPLE {
            Ok(self.path.clone())
        } else {
            Err(Error::custom(format!("no agent for triple {}", triple)))
        }
    }
    fn find_local_agent_binary(&self) -> Result<std::path::PathBuf, Error> {
        Ok(self.path.clone())
    }
}

/// Host and agent communicators joined by an in-memory pipe. Returns the
/// agent-side `Remote` resolver (host side kept alive via the tuple).
fn harness(
    binary_path: std::path::PathBuf,
    cache_dir: std::path::PathBuf,
) -> (Remote, Communicator, Communicator) {
    let (host_stream, agent_stream) = tokio::io::duplex(64 * 1024);

    let (host_outbox, host_inbox) = Communicator::create_outbox();
    let host_dispatcher = AgentFetchDispatcher::new(
        Arc::new(FileResolver { path: binary_path }),
        host_outbox.clone(),
    );
    let host_comm = Communicator::with_dispatcher_and_outbox(
        host_dispatcher,
        host_stream,
        host_outbox,
        host_inbox,
    );

    let pending: PendingVfsReadStreams = Default::default();
    let (agent_outbox, agent_inbox) = Communicator::create_outbox();
    let agent_comm = Communicator::with_dispatcher_and_outbox(
        VfsReadChunkDispatcher::for_api(API_HOST_FETCH_AGENT_CHUNK, pending.clone()),
        agent_stream,
        agent_outbox,
        agent_inbox,
    );

    let lock = Arc::new(std::sync::OnceLock::new());
    assert!(lock.set(agent_comm.clone()).is_ok());
    let remote = Remote::new(lock, pending).with_cache_dir(cache_dir);
    (remote, host_comm, agent_comm)
}

fn fake_binary_bytes() -> Vec<u8> {
    // Patterned and multi-chunk (> one 64 KiB read) so sequencing matters.
    (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect()
}

#[tokio::test]
async fn fetches_foreign_triple_from_host() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("fake-agent");
    let contents = fake_binary_bytes();
    std::fs::write(&binary, &contents).unwrap();

    let (remote, _host, _agent) = harness(binary, dir.path().join("cache"));

    // The remote resolver reports the host's hash.
    assert_eq!(remote.agent_hash().await.unwrap(), HOST_HASH);

    // Gzip stream round-trips through the chunk notifications.
    let mut stream = remote.open_agent_binary(FAKE_TRIPLE, true).await.unwrap();
    assert_eq!(stream.encoding, AgentEncoding::Gzip);
    assert_eq!(stream.raw_size, contents.len() as u64);
    let mut wire = Vec::new();
    stream.reader.read_to_end(&mut wire).await.unwrap();
    assert_eq!(wire.len() as u64, stream.size);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&wire[..]), &mut decoded).unwrap();
    assert_eq!(decoded, contents);

    // Materialize downloads into the hash-keyed cache, executable.
    let path = remote.materialize_agent_binary(FAKE_TRIPLE).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), contents);
    assert!(
        path.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains(HOST_HASH)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
            0
        );
    }
    // Second materialize is a cache hit (same path, still valid).
    let again = remote.materialize_agent_binary(FAKE_TRIPLE).await.unwrap();
    assert_eq!(again, path);
}

#[tokio::test]
async fn self_triple_short_circuits_and_unknown_errors() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("fake-agent");
    std::fs::write(&binary, b"unused").unwrap();

    let (remote, _host, _agent) = harness(binary, dir.path().join("cache"));

    // Own triple never crosses the wire — resolves to the running exe.
    let own = remote
        .materialize_agent_binary(&local_agent_triple())
        .await
        .unwrap();
    assert_eq!(own, std::env::current_exe().unwrap());

    // A triple the host doesn't have surfaces the host's error.
    let err = remote
        .open_agent_binary("riscv64-unknown-none", true)
        .await
        .err()
        .expect("foreign fetch of unknown triple must fail");
    assert!(err.message.contains("no agent for triple"), "{}", err);
}

struct SlowResolver {
    dropped: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

struct SlowReader {
    sent_first: bool,
    dropped: Option<tokio::sync::oneshot::Sender<()>>,
}

impl tokio::io::AsyncRead for SlowReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if !self.sent_first {
            self.sent_first = true;
            buf.put_slice(b"first chunk");
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Pending
        }
    }
}

impl Drop for SlowReader {
    fn drop(&mut self) {
        if let Some(tx) = self.dropped.take() {
            let _ = tx.send(());
        }
    }
}

#[async_trait::async_trait]
impl AgentResolver for SlowResolver {
    async fn agent_hash(&self) -> Result<String, Error> {
        Ok(HOST_HASH.to_string())
    }

    fn find_agent_binary(&self, _triple: &str) -> Result<std::path::PathBuf, Error> {
        Err(Error::not_supported())
    }

    fn find_local_agent_binary(&self) -> Result<std::path::PathBuf, Error> {
        Err(Error::not_supported())
    }

    async fn open_agent_binary(
        &self,
        _triple: &str,
        _accept_gzip: bool,
    ) -> Result<AgentStream, Error> {
        Ok(AgentStream {
            size: 1_000_000,
            raw_size: 1_000_000,
            encoding: AgentEncoding::Raw,
            reader: Box::new(SlowReader {
                sent_first: false,
                dropped: self.dropped.lock().take(),
            }),
        })
    }
}

#[tokio::test]
async fn dropping_fetch_reader_cancels_host_producer() {
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let resolver = Arc::new(SlowResolver {
        dropped: parking_lot::Mutex::new(Some(dropped_tx)),
    });
    let (host_stream, agent_stream) = tokio::io::duplex(64 * 1024);

    let (host_outbox, host_inbox) = Communicator::create_outbox();
    let host_dispatcher = AgentFetchDispatcher::new(resolver, host_outbox.clone());
    let _host = Communicator::with_dispatcher_and_outbox(
        host_dispatcher,
        host_stream,
        host_outbox,
        host_inbox,
    );

    let pending: PendingVfsReadStreams = Default::default();
    let (agent_outbox, agent_inbox) = Communicator::create_outbox();
    let agent = Communicator::with_dispatcher_and_outbox(
        VfsReadChunkDispatcher::for_api(API_HOST_FETCH_AGENT_CHUNK, pending.clone()),
        agent_stream,
        agent_outbox,
        agent_inbox,
    );
    let lock = Arc::new(std::sync::OnceLock::new());
    assert!(lock.set(agent).is_ok());
    let remote = Remote::new(lock, pending);

    let mut stream = remote.open_agent_binary(FAKE_TRIPLE, false).await.unwrap();
    let mut byte = [0u8; 1];
    stream.reader.read_exact(&mut byte).await.unwrap();
    drop(stream.reader);

    tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
        .await
        .expect("host fetch producer survived reader drop")
        .unwrap();
}
