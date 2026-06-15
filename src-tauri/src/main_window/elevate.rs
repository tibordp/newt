//! Launch an elevated `newt-agent` via UAC (`ShellExecuteEx "runas"`).
//!
//! Unlike Linux `pkexec`, `ShellExecuteEx "runas"` cannot redirect stdio to
//! the elevated child, so the agent speaks RPC over a named pipe instead of
//! stdin/stdout. The host GUI stays unelevated; only the agent is elevated.
//!
//! Security: the pipe name is an unguessable UUID, the server is created
//! with `first_pipe_instance(true)` + `max_instances(1)` (no squatting, one
//! connection ever), and the default named-pipe DACL admits only the
//! creating user + admins. There is no auth handshake (same model as the
//! askpass/conpty named pipes). This protects against *other users*; it
//! does not (and cannot) defend against a same-user attacker, who could
//! already tamper with the unelevated Newt process itself.
//!
//! Windows-only module.

use std::mem::size_of;
use std::time::Duration;

use newt_common::agent_resolver::AgentResolver;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::windows::named_pipe::ServerOptions;
use windows_sys::Win32::Foundation::{ERROR_CANCELLED, GetLastError};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NO_CONSOLE, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use super::win_proc::WinProcess;
use crate::common::Error;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Process + pipe handed back from [`spawn_elevated_windows`]. The caller
/// wires `stdout`/`stdin` into the RPC stream (no stderr — `runas` doesn't
/// redirect it).
pub struct ElevatedSpawn {
    pub process: WinProcess,
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
}

/// Spawn the elevated agent and connect to it over a named pipe.
pub async fn spawn_elevated_windows(
    agent_resolver: &dyn AgentResolver,
) -> Result<ElevatedSpawn, Error> {
    let agent_path = agent_resolver.find_local_agent_binary()?;
    let agent_path = agent_path
        .to_str()
        .ok_or_else(|| Error::Custom("agent path is not valid UTF-8".into()))?
        .to_string();

    let pipe_name = format!(r"\\.\pipe\newt-elevated-{}", uuid::Uuid::new_v4());

    // Create the server before launching so the agent can connect immediately.
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .max_instances(1)
        .create(&pipe_name)
        .map_err(|e| Error::Custom(format!("failed to create elevated pipe: {}", e)))?;

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
            return Err(Error::Custom(if err == ERROR_CANCELLED {
                "Elevation request was declined".into()
            } else {
                format!("ShellExecuteEx(runas) failed (error {})", err)
            }));
        }
        if info.hProcess.is_null() {
            return Err(Error::Custom(
                "elevated agent did not start a process".into(),
            ));
        }
        unsafe { WinProcess::from_raw(info.hProcess) }
    };

    match tokio::time::timeout(Duration::from_secs(30), server.connect()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(Error::Custom(format!(
                "elevated agent failed to connect: {}",
                e
            )));
        }
        Err(_) => {
            return Err(Error::Custom(
                "timed out waiting for the elevated agent to connect".into(),
            ));
        }
    }

    let (rx, tx) = tokio::io::split(server);
    Ok(ElevatedSpawn {
        process,
        stdin: Box::new(tx),
        stdout: Box::new(rx),
    })
}
