//! Data types for the file-content verbs on `filesystem::Filesystem`
//! (details, chunks, in-file search).

use crate::filesystem::{Mode, UserGroup};

/// Guess MIME type from a file path's extension.
/// Returns `None` if the extension is not recognized.
pub fn guess_mime_type(path: &std::path::Path) -> Option<String> {
    mime_guess::from_path(path)
        .first()
        .map(|m| m.essence_str().to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum SearchPattern {
    Literal(Vec<u8>),
    Regex(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SearchMatch {
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FileDetails {
    pub size: u64,
    pub mime_type: Option<String>,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Raw link target as reported by the source FS (see `File::symlink_target`).
    pub symlink_target: Option<String>,
    pub user: Option<UserGroup>,
    pub group: Option<UserGroup>,
    pub mode: Option<Mode>,
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct FileChunk {
    // serde_bytes: bincode's serde path walks Vec<u8> per byte; this hits
    // its bytes fast path with an identical wire format.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    pub offset: u64,
    pub total_size: u64,
}
