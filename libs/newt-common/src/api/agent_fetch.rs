//! Agent-binary provisioning: serves the host's agent binaries to a
//! session agent — content hash plus a sequenced chunk stream for nested
//! spawns (pane-scoped agent mounts).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::filesystem::StreamId;
use crate::rpc::{Api, Dispatcher, Outbox};

use super::{
    API_HOST_AGENT_HASH, API_HOST_FETCH_AGENT, API_HOST_FETCH_AGENT_CANCEL,
    API_HOST_FETCH_AGENT_CHUNK, decode, encode,
};

/// Response header for `API_HOST_FETCH_AGENT`. The bytes follow as
/// sequenced `API_HOST_FETCH_AGENT_CHUNK` notifications (empty sentinel =
/// EOF); the consumer validates the received byte count against `size`.
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
    fetches: Arc<Mutex<HashMap<StreamId, CancellationToken>>>,
}

impl AgentFetchDispatcher {
    pub fn new(resolver: Arc<dyn crate::agent_resolver::AgentResolver>, outbox: Outbox) -> Self {
        Self {
            resolver,
            outbox,
            fetches: Default::default(),
        }
    }
}

struct FetchRegistration {
    stream_id: StreamId,
    fetches: Arc<Mutex<HashMap<StreamId, CancellationToken>>>,
}

impl Drop for FetchRegistration {
    fn drop(&mut self) {
        self.fetches.lock().remove(&self.stream_id);
    }
}

#[async_trait::async_trait]
impl Dispatcher for AgentFetchDispatcher {
    async fn invoke(&self, api: Api, req: bytes::Bytes) -> Result<Option<bytes::Bytes>, Error> {
        let ret = match api {
            API_HOST_AGENT_HASH => {
                let ret = self.resolver.agent_hash().await;
                encode(&ret)?
            }
            API_HOST_FETCH_AGENT => {
                let (triple, accept_gzip, stream_id): (String, bool, StreamId) = decode(&req[..])?;
                // Register before opening the stream: a caller can cancel the
                // FETCH invoke while open_agent_binary is still pending.
                let cancel = CancellationToken::new();
                self.fetches.lock().insert(stream_id, cancel.clone());
                let registration = FetchRegistration {
                    stream_id,
                    fetches: self.fetches.clone(),
                };
                let ret: Result<AgentFetchHeader, Error> =
                    match self.resolver.open_agent_binary(&triple, accept_gzip).await {
                        Ok(mut stream) => {
                            let header = AgentFetchHeader {
                                size: stream.size,
                                raw_size: stream.raw_size,
                                encoding: stream.encoding,
                            };
                            let outbox = self.outbox.clone();
                            tokio::spawn(async move {
                                let _registration = registration;
                                let pump = super::send_chunk_stream(
                                    &outbox,
                                    API_HOST_FETCH_AGENT_CHUNK,
                                    stream_id,
                                    stream.reader.as_mut(),
                                    super::OnReadError::Truncate,
                                );
                                tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => {}
                                    result = pump => {
                                        if let Err(e) = result {
                                            // Stream cut short; the consumer's
                                            // size check turns this into a hard
                                            // error.
                                            log::error!("agent fetch stream failed: {}", e);
                                        }
                                    }
                                }
                            });
                            Ok(header)
                        }
                        Err(e) => {
                            drop(registration);
                            Err(e)
                        }
                    };
                encode(&ret)?
            }
            _ => return Ok(None),
        };
        Ok(Some(ret.into()))
    }

    async fn notify(&self, api: Api, req: bytes::Bytes) -> Result<bool, Error> {
        if api == API_HOST_FETCH_AGENT_CANCEL {
            let stream_id: StreamId = decode(&req[..])?;
            if let Some(cancel) = self.fetches.lock().remove(&stream_id) {
                cancel.cancel();
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
