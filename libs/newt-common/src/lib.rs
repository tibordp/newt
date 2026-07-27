pub mod agent_resolver;
pub mod api;
mod archive_pack;
pub mod askpass;
pub mod connect;
pub mod discovery;
pub mod enrich;
pub mod error;
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
