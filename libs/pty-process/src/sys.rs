use std::os::fd::{AsRawFd as _, FromRawFd as _};

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
        Ok(Pts(std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.get_slave_name()?)?
            .into()))
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
        let buf: [c_char; 128] = [0; 128];

        unsafe {
            match libc::ioctl(self.0.as_raw_fd(), u64::from(libc::TIOCPTYGNAME), &buf) {
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

pub struct Pts(std::os::fd::OwnedFd);

impl Pts {
    pub fn setup_subprocess(
        &self,
    ) -> std::io::Result<(
        std::process::Stdio,
        std::process::Stdio,
        std::process::Stdio,
    )> {
        Ok((
            self.0.try_clone()?.into(),
            self.0.try_clone()?.into(),
            self.0.try_clone()?.into(),
        ))
    }

    pub fn session_leader(&self) -> impl FnMut() -> std::io::Result<()> {
        move || {
            nix::unistd::setsid()?;
            // Use fd 0 (stdin) for TIOCSCTTY — by this point stdin has been
            // dup2'd to the PTS slave. We can't use the original PTS fd because
            // std::process closes all O_CLOEXEC fds before running pre_exec.
            //
            // Call libc::ioctl directly instead of using the nix ioctl_write_ptr_bad!
            // macro, because TIOCSCTTY expects an integer argument (0 = don't steal),
            // not a pointer. The nix macro passes a *const c_int which may not be
            // equivalent to integer 0 with musl's variadic argument handling.
            let ret = unsafe { libc::ioctl(0, libc::TIOCSCTTY as _, 0 as libc::c_int) };
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(())
        }
    }
}

impl From<Pts> for std::os::fd::OwnedFd {
    fn from(pts: Pts) -> Self {
        pts.0
    }
}

impl std::os::fd::AsFd for Pts {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl std::os::fd::AsRawFd for Pts {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

nix::ioctl_write_ptr_bad!(set_term_size_unsafe, libc::TIOCSWINSZ, nix::pty::Winsize);

nix::ioctl_write_ptr_bad!(
    set_controlling_terminal_unsafe,
    libc::TIOCSCTTY,
    libc::c_int
);
