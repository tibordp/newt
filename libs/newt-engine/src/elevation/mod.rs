//! Elevated sessions: a root (administrator) agent on this machine, started
//! through the platform's own authorization prompt — pkexec on Linux,
//! Authorization Services on macOS, UAC on Windows. Each platform module
//! holds both sides of its launch: how the host starts the agent, and what
//! the agent does with what it was started with.

use crate::connect::{DynStderr, DynStream};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{AgentProcess, spawn};

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::{AgentProcess, spawn};

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::{AgentProcess, spawn};

/// A started elevated agent, ready for RPC.
pub struct ElevatedAgent {
    pub stream: DynStream,
    /// `None` on Windows: `runas` carries no stdio.
    pub stderr: Option<DynStderr>,
    pub process: AgentProcess,
}
