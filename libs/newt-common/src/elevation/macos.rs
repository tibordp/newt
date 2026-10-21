//! Elevation on macOS. The host starts the agent with Authorization Services'
//! `AuthorizationExecuteWithPrivileges`: the system's own dialog asks for an
//! administrator's password, and the setuid `security_authtrampoline` execs
//! the agent. The call has been deprecated since 10.7, so it is looked up at
//! runtime, and a system without it fails this one transport. The agent it
//! starts:
//!
//! - is the host's own child (the call forks, the trampoline execs in place),
//!   but the call doesn't say which pid;
//! - has fds 0 and 1 on one end of an `AF_UNIX` socketpair, whose other end
//!   the call returns;
//! - has fd 2 and up, and the whole environment, as the host had them, plus
//!   the trampoline's `__AUTHORIZATION` (naming an inherited fd that holds the
//!   authorization) and `_BASH_IMPLICIT_DASH_PEE`;
//! - runs with effective uid 0 and the user's real uid.
//!
//! So before RPC starts the two sides trade a preamble over the socket: the
//! host passes the write end of a stderr pipe (`SCM_RIGHTS`), and the agent,
//! once it has finished becoming root, answers with its pid.

use std::ffi::{CStr, CString, c_char, c_void};
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::ptr::null_mut;

use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, recvmsg, sendmsg};
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::Error;
use crate::agent_resolver::AgentResolver;

use super::ElevatedAgent;

const PROMPT: &CStr = c"Newt wants to open an elevated session.";

// --- Host side ---

/// The agent's process, reaped by pid: it is our child, but can't be adopted
/// into a `tokio::process::Child`.
pub struct AgentProcess {
    pid: libc::pid_t,
    sigchld: Signal,
}

impl AgentProcess {
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        loop {
            let mut status = 0;
            match unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) } {
                0 => {}
                -1 => return Err(std::io::Error::last_os_error()),
                _ => return Ok(std::process::ExitStatus::from_raw(status)),
            }
            // Registered before the agent was started, so an exit that lands
            // between the `waitpid` above and this await still wakes us.
            self.sigchld.recv().await;
        }
    }
}

/// Start the agent as root, prompting for an administrator's password.
/// Blocks a blocking-pool thread while the dialog is up; the dialog can't be
/// cancelled from our side.
pub async fn spawn(agent_resolver: &dyn AgentResolver) -> Result<ElevatedAgent, Error> {
    let agent = agent_resolver.find_local_agent_binary()?;
    let execute = execute_with_privileges()
        .ok_or_else(|| Error::custom("elevated mode is not supported on this version of macOS"))?;
    let agent = CString::new(agent.as_os_str().as_bytes())
        .map_err(|_| Error::custom("agent path contains a NUL byte"))?;
    let sigchld = signal(SignalKind::child())?;

    let (pid, socket, stderr) =
        tokio::task::spawn_blocking(move || launch(execute, &agent)).await??;

    socket.set_nonblocking(true)?;
    // Owned halves, because dropping the write half shuts the socket down
    // rather than just closing our descriptor. The call hands back its end of
    // the socketpair inheritable, so a child spawned elsewhere in the process
    // before we took a close-on-exec copy can hold it open — and the agent
    // only exits once it reads EOF.
    let (rx, tx) = tokio::net::UnixStream::from_std(socket)?.into_split();
    let stderr = tokio::net::unix::pipe::Receiver::from_owned_fd(stderr)?;
    Ok(ElevatedAgent {
        stream: tokio_duplex::Duplex::new(Box::new(rx), Box::new(tx)),
        stderr: Some(Box::new(stderr)),
        process: AgentProcess { pid, sigchld },
    })
}

