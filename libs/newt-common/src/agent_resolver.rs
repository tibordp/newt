use std::path::PathBuf;

use crate::Error;

pub trait AgentResolver: Send + Sync {
    fn agent_hash(&self) -> Result<String, Error>;
    fn find_agent_binary(&self, triple: &str) -> Result<PathBuf, Error>;
    fn find_local_agent_binary(&self) -> Result<PathBuf, Error>;
}

/// The agent triple this binary was compiled for. On Linux we always pair
/// with musl agents (`<arch>-unknown-linux-gnu` host → `<arch>-unknown-linux-musl`
/// agent), matching the cross-compile target produced by `cargo-zigbuild`.
pub fn local_agent_triple() -> String {
    let target = env!("NEWT_TARGET_TRIPLE");
    if let Some(prefix) = target.strip_suffix("-gnu") {
        format!("{}-musl", prefix)
    } else {
        target.to_string()
    }
}

/// Resolver that only knows how to produce its own running executable. Used
/// by the agent. A future revision will add an RPC fallback that fetches a
/// foreign-arch agent binary from the host.
pub struct CurrentExeAgentResolver;

impl CurrentExeAgentResolver {
    pub fn new() -> Self {
        Self
    }

    fn current_exe() -> Result<PathBuf, Error> {
        std::env::current_exe().map_err(Error::from)
    }
}

impl Default for CurrentExeAgentResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentResolver for CurrentExeAgentResolver {
    fn agent_hash(&self) -> Result<String, Error> {
        let path = Self::current_exe()?;
        let bytes = std::fs::read(&path)?;
        let hash = blake3::Hasher::new().update(&bytes).finalize();
        Ok(hash.to_hex()[..16].to_string())
    }

    fn find_agent_binary(&self, triple: &str) -> Result<PathBuf, Error> {
        if triple == local_agent_triple() {
            Self::current_exe()
        } else {
            Err(Error::custom(format!(
                "agent does not have binary for triple {} (sub-agent bootstrap not yet implemented)",
                triple
            )))
        }
    }

    fn find_local_agent_binary(&self) -> Result<PathBuf, Error> {
        Self::current_exe()
    }
}
