//! The RPC-backed `Remote` resolver against a real `AgentFetchDispatcher`,
//! wired over an in-memory duplex — the same arrangement as a session agent
//! fetching a foreign-triple binary from the host.

use std::sync::Arc;

use tokio::io::AsyncReadExt;

use super::{AgentEncoding, AgentResolver, Remote, local_agent_triple};
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
