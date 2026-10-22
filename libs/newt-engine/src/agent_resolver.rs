use std::sync::Arc;

use newt_common::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentEncoding {
    Raw,
    Gzip,
}

impl AgentEncoding {
    /// Wire name used in the bootstrap transfer header.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentEncoding::Raw => "raw",
            AgentEncoding::Gzip => "gzip",
        }
    }
}

/// A sized byte stream of an agent binary, ready to splice into a
/// bootstrap upload (`<size> <encoding>\n` + bytes) or write to disk.
pub struct AgentStream {
    /// Bytes on the wire (compressed size when `encoding` is gzip).
    pub size: u64,
    /// Uncompressed size, for log lines.
    pub raw_size: u64,
    pub encoding: AgentEncoding,
    pub reader: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
}

/// Package agent-binary bytes as an `AgentStream`, gzip-compressing when
/// the consumer can decode it.
pub fn agent_stream_from_bytes(data: Vec<u8>, accept_gzip: bool) -> Result<AgentStream, Error> {
    let raw_size = data.len() as u64;
    if accept_gzip {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&data)?;
        let compressed = encoder.finish()?;
        Ok(AgentStream {
            size: compressed.len() as u64,
            raw_size,
            encoding: AgentEncoding::Gzip,
            reader: Box::new(std::io::Cursor::new(compressed)),
        })
    } else {
        Ok(AgentStream {
            size: raw_size,
            raw_size,
            encoding: AgentEncoding::Raw,
            reader: Box::new(std::io::Cursor::new(data)),
        })
    }
}

#[async_trait::async_trait]
pub trait AgentResolver: Send + Sync {
    /// Hash keying the agent cache on bootstrap targets. For RPC-backed
    /// resolvers this is the *host's* hash (fetched once per session), so
    /// cache keys stay consistent regardless of which side supplied the
    /// bytes.
    async fn agent_hash(&self) -> Result<String, Error>;
    fn find_agent_binary(&self, triple: &str) -> Result<std::path::PathBuf, Error>;
    fn find_local_agent_binary(&self) -> Result<std::path::PathBuf, Error>;

    /// Open the agent binary for `triple` as a sized stream, compressing
    /// when the consumer can decode it. Default: read the file
    /// `find_agent_binary` points at.
    async fn open_agent_binary(
        &self,
        triple: &str,
        accept_gzip: bool,
    ) -> Result<AgentStream, Error> {
        let path = self.find_agent_binary(triple)?;
        let data = tokio::fs::read(&path).await?;
        agent_stream_from_bytes(data, accept_gzip)
    }

    /// A local file containing the binary, for copy-based transports
    /// (`docker cp`). Default: the resolved path itself; RPC-backed
    /// resolvers download foreign triples into a hash-keyed cache.
    async fn materialize_agent_binary(&self, triple: &str) -> Result<std::path::PathBuf, Error> {
        self.find_agent_binary(triple)
    }
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

/// File name of the agent binary for `triple`. Windows targets carry the
/// `.exe` extension; every other platform does not. Derived from the
/// *triple's* OS, not the host's — a Windows host bootstrapping a Linux
/// remote still reads a plain `newt-agent`.
pub fn agent_file_name(triple: &str) -> &'static str {
    if triple.contains("windows") {
        "newt-agent.exe"
    } else {
        "newt-agent"
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
/// by the agent; foreign-arch sub-agent bootstrap is not implemented.
pub struct CurrentExeAgentResolver;

impl CurrentExeAgentResolver {
    pub fn new() -> Self {
        Self
    }

    fn current_exe() -> Result<std::path::PathBuf, Error> {
        std::env::current_exe().map_err(Error::from)
    }
}

impl Default for CurrentExeAgentResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AgentResolver for CurrentExeAgentResolver {
    async fn agent_hash(&self) -> Result<String, Error> {
        let path = Self::current_exe()?;
        let bytes = tokio::fs::read(&path).await?;
        let hash = blake3::Hasher::new().update(&bytes).finalize();
        Ok(hash.to_hex()[..16].to_string())
    }

    fn find_agent_binary(&self, triple: &str) -> Result<std::path::PathBuf, Error> {
        if triple == local_agent_triple() {
            Self::current_exe()
        } else {
            Err(Error::custom(format!(
                "agent does not have binary for triple {} (host connection required)",
                triple
            )))
        }
    }

    fn find_local_agent_binary(&self) -> Result<std::path::PathBuf, Error> {
        Self::current_exe()
    }
}

// ---------------------------------------------------------------------------
// Remote — RPC-backed resolver used by the agent for nested spawns
// ---------------------------------------------------------------------------

/// Where an agent caches binaries it materializes for copy-based nested
/// spawns. Mirrors the bootstrap script's cache location: with no cache
/// directory, a private one made for this process, since a fixed name in
/// the temporary directory could be one another user made first.
fn agent_cache_dir() -> Result<std::path::PathBuf, Error> {
    if let Some(dir) = dirs::cache_dir() {
        return Ok(dir.join("newt"));
    }
    static PRIVATE: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    PRIVATE
        .get_or_init(|| {
            tempfile::Builder::new()
                .prefix("newt-")
                .tempdir()
                .ok()
                .map(tempfile::TempDir::keep)
        })
        .clone()
        .ok_or_else(|| Error::custom("no directory to cache the agent in"))
}

/// `AgentResolver` that serves its own triple from the running executable
/// and fetches foreign triples from the host over `API_HOST_FETCH_AGENT`.
/// Reports the *host's* agent hash. Used by the agent for nested spawns
/// (pane-scoped agent mounts on a remote session).
pub struct Remote {
    communicator: Arc<std::sync::OnceLock<newt_rpc::Communicator>>,
    cached_hash: tokio::sync::OnceCell<String>,
    cache_dir: Option<std::path::PathBuf>,
    local: CurrentExeAgentResolver,
}

impl Remote {
    pub fn new(communicator: Arc<std::sync::OnceLock<newt_rpc::Communicator>>) -> Self {
        Self {
            communicator,
            cached_hash: tokio::sync::OnceCell::new(),
            cache_dir: None,
            local: CurrentExeAgentResolver::new(),
        }
    }

    /// Override the materialize cache location (tests).
    pub fn with_cache_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.cache_dir = Some(dir);
        self
    }

    fn communicator(&self) -> Result<newt_rpc::Communicator, Error> {
        self.communicator
            .get()
            .cloned()
            .ok_or_else(|| Error::custom("host communicator not available"))
    }
}

#[async_trait::async_trait]
impl AgentResolver for Remote {
    async fn agent_hash(&self) -> Result<String, Error> {
        self.cached_hash
            .get_or_try_init(|| async {
                let ret: Result<String, Error> = self
                    .communicator()?
                    .invoke(crate::api::API_HOST_AGENT_HASH, &())
                    .await?;
                ret
            })
            .await
            .cloned()
    }

