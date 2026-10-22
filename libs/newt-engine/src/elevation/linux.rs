//! Elevation on Linux: `pkexec <agent>`. Polkit's agent shows the dialog, and
//! RPC runs over the agent's stdin/stdout. pkexec also scrubs the
//! environment, so the agent needs nothing of its own.

use std::process::Stdio;

use crate::agent_resolver::AgentResolver;
use crate::proc::set_parent_death_signal;
use newt_common::Error;

use super::ElevatedAgent;

pub struct AgentProcess(tokio::process::Child);

impl AgentProcess {
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait().await
    }
}

pub async fn spawn(agent_resolver: &dyn AgentResolver) -> Result<ElevatedAgent, Error> {
    let agent_path = agent_resolver.find_local_agent_binary()?;
    #[cfg(feature = "elevate-agent-copy")]
    let agent_path = stage_agent_for_root(agent_resolver, &agent_path).await?;
    let mut cmd = tokio::process::Command::new("pkexec");
    cmd.arg(&agent_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    set_parent_death_signal(&mut cmd);
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        cmd.env("RUST_LOG", rust_log);
    }
    let mut child = cmd.spawn()?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    Ok(ElevatedAgent {
        stream: tokio_duplex::Duplex::new(Box::new(stdout), Box::new(stdin)),
        stderr: Some(Box::new(stderr)),
        process: AgentProcess(child),
    })
}

/// Copy the agent where root can run it. The AppImage runs from its user's
/// FUSE mount, which root can't see, so `pkexec` can't start the agent in
/// place. The copy goes to the user's runtime dir (a private tmpfs, cleared
/// at logout), named by the agents' hash so one build stages it once.
#[cfg(feature = "elevate-agent-copy")]
async fn stage_agent_for_root(
    agent_resolver: &dyn AgentResolver,
    agent: &std::path::Path,
) -> Result<std::path::PathBuf, Error> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        Error::custom("an elevated session needs XDG_RUNTIME_DIR to stage the agent")
    })?;
    let dir = std::path::PathBuf::from(runtime_dir).join("newt");
    tokio::fs::create_dir_all(&dir).await?;
    let name = format!("newt-agent-{}", agent_resolver.agent_hash().await?);
    let staged = dir.join(&name);
    if !tokio::fs::try_exists(&staged).await? {
        let partial = dir.join(format!(".{name}.{}", std::process::id()));
        tokio::fs::copy(agent, &partial).await?;
        tokio::fs::rename(&partial, &staged).await?;
    }
    Ok(staged)
}
