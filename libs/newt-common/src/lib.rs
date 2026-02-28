pub mod api;
pub mod file_reader;
pub mod filesystem;
pub mod hot_paths;
pub mod operation;
pub mod rpc;
pub mod sys;
pub mod terminal;
pub mod vfs;

use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ErrorKind {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    NotSupported,
    Cancelled,
    Connection,
    Other,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn custom(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Other,
            message: msg.into(),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            kind: ErrorKind::Cancelled,
            message: "operation cancelled".into(),
        }
    }

    pub fn not_supported() -> Self {
        Self {
            kind: ErrorKind::NotSupported,
            message: "operation not supported".into(),
        }
    }

    pub fn connection() -> Self {
        Self {
            kind: ErrorKind::Connection,
            message: "connection error".into(),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        let kind = match e.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            std::io::ErrorKind::AlreadyExists => ErrorKind::AlreadyExists,
            std::io::ErrorKind::NotADirectory => ErrorKind::NotADirectory,
            std::io::ErrorKind::IsADirectory => ErrorKind::IsADirectory,
            std::io::ErrorKind::DirectoryNotEmpty => ErrorKind::DirectoryNotEmpty,
            std::io::ErrorKind::Unsupported => ErrorKind::NotSupported,
            _ => ErrorKind::Other,
        };
        Self {
            kind,
            message: e.to_string(),
        }
    }
}

impl From<nix::Error> for Error {
    fn from(e: nix::Error) -> Self {
        let kind = match e {
            nix::Error::ENOENT => ErrorKind::NotFound,
            nix::Error::EACCES | nix::Error::EPERM => ErrorKind::PermissionDenied,
            nix::Error::EEXIST => ErrorKind::AlreadyExists,
            nix::Error::ENOTDIR => ErrorKind::NotADirectory,
            nix::Error::EISDIR => ErrorKind::IsADirectory,
            nix::Error::ENOTEMPTY => ErrorKind::DirectoryNotEmpty,
            nix::Error::ENOTSUP => ErrorKind::NotSupported,
            _ => ErrorKind::Other,
        };
        Self {
            kind,
            message: e.to_string(),
        }
    }
}

impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Self {
        Self {
            kind: ErrorKind::Other,
            message: e.to_string(),
        }
    }
}

impl From<notify::Error> for Error {
    fn from(e: notify::Error) -> Self {
        Self {
            kind: ErrorKind::Other,
            message: e.to_string(),
        }
    }
}

impl From<pty_process::Error> for Error {
    fn from(e: pty_process::Error) -> Self {
        Self {
            kind: ErrorKind::Other,
            message: e.to_string(),
        }
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
