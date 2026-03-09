use std::io::{Read as _, Write as _};
use std::sync::Arc;

type AsyncPty = tokio::io::unix::AsyncFd<crate::sys::Pty>;

/// An allocated pty
pub struct Pty(AsyncPty);

impl Pty {
    /// Allocate and return a new pty.
    pub fn new() -> crate::Result<Self> {
        let pty = crate::sys::Pty::open()?;
        #[cfg(target_os = "linux")]
        pty.set_nonblocking()?;
        Ok(Self(tokio::io::unix::AsyncFd::new(pty)?))
    }

    /// Opens a file descriptor for the other end of the pty, which should be
    /// attached to the child process running in it.
    pub fn pts(&self) -> crate::Result<Pts> {
        Ok(Pts(self.0.get_ref().pts()?))
    }

    /// Splits the pty into owned read and write halves that can be moved to
    /// independent tasks.
    #[must_use]
    pub fn into_split(self) -> (OwnedReadPty, OwnedWritePty) {
        let Self(pt) = self;
        let read_pt = Arc::new(pt);
        let write_pt = Arc::clone(&read_pt);
        (OwnedReadPty(read_pt), OwnedWritePty(write_pt))
    }
}

/// The child end of the pty
pub struct Pts(pub(crate) crate::sys::Pts);

/// Owned read half of a [`Pty`]
#[derive(Debug)]
pub struct OwnedReadPty(Arc<AsyncPty>);

impl tokio::io::AsyncRead for OwnedReadPty {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf,
    ) -> std::task::Poll<std::io::Result<()>> {
        loop {
            let mut guard = match self.0.poll_read_ready(cx) {
                std::task::Poll::Ready(guard) => guard,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }?;
            let b = buf.initialize_unfilled();
            match guard.try_io(|inner| (&inner.get_ref().0).read(b)) {
                Ok(Ok(bytes)) => {
                    buf.advance(bytes);
                    return std::task::Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return std::task::Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

/// Owned write half of a [`Pty`]
#[derive(Debug)]
pub struct OwnedWritePty(Arc<AsyncPty>);

impl OwnedWritePty {
    /// Change the terminal size associated with the pty.
    pub fn resize(&self, size: crate::Size) -> crate::Result<()> {
        self.0.get_ref().set_term_size(size)
    }
}

impl tokio::io::AsyncWrite for OwnedWritePty {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        loop {
            let mut guard = match self.0.poll_write_ready(cx) {
                std::task::Poll::Ready(guard) => guard,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }?;
            match guard.try_io(|inner| (&inner.get_ref().0).write(buf)) {
                Ok(result) => return std::task::Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        loop {
            let mut guard = match self.0.poll_write_ready(cx) {
                std::task::Poll::Ready(guard) => guard,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }?;
            match guard.try_io(|inner| (&inner.get_ref().0).flush()) {
                Ok(_) => return std::task::Poll::Ready(Ok(())),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}
