pub mod agent_resolver;
pub mod api;
mod archive_pack;
pub mod askpass;
pub mod connect;
pub mod discovery;
pub mod enrich;
pub mod error;
pub mod file_reader;
pub mod filesystem;
pub mod hot_paths;
pub mod locale;
pub mod operation;
pub mod proc;
pub mod rpc;
pub mod shell;
pub mod shell_control;
pub mod terminal;
pub mod vfs;

#[cfg(test)]
mod test_support;

pub use error::{Error, ErrorKind};

use std::time::SystemTime;

pub trait ToUnix {
    fn to_unix(&self) -> i64;
}

impl ToUnix for SystemTime {
    fn to_unix(&self) -> i64 {
        let ms = self
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|t| t.as_millis() as i128)
            .unwrap_or_else(|e| -(e.duration().as_millis() as i128));
        ms.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }
}
