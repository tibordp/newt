//! Unix pseudo-terminal backend: a child spawned as the session leader of a
//! new pty, and the master split into async read and write halves. The
//! Windows counterpart is `conpty.rs`.
//!
//! Derived from pty-process by Jesse Luehrs, MIT licensed (see
//! THIRD-PARTY-NOTICES.md).

use std::ffi::CString;
use std::io::{self, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use newt_common::Error;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{PtyMaster, Winsize, grantpt, posix_openpt, unlockpt};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{Child, Command};

type Master = Arc<AsyncFd<PtyMaster>>;

/// Spawns `cmd` on a new pty: stdin, stdout and stderr are the pty, and it
/// becomes a session leader with the pty as its controlling terminal.
pub fn spawn(cmd: &mut Command) -> Result<(Child, PtyReader, PtyWriter), Error> {
    let master = posix_openpt(OFlag::O_RDWR | OFlag::O_NOCTTY | OFlag::O_CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let flags = OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL)?);
    fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;

    let slave = slave_path(&master)?;
    let master = Arc::new(AsyncFd::new(master)?);

    // Safety: between fork and exec the hook makes only async-signal-safe
    // calls (setsid, open, ioctl, dup2, close) and allocates nothing — the
    // slave path was built above.
    unsafe {
        cmd.pre_exec(move || {
            nix::unistd::setsid()?;
            let fd = libc::open(slave.as_ptr(), libc::O_RDWR);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(fd, libc::TIOCSCTTY as _, 0 as libc::c_int) != 0 {
                return Err(io::Error::last_os_error());
            }
            for target in 0..3 {
                if libc::dup2(fd, target) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if fd > 2 {
                libc::close(fd);
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;

    Ok((child, PtyReader(master.clone()), PtyWriter(master)))
}

#[cfg(target_os = "macos")]
fn slave_path(master: &PtyMaster) -> Result<CString, Error> {
    // TIOCPTYGNAME rather than ptsname(3), which returns a static buffer.
    let mut buf: [libc::c_char; 128] = [0; 128];
    // Safety: TIOCPTYGNAME writes a NUL-terminated name of at most 128 bytes.
    let ret = unsafe {
        libc::ioctl(
            master.as_raw_fd(),
            libc::c_ulong::from(libc::TIOCPTYGNAME),
            buf.as_mut_ptr(),
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // Safety: the ioctl succeeded, so `buf` holds a NUL-terminated string.
    Ok(unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

#[cfg(target_os = "linux")]
fn slave_path(master: &PtyMaster) -> Result<CString, Error> {
    let name = nix::pty::ptsname_r(master)?;
    Ok(CString::new(name).expect("ptsname_r returned an interior NUL"))
}

pub struct PtyReader(Master);

impl AsyncRead for PtyReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(cx))?;
            match guard.try_io(|fd| fd.get_ref().read(buf.initialize_unfilled())) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

pub struct PtyWriter(Master);

impl PtyWriter {
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), Error> {
        let size = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // Safety: the fd is a live pty master and `size` is a valid winsize.
        unsafe { set_winsize(self.0.as_raw_fd(), &size) }?;
        Ok(())
    }
}

impl AsyncWrite for PtyWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(cx))?;
            match guard.try_io(|fd| fd.get_ref().write(buf)) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(cx))?;
            match guard.try_io(|fd| fd.get_ref().flush()) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

nix::ioctl_write_ptr_bad!(set_winsize, libc::TIOCSWINSZ, Winsize);

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn child_gets_the_pty_as_its_controlling_terminal() {
        // `/dev/tty` opens only for a process with a controlling terminal;
        // the `read` holds the child until the resize has landed.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "read _; stty size </dev/tty"]);
        let (mut child, mut reader, mut writer) = spawn(&mut cmd).unwrap();

        writer.resize(40, 100).unwrap();
        writer.write_all(b"\n").await.unwrap();

        let mut output = Vec::new();
        let mut buf = [0; 1024];
        // At hang-up Linux reads EIO from the master and macOS reads EOF.
        while let Ok(n @ 1..) = reader.read(&mut buf).await {
            output.extend_from_slice(&buf[..n]);
        }
        assert!(child.wait().await.unwrap().success());
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("40 100"), "output: {output:?}");
    }
}