    fn find_agent_binary(&self, triple: &str) -> Result<std::path::PathBuf, Error> {
        self.local.find_agent_binary(triple)
    }

    fn find_local_agent_binary(&self) -> Result<std::path::PathBuf, Error> {
        self.local.find_local_agent_binary()
    }

    async fn open_agent_binary(
        &self,
        triple: &str,
        accept_gzip: bool,
    ) -> Result<AgentStream, Error> {
        // Self fast path: our own triple never crosses the wire.
        if triple == local_agent_triple() {
            let data = tokio::fs::read(CurrentExeAgentResolver::current_exe()?).await?;
            return agent_stream_from_bytes(data, accept_gzip);
        }

        let communicator = self.communicator()?;
        let (stream_id, data) = communicator.open_stream();
        let header: Result<crate::api::AgentFetchHeader, Error> = communicator
            .invoke(
                crate::api::API_HOST_FETCH_AGENT,
                &(triple.to_string(), accept_gzip, stream_id),
            )
            .await?;
        let header = header?;

        Ok(AgentStream {
            size: header.size,
            raw_size: header.raw_size,
            encoding: header.encoding,
            reader: Box::new(data.into_async_read()),
        })
    }

    async fn materialize_agent_binary(&self, triple: &str) -> Result<std::path::PathBuf, Error> {
        if triple == local_agent_triple() {
            return CurrentExeAgentResolver::current_exe();
        }

        let hash = self.agent_hash().await?;
        let cache_dir = match self.cache_dir.clone() {
            Some(dir) => dir,
            None => agent_cache_dir()?,
        };
        tokio::fs::create_dir_all(&cache_dir).await?;
        let dest = cache_dir.join(format!("newt-agent-{}-{}", hash, triple));
        if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
            // Hash-keyed and content-immutable — a hit is always current.
            return Ok(dest);
        }

        let mut stream = self.open_agent_binary(triple, false).await?;
        let tmp = cache_dir.join(format!(
            "newt-agent-{}-{}.tmp.{}",
            hash,
            triple,
            std::process::id()
        ));
        let mut file = tokio::fs::File::create(&tmp).await?;
        let copied = tokio::io::copy(&mut stream.reader, &mut file).await?;
        file.sync_all().await?;
        drop(file);
        if copied != stream.size {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(Error::custom(format!(
                "agent download truncated: {} of {} bytes",
                copied, stream.size
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).await?;
        }
        tokio::fs::rename(&tmp, &dest).await?;
        Ok(dest)
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

#[cfg(test)]
#[path = "agent_resolver_tests.rs"]
mod remote_tests;
