//! Elevation on Windows: UAC (`ShellExecuteEx "runas"`).
//!
//! Unlike Linux `pkexec`, `ShellExecuteEx "runas"` cannot redirect stdio to
//! the elevated child, so the agent speaks RPC over a named pipe instead of
//! stdin/stdout. The host GUI stays unelevated; only the agent is elevated.
//!
//! Security: the pipe name is an unguessable UUID, the server is created
//! with `first_pipe_instance(true)` + `max_instances(1)` (no squatting, one
//! connection ever), and `pipe::create`'s protected DACL admits only
//! SYSTEM, Administrators and the creating user. There is no auth handshake (same model as the
//! askpass/conpty named pipes). This protects against *other users*; it
//! does not (and cannot) defend against a same-user attacker, who could
//! already tamper with the unelevated Newt process itself.

use std::mem::size_of;
use std::time::Duration;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, ServerOptions};
use windows_sys::Win32::Foundation::{ERROR_CANCELLED, GetLastError};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NO_CONSOLE, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::Error;
use crate::agent_resolver::AgentResolver;
use crate::win_proc::WinProcess;

use super::ElevatedAgent;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// --- Host side ---

pub struct AgentProcess(WinProcess);

impl AgentProcess {
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait().await
    }
}

/// Spawn the elevated agent and connect to it over a named pipe.
pub async fn spawn(agent_resolver: &dyn AgentResolver) -> Result<ElevatedAgent, Error> {
    let agent_path = agent_resolver.find_local_agent_binary()?;
    let agent_path = agent_path
        .to_str()
        .ok_or_else(|| Error::custom("agent path is not valid UTF-8"))?
        .to_string();

    let pipe_name = crate::pipe::unique_name("newt-elevated");

    // Create the server before launching so the agent can connect immediately.
    let server = crate::pipe::create(
        ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1),
        &pipe_name,
    )
    .map_err(|e| Error::custom(format!("failed to create elevated pipe: {}", e)))?;

    // ShellExecuteEx "runas" → UAC consent prompt; the agent reaches us via
    // `--pipe`. Scoped to a sync block so the raw-pointer-bearing
    // `SHELLEXECUTEINFOW` is dropped before the `.await` below, otherwise the
    // future would be `!Send`. Dropping the returned `WinProcess` (e.g. on a
    // failed connect) terminates the orphaned elevated agent.
    let process = {
        let verb = wide("runas");
        let file = wide(&agent_path);
        let params = wide(&format!("--pipe {}", pipe_name));

        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;

        let ok = unsafe { ShellExecuteExW(&mut info) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(Error::custom(if err == ERROR_CANCELLED {
                "Elevation request was declined".to_string()
            } else {
                format!("ShellExecuteEx(runas) failed (error {})", err)
            }));
        }
        if info.hProcess.is_null() {
            return Err(Error::custom("elevated agent did not start a process"));
        }
        unsafe { WinProcess::from_raw(info.hProcess) }
    };

    match tokio::time::timeout(Duration::from_secs(30), server.connect()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(Error::custom(format!(
                "elevated agent failed to connect: {}",
                e
            )));
        }
        Err(_) => {
            return Err(Error::custom(
                "timed out waiting for the elevated agent to connect",
            ));
        }
    }

    let (rx, tx) = tokio::io::split(server);
    Ok(ElevatedAgent {
        stream: tokio_duplex::Duplex::new(Box::new(rx), Box::new(tx)),
        stderr: None,
        process: AgentProcess(process),
    })
}

// --- Agent side ---

/// Connect to the host's named pipe, retrying briefly while it's busy
/// (ERROR_PIPE_BUSY = 231) — the host creates the server right before
/// launching us, so a short race is expected.
pub async fn connect(name: &str) -> Result<NamedPipeClient, Error> {
    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..200 {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => {
                return Err(Error::custom(format!(
                    "failed to open named pipe {:?}: {}",
                    name, e
                )));
            }
        }
    }
    Err(Error::custom(format!(
        "timed out connecting to named pipe {:?}",
        name
    )))
}