fn launch(
    execute: ExecuteWithPrivileges,
    agent: &CStr,
) -> Result<(libc::pid_t, UnixStream, OwnedFd), Error> {
    let authorization = authorize()?;
    let argv = [AGENT_FLAG.as_ptr().cast_mut(), null_mut()];
    let mut pipe: *mut libc::FILE = null_mut();
    let status = unsafe { execute(authorization.0, agent.as_ptr(), 0, argv.as_ptr(), &mut pipe) };
    check(status)?;

    let fd = unsafe { libc::fcntl(libc::fileno(pipe), libc::F_DUPFD_CLOEXEC, 0) };
    let dup = (fd >= 0)
        .then(|| unsafe { UnixStream::from_raw_fd(fd) })
        .ok_or_else(std::io::Error::last_os_error);
    unsafe { libc::fclose(pipe) };
    let mut socket = dup?;

    let (stderr, stderr_tx) = std::io::pipe()?;
    send_stderr(&socket, &stderr_tx)?;
    drop(stderr_tx);

    let mut pid = [0; size_of::<libc::pid_t>()];
    socket.read_exact(&mut pid)?;
    Ok((libc::pid_t::from_ne_bytes(pid), socket, stderr.into()))
}

fn send_stderr(socket: &UnixStream, stderr: &impl AsRawFd) -> nix::Result<()> {
    sendmsg::<()>(
        socket.as_raw_fd(),
        &[IoSlice::new(&[0])],
        &[ControlMessage::ScmRights(&[stderr.as_raw_fd()])],
        MsgFlags::empty(),
        None,
    )
    .map(drop)
}

/// The agent's command-line flag for a launch through [`spawn`].
const AGENT_FLAG: &CStr = c"--macos-elevated";

type OSStatus = i32;
type AuthorizationRef = *mut c_void;

#[repr(C)]
struct AuthorizationItem {
    name: *const c_char,
    value_length: usize,
    value: *mut c_void,
    flags: u32,
}

#[repr(C)]
struct AuthorizationItemSet {
    count: u32,
    items: *mut AuthorizationItem,
}

const FLAG_INTERACTION_ALLOWED: u32 = 1 << 0;
const FLAG_EXTEND_RIGHTS: u32 = 1 << 1;
const FLAG_PRE_AUTHORIZE: u32 = 1 << 4;
const ERR_AUTHORIZATION_CANCELED: OSStatus = -60006;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn AuthorizationCreate(
        rights: *const AuthorizationItemSet,
        environment: *const AuthorizationItemSet,
        flags: u32,
        authorization: *mut AuthorizationRef,
    ) -> OSStatus;
    fn AuthorizationFree(authorization: AuthorizationRef, flags: u32) -> OSStatus;
}

type ExecuteWithPrivileges = unsafe extern "C" fn(
    authorization: AuthorizationRef,
    path_to_tool: *const c_char,
    options: u32,
    arguments: *const *mut c_char,
    communications_pipe: *mut *mut libc::FILE,
) -> OSStatus;

fn execute_with_privileges() -> Option<ExecuteWithPrivileges> {
    let symbol = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"AuthorizationExecuteWithPrivileges".as_ptr(),
        )
    };
    (!symbol.is_null())
        .then(|| unsafe { std::mem::transmute::<*mut c_void, ExecuteWithPrivileges>(symbol) })
}

struct Authorization(AuthorizationRef);

impl Drop for Authorization {
    fn drop(&mut self) {
        unsafe { AuthorizationFree(self.0, 0) };
    }
}

/// Obtain the right to run a tool as root, showing the dialog. The right is
/// pre-authorized here, so the trampoline's own check doesn't prompt again.
fn authorize() -> Result<Authorization, Error> {
    let mut right = AuthorizationItem {
        name: c"system.privilege.admin".as_ptr(),
        value_length: 0,
        value: null_mut(),
        flags: 0,
    };
    let rights = AuthorizationItemSet {
        count: 1,
        items: &mut right,
    };
    let mut prompt = AuthorizationItem {
        name: c"prompt".as_ptr(),
        value_length: PROMPT.count_bytes(),
        value: PROMPT.as_ptr().cast_mut().cast(),
        flags: 0,
    };
    let environment = AuthorizationItemSet {
        count: 1,
        items: &mut prompt,
    };
    let mut authorization = null_mut();
    let status = unsafe {
        AuthorizationCreate(
            &rights,
            &environment,
            FLAG_INTERACTION_ALLOWED | FLAG_EXTEND_RIGHTS | FLAG_PRE_AUTHORIZE,
            &mut authorization,
        )
    };
    check(status)?;
    Ok(Authorization(authorization))
}

