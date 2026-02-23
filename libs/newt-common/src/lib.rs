pub mod api;
pub mod file_reader;
pub mod filesystem;
pub mod operation;
pub mod rpc;
pub mod sys;
pub mod terminal;
pub mod vfs;
pub mod vfs_archive;

use std::time::SystemTime;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Tokio(#[from] tokio::task::JoinError),
    #[error("{0}")]
    Notify(#[from] notify::Error),
    #[error("{0}")]
    PtyProcess(#[from] pty_process::Error),
    #[error("{0}")]
    Nix(#[from] nix::Error),
    #[error("{0}")]
    Custom(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("connection error")]
    Connection,
    #[error("{0}")]
    Remote(String),
    #[error("operation not supported")]
    NotSupported,
}

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

impl<'de> serde::Deserialize<'de> for Error {
    fn deserialize<D>(deserializer: D) -> Result<Error, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Error::Remote(s))
    }
}

pub trait ToUnix {
    fn to_unix(&self) -> i128;
}

impl ToUnix for SystemTime {
    fn to_unix(&self) -> i128 {
        self.duration_since(SystemTime::UNIX_EPOCH)
            .map(|t| t.as_millis() as i128)
            .unwrap_or_else(|e| -(e.duration().as_millis() as i128))
    }
}
