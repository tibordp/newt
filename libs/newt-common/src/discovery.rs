//! Stateless enumerators for the Connect dialog's combo-boxes: SSH config
//! hosts, docker/podman containers, kubectl contexts and pods. Failures fold
//! to empty results + a warning string so the dialog can show a single
//! inline hint rather than a blocking error.
//!
//! Symmetric service (like `hot_paths`): the host runs `Local` directly for
//! window-scoped connections; pane-scoped agent mounts run on the session
//! owner, so discovery routes to the agent's `Local` via `Remote` in remote
//! sessions — listing the containers the mount would actually reach.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::Error;
use crate::proc::NoConsoleWindow;
use crate::shell::resolve_program;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

// NOTE: these types cross the agent↔host bincode RPC boundary — no
// `skip_serializing_if` (it desyncs bincode's serialize/deserialize).
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct SshHostEntry {
    pub host: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ContainerEntry {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct KubePodEntry {
    pub namespace: String,
    pub name: String,
    pub containers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct DiscoveryResult<T> {
    pub items: Vec<T>,
    /// Best-effort failure note. When present, the dialog should show this
    /// dimmed under the combo-box instead of an empty list.
    pub warning: Option<String>,
}

impl<T> DiscoveryResult<T> {
    pub fn ok(items: Vec<T>) -> Self {
        Self {
            items,
            warning: None,
        }
    }
    pub fn warn(msg: impl Into<String>) -> Self {
        Self {
            items: Vec::new(),
            warning: Some(msg.into()),
        }
    }
}

#[async_trait::async_trait]
pub trait DiscoveryProvider: Send + Sync {
    async fn ssh_hosts(&self) -> Result<DiscoveryResult<SshHostEntry>, Error>;
    /// `engine` is the CLI program: `docker` or `podman`.
    async fn containers(&self, engine: String) -> Result<DiscoveryResult<ContainerEntry>, Error>;
    async fn kube_contexts(&self) -> Result<DiscoveryResult<String>, Error>;
    async fn kube_pods(
        &self,
        context: Option<String>,
        namespace: Option<String>,
    ) -> Result<DiscoveryResult<KubePodEntry>, Error>;
}

// ---------------------------------------------------------------------------
// Local
// ---------------------------------------------------------------------------

pub struct Local {
    extra_path: Vec<String>,
}

impl Local {
    pub fn new(extra_path: Vec<String>) -> Self {
        Self { extra_path }
    }
}

async fn run_capture(cmd: &mut Command) -> Result<Vec<u8>, String> {
    let fut = cmd.no_console_window().output();
    let out = tokio::time::timeout(DISCOVERY_TIMEOUT, fut)
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("exit {:?}", out.status.code())
        } else {
            stderr
        });
    }
    Ok(out.stdout)
}

