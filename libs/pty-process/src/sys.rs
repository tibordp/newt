use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::path::PathBuf;

#[derive(Debug)]
pub struct Pty(pub nix::pty::PtyMaster);

#[cfg(any(target_os = "android", target_os = "linux", target_os = "freebsd"))]
#[cfg_attr(docsrs, doc(cfg(all())))]
#[inline]
pub fn ptsname_r(fd: &nix::pty::PtyMaster) -> nix::Result<String> {
    let mut name_buf = Vec::<libc::c_char>::with_capacity(64);
    let name_buf_ptr = name_buf.as_mut_ptr();
    let cname = unsafe {
        let cap = name_buf.capacity();
        if libc::ptsname_r(fd.as_raw_fd(), name_buf_ptr, cap) != 0 {
            return Err(nix::Error::last());
        }
        std::ffi::CStr::from_ptr(name_buf.as_ptr())
    };

    let name = cname.to_string_lossy().into_owned();
    Ok(name)
}

impl Pty {
    pub fn open() -> crate::Result<Self> {
        #[cfg(not(target_os = "linux"))]
        let pt = nix::pty::posix_openpt(
            nix::fcntl::OFlag::O_RDWR
                | nix::fcntl::OFlag::O_NOCTTY
                | nix::fcntl::OFlag::O_CLOEXEC
                | nix::fcntl::OFlag::O_NONBLOCK,
        )?;
        #[cfg(target_os = "linux")]
        let pt = nix::pty::posix_openpt(
            nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_NOCTTY | nix::fcntl::OFlag::O_CLOEXEC,
        )?;
        nix::pty::grantpt(&pt)?;
        nix::pty::unlockpt(&pt)?;

        Ok(Self(pt))
    }

    pub fn set_term_size(&self, size: crate::Size) -> crate::Result<()> {
        let size = size.into();
        let fd = self.0.as_raw_fd();

        // Safety: nix::pty::PtyMaster is required to contain a valid file
        // descriptor and size is guaranteed to be initialized because it's a
        // normal rust value, and nix::pty::Winsize is a repr(C) struct with
        // the same layout as `struct winsize` from sys/ioctl.h.
        Ok(
            unsafe { set_term_size_unsafe(fd, std::ptr::NonNull::from(&size).as_ptr()) }
                .map(|_| ())?,
        )
    }

    pub fn pts(&self) -> crate::Result<Pts> {
        Ok(Pts(self.get_slave_name()?))
    }

    #[cfg(target_os = "macos")]
    fn get_slave_name(&self) -> std::io::Result<std::path::PathBuf> {
        use std::ffi::{CStr, OsStr};
        use std::os::raw::c_char;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;
        // ptsname_r is a linux extension but ptsname isn't thread-safe
        // we could use a static mutex but instead we re-implemented ptsname_r with a syscall
        // ioctl(fd, TIOCPTYGNAME, buf) manually
        // the buffer size on OSX is 128, defined by sys/ttycom.h
        //
        //
        let mut buf: [c_char; 128] = [0; 128];

        unsafe {
            match libc::ioctl(
                self.0.as_raw_fd(),
                u64::from(libc::TIOCPTYGNAME),
                buf.as_mut_ptr(),
            ) {
                0 => Ok(PathBuf::from(OsStr::from_bytes(
                    CStr::from_ptr(buf.as_ptr()).to_bytes(),
                ))),
                _ => Err(std::io::Error::last_os_error()),
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn get_slave_name(&self) -> std::io::Result<std::path::PathBuf> {
        Ok(ptsname_r(&self.0)?.into())
    }

    #[cfg(target_os = "linux")]
    pub fn set_nonblocking(&self) -> nix::Result<()> {
        let bits = nix::fcntl::fcntl(self.0.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)?;
        // Safety: bits was just returned from a F_GETFL call. ideally i would
        // just be able to use from_bits here, but it fails for some reason?
        let mut opts = unsafe { nix::fcntl::OFlag::from_bits_unchecked(bits) };
        opts |= nix::fcntl::OFlag::O_NONBLOCK;
        nix::fcntl::fcntl(self.0.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(opts))?;

        Ok(())
    }
}

impl From<Pty> for std::os::fd::OwnedFd {
    fn from(pty: Pty) -> Self {
        let Pty(nix_ptymaster) = pty;
        let raw_fd = nix_ptymaster.as_raw_fd();
        std::mem::forget(nix_ptymaster);

        // Safety: nix::pty::PtyMaster is required to contain a valid file
        // descriptor, and we ensured that the file descriptor will remain
        // valid by skipping the drop implementation for nix::pty::PtyMaster
        unsafe { Self::from_raw_fd(raw_fd) }
    }
}

impl std::os::fd::AsFd for Pty {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        let raw_fd = self.0.as_raw_fd();

        // Safety: nix::pty::PtyMaster is required to contain a valid file
        // descriptor, and it is owned by self
        unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) }
    }
}

impl std::os::fd::AsRawFd for Pty {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

pub struct Pts(PathBuf);

impl Pts {
    /// Returns a closure suitable for `pre_exec` that sets up the child as a
    /// session leader with the slave PTY as its controlling terminal.
    ///
    /// `dup_fds` controls which of stdin(0)/stdout(1)/stderr(2) get dup2'd to
    /// the slave fd. Pass `false` for any fd that was overridden by the caller
    /// (e.g. via `Command::stdin()`).
    ///
    /// The closure only uses async-signal-safe syscalls (`setsid`, `open`,
    /// `ioctl`, `dup2`, `close`). The `CString` allocation for the path is
    /// done here, before the closure is returned, so nothing allocates between
    /// fork and exec.
    pub fn session_leader(&self, dup_fds: [bool; 3]) -> impl FnMut() -> std::io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(self.0.as_os_str().as_bytes().to_vec())
            .expect("slave path contains null");

        move || {
            nix::unistd::setsid()?;

            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }

            // Required on BSDs, redundant but harmless on Linux
            let ret = unsafe { libc::ioctl(fd, libc::TIOCSCTTY as _, 0 as libc::c_int) };
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }

            for (target, should_dup) in dup_fds.iter().enumerate() {
                if *should_dup && unsafe { libc::dup2(fd, target as libc::c_int) } < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }

            if fd > 2 {
                unsafe { libc::close(fd) };
            }
            Ok(())
        }
    }
}

nix::ioctl_write_ptr_bad!(set_term_size_unsafe, libc::TIOCSWINSZ, nix::pty::Winsize);
