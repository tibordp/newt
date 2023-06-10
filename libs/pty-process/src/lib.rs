//! This crate is a wrapper around [`tokio::process::Command`] or
//! [`std::process::Command`] which provides the ability to allocate a pty
//! and spawn new processes attached to that pty, with the pty as their
//! controlling terminal. This allows for manipulation of interactive
//! programs.
//!
//! The basic functionality looks like this:
//!
//! ```no_run
//! # #[cfg(feature = "async")]
//! # #[tokio::main]
//! # async fn main() -> pty_process::Result<()> {
//! let mut pty = pty_process::Pty::new()?;
//! pty.resize(pty_process::Size::new(24, 80))?;
//! let mut cmd = pty_process::Command::new("nethack");
//! let child = cmd.spawn(&pty.pts()?)?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "async"))]
//! # fn main() -> pty_process::Result<()> {
//! let mut pty = pty_process::blocking::Pty::new()?;
//! pty.resize(pty_process::Size::new(24, 80))?;
//! let mut cmd = pty_process::blocking::Command::new("nethack");
//! let child = cmd.spawn(&pty.pts()?)?;
//! # Ok(())
//! # }
//! ```
//!
//! The returned `child` is a normal instance of [`tokio::process::Child`] (or
//! [`std::process::Child`] for the [`blocking`](crate::blocking) variant),
//! with its `stdin`/`stdout`/`stderr` file descriptors pointing at the given
//! pty. The `pty` instance implements [`tokio::io::AsyncRead`] and
//! [`tokio::io::AsyncWrite`] (or [`std::io::Read`] and [`std::io::Write`] for
//! the [`blocking`] variant), and can be used to communicate with the child
//! process. The child process will also be made a session leader of a new
//! session, and the controlling terminal of that session will be set to the
//! given pty.
//!
//! # Features
//!
//! By default, only the [`blocking`](crate::blocking) APIs are available. To
//! include the asynchronous APIs, you must enable the `async` feature.

mod error;
pub use error::{Error, Result};
mod types;
pub use types::Size;

mod command;
mod sys;
pub use command::Command;
mod pty;

pub use pty::{OwnedReadPty, OwnedWritePty, Pts, Pty, ReadPty, WritePty};
