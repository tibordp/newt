use std::os::fd::AsRawFd as _;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Pty(pub nix::pty::PtyMaster);

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
        let size: nix::pty::Winsize = size.into();
        let fd = self.0.as_raw_fd();

        // Safety: nix::pty::PtyMaster contains a valid file descriptor and
        // size is initialized. nix::pty::Winsize is repr(C) matching
        // `struct winsize` from sys/ioctl.h.
        Ok(
            unsafe { set_term_size_unsafe(fd, std::ptr::NonNull::from(&size).as_ptr()) }
                .map(|_| ())?,
        )
    }

    pub fn pts(&self) -> crate::Result<Pts> {
        Ok(Pts(self.get_slave_name()?))
    }

    #[cfg(target_os = "macos")]
    fn get_slave_name(&self) -> std::io::Result<PathBuf> {
        use std::ffi::{CStr, OsStr};
        use std::os::raw::c_char;
        use std::os::unix::ffi::OsStrExt;

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
    fn get_slave_name(&self) -> std::io::Result<PathBuf> {
        let mut name_buf = Vec::<libc::c_char>::with_capacity(64);
        let name_buf_ptr = name_buf.as_mut_ptr();
        let cname = unsafe {
            let cap = name_buf.capacity();
            if libc::ptsname_r(self.0.as_raw_fd(), name_buf_ptr, cap) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            std::ffi::CStr::from_ptr(name_buf.as_ptr())
        };
        Ok(cname.to_string_lossy().into_owned().into())
    }

    #[cfg(target_os = "linux")]
    pub fn set_nonblocking(&self) -> nix::Result<()> {
        let bits = nix::fcntl::fcntl(&self.0, nix::fcntl::FcntlArg::F_GETFL)?;
        let mut opts = nix::fcntl::OFlag::from_bits_truncate(bits);
        opts |= nix::fcntl::OFlag::O_NONBLOCK;
        nix::fcntl::fcntl(&self.0, nix::fcntl::FcntlArg::F_SETFL(opts))?;
        Ok(())
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
    /// session leader with the slave PTY as its controlling terminal, with
    /// stdin/stdout/stderr all connected to the pty.
    ///
    /// The closure only uses async-signal-safe syscalls (`setsid`, `open`,
    /// `ioctl`, `dup2`, `close`). The `CString` allocation for the path is
    /// done here, before the closure is returned.
    pub fn session_leader(&self) -> impl FnMut() -> std::io::Result<()> + use<> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(self.0.as_os_str().as_bytes().to_vec())
            .expect("slave path contains null");

        move || {
            nix::unistd::setsid()?;

            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }

            let ret = unsafe { libc::ioctl(fd, libc::TIOCSCTTY as _, 0 as libc::c_int) };
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }

            for target in 0..3 {
                if unsafe { libc::dup2(fd, target) } < 0 {
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
