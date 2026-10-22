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

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::NotFound,
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

/// Carries the error whole, for an `io::Error` that converts back into it:
/// a read through an `AsyncRead` adapter keeps its kind and message.
impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        let kind = match e.kind {
            ErrorKind::NotFound => std::io::ErrorKind::NotFound,
            ErrorKind::PermissionDenied => std::io::ErrorKind::PermissionDenied,
            ErrorKind::AlreadyExists => std::io::ErrorKind::AlreadyExists,
            ErrorKind::NotADirectory => std::io::ErrorKind::NotADirectory,
            ErrorKind::IsADirectory => std::io::ErrorKind::IsADirectory,
            ErrorKind::DirectoryNotEmpty => std::io::ErrorKind::DirectoryNotEmpty,
            ErrorKind::NotSupported => std::io::ErrorKind::Unsupported,
            ErrorKind::Cancelled | ErrorKind::Connection | ErrorKind::Other => {
                std::io::ErrorKind::Other
            }
        };
        std::io::Error::new(kind, e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if let Some(inner) = e.get_ref().and_then(|inner| inner.downcast_ref::<Error>()) {
            return inner.clone();
        }
        // Stable std maps only EOPNOTSUPP to `Unsupported`; on macOS,
        // ENOTSUP (what xattr calls answer) is a different number. Drop once
        // rust-lang/rust#158580 reaches the stable toolchain.
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::ENOTSUP) {
            return Self {
                kind: ErrorKind::NotSupported,
                message: e.to_string(),
            };
        }
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn both_not_supported_errnos_map_to_not_supported() {
        for errno in [libc::ENOTSUP, libc::EOPNOTSUPP] {
            let error = Error::from(std::io::Error::from_raw_os_error(errno));
            assert_eq!(error.kind, ErrorKind::NotSupported, "errno {errno}");
        }
    }
}
