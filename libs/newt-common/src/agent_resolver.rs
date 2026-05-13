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

/// Map an OS+arch pair (as reported by `uname -s -m` or `docker inspect`) to
/// our agent target triple. Mirrors the `case` tables in `scripts/bootstrap.sh`
/// — keep the two in sync. Accepts the common synonyms (`amd64`/`x86_64`,
/// `arm64`/`aarch64`, `linux`/`Linux`).
///
/// Returns `None` for unsupported combinations rather than constructing an
/// invalid triple, so callers can surface a clean error message.
pub fn triple_from_os_arch(os: &str, arch: &str) -> Option<String> {
    let os_part = match os.to_ascii_lowercase().as_str() {
        "linux" => "unknown-linux-musl",
        "darwin" | "macos" | "mac" => "apple-darwin",
        _ => return None,
    };
    let arch_part = match arch.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => return None,
    };
    Some(format!("{}-{}", arch_part, os_part))
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

#[cfg(test)]
mod tests {
    use super::triple_from_os_arch;

    #[test]
    fn known_combinations() {
        assert_eq!(
            triple_from_os_arch("linux", "x86_64").as_deref(),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            triple_from_os_arch("linux", "amd64").as_deref(),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            triple_from_os_arch("Linux", "arm64").as_deref(),
            Some("aarch64-unknown-linux-musl")
        );
        assert_eq!(
            triple_from_os_arch("Darwin", "aarch64").as_deref(),
            Some("aarch64-apple-darwin")
        );
    }

    #[test]
    fn unknown_combinations() {
        assert_eq!(triple_from_os_arch("windows", "x86_64"), None);
        assert_eq!(triple_from_os_arch("linux", "riscv64"), None);
    }
}