fn parse_ssh_config() -> DiscoveryResult<SshHostEntry> {
    let home = match std::env::var_os("HOME") {
        Some(h) => std::path::PathBuf::from(h),
        None => return DiscoveryResult::ok(Vec::new()),
    };
    let path = home.join(".ssh").join("config");
    if !path.is_file() {
        return DiscoveryResult::ok(Vec::new());
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return DiscoveryResult::warn(format!("read {}: {}", path.display(), e)),
    };
    let mut out: Vec<SshHostEntry> = Vec::new();
    let mut current: Option<SshHostEntry> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (
                k.trim().to_ascii_lowercase(),
                v.trim().trim_start_matches('=').trim(),
            ),
            None => continue,
        };
        match key.as_str() {
            "host" => {
                // `Host` lines may name several hosts; emit one entry per name,
                // skipping wildcard patterns (`*`, `?`, `!`) which can't be
                // dialed directly.
                if let Some(e) = current.take()
                    && !e.host.is_empty()
                {
                    out.push(e);
                }
                for name in value.split_whitespace() {
                    if name.chars().any(|c| matches!(c, '*' | '?' | '!')) {
                        continue;
                    }
                    out.push(SshHostEntry {
                        host: name.to_string(),
                        hostname: None,
                        user: None,
                    });
                }
                current = out.pop();
            }
            "hostname" => {
                if let Some(c) = current.as_mut() {
                    c.hostname = Some(value.to_string());
                }
            }
            "user" => {
                if let Some(c) = current.as_mut() {
                    c.user = Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    if let Some(e) = current {
        out.push(e);
    }
    // `Include` directives are not followed.
    DiscoveryResult::ok(out)
}

async fn discover_engine_containers(
    program: &str,
    extra_path: &[String],
) -> DiscoveryResult<ContainerEntry> {
    // Both engines accept `ps --format '{{json .}}'` and emit one JSON object
    // per line (NDJSON), even on engines too old to support `--format=json`.
    let resolved = resolve_program(program, extra_path);
    let mut cmd = Command::new(&resolved);
    cmd.args(["ps", "-a", "--no-trunc", "--format", "{{json .}}"]);
    let out = match run_capture(&mut cmd).await {
        Ok(o) => o,
        Err(e) => {
            log::info!("discovery: {} ps failed: {}", program, e);
            return DiscoveryResult::warn(format!("{} ps: {}", program, e));
        }
    };
    let text = String::from_utf8_lossy(&out);
    log::info!(
        "discovery: {} ps returned {} bytes / {} lines",
        program,
        text.len(),
        text.lines().filter(|l| !l.trim().is_empty()).count()
    );
    let mut items = Vec::new();
    let mut parse_failures = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                parse_failures += 1;
                log::debug!("discovery: {} ps json parse error: {}", program, e);
                continue;
            }
        };
        items.push(ContainerEntry {
            id: v
                .get("ID")
                .or_else(|| v.get("Id"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("Names")
                .or_else(|| v.get("Name"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            image: v
                .get("Image")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            state: v
                .get("State")
                .or_else(|| v.get("Status"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    let warning = if parse_failures > 0 {
        Some(format!("{} entries failed to parse", parse_failures))
    } else {
        None
    };
    DiscoveryResult { items, warning }
}

#[async_trait::async_trait]
impl DiscoveryProvider for Local {
    async fn ssh_hosts(&self) -> Result<DiscoveryResult<SshHostEntry>, Error> {
        Ok(parse_ssh_config())
    }

    async fn containers(&self, engine: String) -> Result<DiscoveryResult<ContainerEntry>, Error> {
        // Only known engine CLIs — this arrives over RPC in remote sessions,
        // so it must not become a run-anything endpoint.
        if engine != "docker" && engine != "podman" {
            return Err(Error::custom(format!(
                "unknown container engine: {}",
                engine
            )));
        }
        Ok(discover_engine_containers(&engine, &self.extra_path).await)
    }

    async fn kube_contexts(&self) -> Result<DiscoveryResult<String>, Error> {
        let resolved = resolve_program("kubectl", &self.extra_path);
        let mut cmd = Command::new(&resolved);
        cmd.args(["config", "get-contexts", "-o", "name"]);
        let out = match run_capture(&mut cmd).await {
            Ok(o) => o,
            Err(e) => return Ok(DiscoveryResult::warn(format!("kubectl: {}", e))),
        };
        let text = String::from_utf8_lossy(&out);
        let items = text
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(DiscoveryResult::ok(items))
    }

    async fn kube_pods(
        &self,
        context: Option<String>,
        namespace: Option<String>,
    ) -> Result<DiscoveryResult<KubePodEntry>, Error> {
        // Global flags need to follow the subcommand on some kubectl wrappers
        // (notably orbstack's): "flags cannot be placed before plugin name". Put
        // `get pods` first, then `--context`/`--namespace`/`-o json`.
        let resolved = resolve_program("kubectl", &self.extra_path);
        let mut cmd = Command::new(&resolved);
        cmd.args(["get", "pods", "-o", "json"]);
        if let Some(c) = &context {
            cmd.arg(format!("--context={}", c));
        }
        match &namespace {
            Some(ns) if ns != "*" => {
                cmd.arg(format!("--namespace={}", ns));
            }
            _ => {
                cmd.arg("--all-namespaces");
            }
        }
        let out = match run_capture(&mut cmd).await {
            Ok(o) => o,
            Err(e) => return Ok(DiscoveryResult::warn(format!("kubectl: {}", e))),
        };
        let json: serde_json::Value = match serde_json::from_slice(&out) {
            Ok(v) => v,
            Err(e) => return Ok(DiscoveryResult::warn(format!("parse: {}", e))),
        };
        let mut items = Vec::new();
        if let Some(arr) = json.get("items").and_then(|v| v.as_array()) {
            for it in arr {
                let name = it
                    .pointer("/metadata/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let ns = it
                    .pointer("/metadata/namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let containers = it
                    .pointer("/spec/containers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|c| {
                                c.get("name").and_then(|n| n.as_str()).map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !name.is_empty() {
                    items.push(KubePodEntry {
                        namespace: ns,
                        name,
                        containers,
                    });
                }
            }
        }
        Ok(DiscoveryResult::ok(items))
    }
}

// ---------------------------------------------------------------------------
// Remote
// ---------------------------------------------------------------------------

pub struct Remote {
    communicator: crate::rpc::Communicator,
}

impl Remote {
    pub fn new(communicator: crate::rpc::Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl DiscoveryProvider for Remote {
    async fn ssh_hosts(&self) -> Result<DiscoveryResult<SshHostEntry>, Error> {
        let ret: Result<DiscoveryResult<SshHostEntry>, Error> = self
            .communicator
            .invoke(crate::api::API_DISCOVER_SSH_HOSTS, &())
            .await?;
        ret
    }

    async fn containers(&self, engine: String) -> Result<DiscoveryResult<ContainerEntry>, Error> {
        let ret: Result<DiscoveryResult<ContainerEntry>, Error> = self
            .communicator
            .invoke(crate::api::API_DISCOVER_CONTAINERS, &engine)
            .await?;
        ret
    }

    async fn kube_contexts(&self) -> Result<DiscoveryResult<String>, Error> {
        let ret: Result<DiscoveryResult<String>, Error> = self
            .communicator
            .invoke(crate::api::API_DISCOVER_KUBE_CONTEXTS, &())
            .await?;
        ret
    }

    async fn kube_pods(
        &self,
        context: Option<String>,
        namespace: Option<String>,
    ) -> Result<DiscoveryResult<KubePodEntry>, Error> {
        let ret: Result<DiscoveryResult<KubePodEntry>, Error> = self
            .communicator
            .invoke(crate::api::API_DISCOVER_KUBE_PODS, &(context, namespace))
            .await?;
        ret
    }
}
