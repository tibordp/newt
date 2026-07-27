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
            // Cross-device rename/clone: "not supported for this pair of
            // paths". Strategy cascades (rename → copy+delete, copy_within
            // → streaming) key on NotSupported to decide fallback.
            std::io::ErrorKind::CrossesDevices => ErrorKind::NotSupported,
            _ => ErrorKind::Other,
        };
        Self {
            kind,
            message: e.to_string(),
        }
    }
}

#[cfg(unix)]
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
            nix::Error::EXDEV => ErrorKind::NotSupported,
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
