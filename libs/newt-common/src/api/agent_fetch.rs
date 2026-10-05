//! Agent-binary provisioning: serves the host's agent binaries to a
//! session agent — content hash plus a byte stream for nested spawns
//! (pane-scoped agent mounts).

use std::sync::Arc;

use crate::rpc::{Api, Dispatcher, Outbox, StreamId};

use super::{API_HOST_AGENT_HASH, API_HOST_FETCH_AGENT, decode, encode};

/// Response header for `API_HOST_FETCH_AGENT`. The bytes follow on the
/// stream the request named; the consumer validates the received byte
/// count against `size`.
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
}

impl AgentFetchDispatcher {
    pub fn new(resolver: Arc<dyn crate::agent_resolver::AgentResolver>, outbox: Outbox) -> Self {
        Self { resolver, outbox }
    }
}

#[async_trait::async_trait]
impl Dispatcher for AgentFetchDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Option<bytes::Bytes> {
        let ret = match api {
            API_HOST_AGENT_HASH => {
                let ret = self.resolver.agent_hash().await;
                encode(&ret)
            }
            API_HOST_FETCH_AGENT => {
                let (triple, accept_gzip, stream_id): (String, bool, StreamId) = decode(&req[..]);
                let ret = self
                    .resolver
                    .open_agent_binary(&triple, accept_gzip)
                    .await
                    .map(|stream| {
                        let mut reader = stream.reader;
                        let data = self.outbox.stream_sender(stream_id);
                        tokio::spawn(async move {
                            match crate::rpc::pump(reader.as_mut(), data).await {
                                Err(e) if e.kind != crate::ErrorKind::Cancelled => {
                                    log::error!("agent fetch stream failed: {}", e);
                                }
                                _ => {}
                            }
                        });
                        AgentFetchHeader {
                            size: stream.size,
                            raw_size: stream.raw_size,
                            encoding: stream.encoding,
                        }
                    });
                encode(&ret)
            }
            _ => return None,
        };
        Some(ret.into())
    }
}
