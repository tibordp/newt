//! Shell integration: the `newt` CLI inside built-in terminals remote-controls
//! the owning session over per-session HTTP (Unix domain socket / Windows
//! named pipe). See `design_docs/DESIGN_SHELL_INTEGRATION.md`.
//!
//! Unlike the host↔agent RPC, this protocol crosses versions: shells outlive
//! app restarts and upgrades, so unknown routes and malformed requests are
//! answered with HTTP errors, never panics.

use serde::{Deserialize, Serialize};

use crate::filesystem::ByteStream;
use crate::vfs::VfsPath;

mod cli;
mod server;

pub use cli::{VERBS, is_cli_invocation, run_cli};
pub use server::ShellIntegration;

pub const ENV_SOCK: &str = "NEWT_SHELL_SOCK";
pub const ENV_TERMINAL: &str = "NEWT_TERMINAL";
/// Set by the Windows `newt.cmd` shim, where argv[0] can't be `newt`.
pub const ENV_CLI: &str = "NEWT_CLI";

// ---------------------------------------------------------------------------
// Control-plane types. These also ride API_HOST_SHELL_CONTROL (bincode)
// between agent and host, where normal internal-ABI rules apply.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneSelector {
    Active,
    Other,
    Left,
    Right,
}

impl PaneSelector {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "other" => Some(Self::Other),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlRequest {
    Pwd {
        pane: PaneSelector,
    },
    /// `cd` and `focus`: non-strict navigate (a leaf path lands on the
    /// parent with the entry focused).
    Navigate {
        pane: PaneSelector,
        path: String,
        cwd: String,
    },
    /// Tier-1 registry command dispatch (same ids as keybindings/palette).
    Command {
        pane: PaneSelector,
        id: String,
    },
    ListCommands,
    /// Resolve a path argument to a VfsPath (data plane for `cat` reads the
    /// result on the session side that owns the VFS registry).
    ResolveFile {
        pane: PaneSelector,
        path: String,
        cwd: String,
    },
    /// Open the built-in viewer (or editor) on the host.
    Open {
        pane: PaneSelector,
        path: String,
        cwd: String,
        edit: bool,
    },
    /// `cp` / `mv` through the operations framework.
    Transfer {
        move_files: bool,
        sources: Vec<String>,
        dest: String,
        cwd: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandListEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlResponse {
    Ok,
    Text(String),
    Commands(Vec<CommandListEntry>),
    ResolvedFile(VfsPath),
}

pub type ControlResult = Result<ControlResponse, String>;

/// Session-side verb handler. The control plane always reaches the host
/// (directly in a local session, via API_HOST_SHELL_CONTROL from the agent);
/// the data plane reads on whichever side owns the session's VFS registry.
#[async_trait::async_trait]
pub trait ShellControlHandler: Send + Sync + 'static {
    async fn control(&self, req: ControlRequest) -> ControlResult;
    async fn read_file(&self, path: VfsPath) -> Result<ByteStream, String>;
}
