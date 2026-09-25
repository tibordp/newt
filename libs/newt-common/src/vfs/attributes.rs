//! File attributes a copy can carry beyond the bytes. Values keep their
//! native representation across the RPC boundary; only the side that owns
//! the filesystem interprets them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Everything a copy may preserve. Timestamps, permissions and numeric
/// ownership travel through `get_metadata`/`set_metadata` in one stat;
/// `read_attribute` answers `None` for those and serves the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, specta::Type)]
pub enum AttributeKind {
    /// The stat itself, when reading it fails.
    Metadata,
    Timestamps,
    Permissions,
    Owner {
        by_name: bool,
    },
    Group {
        by_name: bool,
    },
    HardLinks,
    Sparse,
    ExtendedAttributes,
    AccessControl,
    Streams,
    ObjectMetadata,
    ObjectTags,
    ObjectAccess,
}

impl AttributeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Metadata => "source attributes",
            Self::Timestamps => "timestamps",
            Self::Permissions => "permissions",
            Self::Owner { .. } => "owner",
            Self::Group { .. } => "group",
            Self::ExtendedAttributes => "extended attributes",
            Self::AccessControl => "access control list",
            Self::Streams => "alternate streams / resource fork",
            Self::HardLinks => "hard-link relationships",
            Self::Sparse => "sparse allocation",
            Self::ObjectMetadata => "object metadata / storage class",
            Self::ObjectTags => "object tags",
            Self::ObjectAccess => "object access grants",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Xattr {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedStream {
    pub name: String,
    pub path: super::path::PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObjectMetadata {
    pub metadata: BTreeMap<String, String>,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub content_language: Option<String>,
    pub content_disposition: Option<String>,
    pub cache_control: Option<String>,
    pub expires: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Attribute {
    OwnerName(String),
    GroupName(String),
    /// Owner or group in the filesystem's own encoding (a Windows security
    /// descriptor), meaningful only to a destination of the same `format`.
    NativeOwner {
        format: String,
        data: Vec<u8>,
        group: bool,
    },
    /// Opaque per-file identity (device + inode); equal values are hard
    /// links of one another.
    Identity(Vec<u8>),
    Sparse(bool),
    ExtendedAttributes(Vec<Xattr>),
    AccessControl {
        format: String,
        data: Vec<u8>,
    },
    Streams(Vec<NamedStream>),
    ObjectMetadata(ObjectMetadata),
    ObjectTags(BTreeMap<String, String>),
    ObjectAccess(Vec<super::properties::PropertyGrant>),
}

/// Attributes that must be decided when the destination is created rather
/// than patched on afterwards.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WriteOptions {
    pub sparse: bool,
    pub object_metadata: Option<ObjectMetadata>,
    pub object_storage_class: Option<String>,
    pub object_canned_acl: Option<String>,
    /// Fail with `AlreadyExists` if anything is at the path: a file, a
    /// directory, a symlink (dangling included). The refusal may come from
    /// the open, a write or the finish; the destination is untouched
    /// whichever it is.
    pub create_new: bool,
    /// Expected length of what will be written. Advisory: a wrong hint may
    /// cost an optimization, never the contract.
    pub size_hint: Option<u64>,
}

impl WriteOptions {
    /// Asks nothing the VFS must honour; `size_hint` is advisory.
    pub fn is_default(&self) -> bool {
        !self.sparse
            && self.object_metadata.is_none()
            && self.object_storage_class.is_none()
            && self.object_canned_acl.is_none()
            && !self.create_new
    }
}
