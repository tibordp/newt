#![feature(io_error_more)]

pub mod communicator;
pub mod filesystem;

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
    Custom(String),
    #[error("operation cancelled")]
    Cancelled,
}

// we must manually implement serde::Serialize
impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
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