fn check(status: OSStatus) -> Result<(), Error> {
    match status {
        0 => Ok(()),
        ERR_AUTHORIZATION_CANCELED => Err(Error::custom("Elevation request was declined")),
        _ => Err(Error::custom(format!(
            "authorization failed (OSStatus {})",
            status
        ))),
    }
}

// --- Agent side ---

/// Finish becoming root in an agent started by [`spawn`], and answer the
/// host's preamble. Call first thing in `main`: it reads the preamble off
/// fd 0 before anything else does, and rewrites the environment, which is
/// only sound while the process is single-threaded.
pub fn adopt() -> std::io::Result<()> {
    let stderr = receive_stderr(0);
    if unsafe { libc::dup2(stderr.as_raw_fd(), 2) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    drop(stderr);

    if let Some(fd) = std::env::var("__AUTHORIZATION")
        .ok()
        .and_then(|v| v.strip_prefix("auth ")?.parse::<RawFd>().ok())
    {
        unsafe { libc::close(fd) };
    }

    // With only the effective uid switched, shells drop back to the real one
    // (zsh always; bash unless `-p`, which `_BASH_IMPLICIT_DASH_PEE` asks
    // of Apple's), and the supplementary groups are still the user's.
    let root = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(0))?
        .ok_or_else(|| std::io::Error::other("no passwd entry for uid 0"))?;
    let name = CString::new(root.name.as_bytes())?;
    if unsafe { libc::initgroups(name.as_ptr(), 0) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    nix::unistd::setgid(nix::unistd::Gid::from_raw(0))?;
    nix::unistd::setuid(nix::unistd::Uid::from_raw(0))?;

    reset_environment(&root);

    let mut stdout = std::io::stdout();
    stdout.write_all(&std::process::id().cast_signed().to_ne_bytes())?;
    stdout.flush()
}

fn receive_stderr(socket: RawFd) -> OwnedFd {
    let mut byte = [0];
    let mut iov = [IoSliceMut::new(&mut byte)];
    let mut space = nix::cmsg_space!(RawFd);
    let message = recvmsg::<()>(socket, &mut iov, Some(&mut space), MsgFlags::empty())
        .expect("reading the elevation preamble");
    let fds = match message.cmsgs().expect("elevation preamble").next() {
        Some(ControlMessageOwned::ScmRights(fds)) => fds,
        other => panic!("elevation preamble carried {:?}, not a stderr pipe", other),
    };
    let [fd] = fds[..] else {
        panic!("elevation preamble carried {} descriptors", fds.len());
    };
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Replace the environment, which is the user's, with root's: what pkexec
/// leaves an elevated agent on Linux. The locale and `RUST_LOG` survive.
fn reset_environment(root: &nix::unistd::User) {
    let keep = |name: &std::ffi::OsStr| {
        let name = name.as_bytes();
        name == b"LANG" || name.starts_with(b"LC_") || name == b"RUST_LOG"
    };
    let doomed: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| !keep(name))
        .collect();
    // SAFETY: `adopt` runs before the process starts any threads.
    unsafe {
        for name in doomed {
            std::env::remove_var(name);
        }
        std::env::set_var("HOME", &root.dir);
        std::env::set_var("USER", &root.name);
        std::env::set_var("LOGNAME", &root.name);
        std::env::set_var("SHELL", &root.shell);
        std::env::set_var("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_pipe_crosses_the_socket() {
        let (host, agent) = UnixStream::pair().unwrap();
        let (mut stderr, stderr_tx) = std::io::pipe().unwrap();
        send_stderr(&host, &stderr_tx).unwrap();
        drop(stderr_tx);

        let received = receive_stderr(agent.as_raw_fd());
        std::fs::File::from(received)
            .write_all(b"agent log")
            .unwrap();

        let mut log = String::new();
        stderr.read_to_string(&mut log).unwrap();
        assert_eq!(log, "agent log");
    }

    #[tokio::test]
    async fn reaps_a_child_known_only_by_pid() {
        let sigchld = signal(SignalKind::child()).unwrap();
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 3"])
            .spawn()
            .unwrap();
        let mut process = AgentProcess {
            pid: child.id().cast_signed(),
            sigchld,
        };
        // Reaped by `wait` below, not by `Child`.
        std::mem::forget(child);

        let status = process.wait().await.unwrap();
        assert_eq!(status.code(), Some(3));
    }
}
