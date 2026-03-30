use std::path::{Path, PathBuf};

use crate::vfs::s3::S3VfsDescriptor;
use crate::vfs::{DisplayPathPriority, VfsDescriptor};

// ---------------------------------------------------------------------------
// S3VfsDescriptor — try_parse_display_path
// ---------------------------------------------------------------------------

#[test]
fn s3_unscoped_matches_any_s3_path() {
    let desc = S3VfsDescriptor;
    let meta = b""; // empty = unscoped

    let m = desc
        .try_parse_display_path("s3://my-bucket/some/key", meta)
        .unwrap();
    assert_eq!(m.path, PathBuf::from("/my-bucket/some/key"));
    assert_eq!(m.priority, DisplayPathPriority::Generic);
}

#[test]
fn s3_unscoped_root() {
    let desc = S3VfsDescriptor;
    let m = desc.try_parse_display_path("s3://", b"").unwrap();
    assert_eq!(m.path, PathBuf::from("/"));
    assert_eq!(m.priority, DisplayPathPriority::Generic);
}

#[test]
fn s3_scoped_exact_match() {
    let desc = S3VfsDescriptor;
    let meta = b"my-bucket";

    let m = desc
        .try_parse_display_path("s3://my-bucket/some/key", meta)
        .unwrap();
    assert_eq!(m.path, PathBuf::from("/some/key"));
    assert_eq!(m.priority, DisplayPathPriority::Exact);
}

#[test]
fn s3_scoped_bucket_root_with_slash() {
    let desc = S3VfsDescriptor;
    let m = desc
        .try_parse_display_path("s3://my-bucket/", b"my-bucket")
        .unwrap();
    assert_eq!(m.path, PathBuf::from("/"));
}

#[test]
fn s3_scoped_bucket_root_without_slash() {
    let desc = S3VfsDescriptor;
    let m = desc
        .try_parse_display_path("s3://my-bucket", b"my-bucket")
        .unwrap();
    assert_eq!(m.path, PathBuf::from("/"));
}

#[test]
fn s3_scoped_does_not_match_different_bucket() {
    let desc = S3VfsDescriptor;
    let meta = b"my-bucket";

    // "other-bucket" should not match a mount for "my-bucket"
    let result = desc.try_parse_display_path("s3://other-bucket/key", meta);
    assert!(result.is_none());
}

#[test]
fn s3_non_s3_url_returns_none() {
    let desc = S3VfsDescriptor;
    assert!(desc.try_parse_display_path("/home/user", b"").is_none());
    assert!(
        desc.try_parse_display_path("sftp://host/path", b"")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// S3VfsDescriptor — format_path
// ---------------------------------------------------------------------------

#[test]
fn s3_format_path_scoped_root() {
    let desc = S3VfsDescriptor;
    assert_eq!(
        desc.format_path(Path::new("/"), b"my-bucket"),
        "s3://my-bucket/"
    );
}

#[test]
fn s3_format_path_scoped_key() {
    let desc = S3VfsDescriptor;
    assert_eq!(
        desc.format_path(Path::new("/some/key"), b"my-bucket"),
        "s3://my-bucket/some/key"
    );
}

#[test]
fn s3_format_path_unscoped_root() {
    let desc = S3VfsDescriptor;
    assert_eq!(desc.format_path(Path::new("/"), b""), "s3://");
}

#[test]
fn s3_format_path_unscoped_bucket() {
    let desc = S3VfsDescriptor;
    assert_eq!(
        desc.format_path(Path::new("/bucket/key"), b""),
        "s3://bucket/key"
    );
}

// ---------------------------------------------------------------------------
// S3VfsDescriptor — breadcrumbs
// ---------------------------------------------------------------------------

#[test]
fn s3_breadcrumbs_scoped_root() {
    let desc = S3VfsDescriptor;
    let crumbs = desc.breadcrumbs(Path::new("/"), b"my-bucket");
    assert_eq!(crumbs.len(), 1);
    assert_eq!(crumbs[0].label, "s3://my-bucket/");
    assert_eq!(crumbs[0].nav_path, "/");
}

#[test]
fn s3_breadcrumbs_scoped_nested() {
    let desc = S3VfsDescriptor;
    let crumbs = desc.breadcrumbs(Path::new("/a/b/c"), b"my-bucket");
    assert_eq!(crumbs.len(), 4); // root + a/ + b/ + c
    assert_eq!(crumbs[0].label, "s3://my-bucket/");
    assert_eq!(crumbs[1].label, "a/");
    assert_eq!(crumbs[1].nav_path, "/a");
    assert_eq!(crumbs[2].label, "b/");
    assert_eq!(crumbs[2].nav_path, "/a/b");
    assert_eq!(crumbs[3].label, "c");
    assert_eq!(crumbs[3].nav_path, "/a/b/c");
}

#[test]
fn s3_breadcrumbs_unscoped_root() {
    let desc = S3VfsDescriptor;
    let crumbs = desc.breadcrumbs(Path::new("/"), b"");
    assert_eq!(crumbs.len(), 1);
    assert_eq!(crumbs[0].label, "s3://");
}

// ---------------------------------------------------------------------------
// Archive — breadcrumbs and display path
// ---------------------------------------------------------------------------

use crate::vfs::archive::{is_archive_name, is_zip_name};

#[test]
fn is_archive_name_tar_variants() {
    assert!(is_archive_name("file.tar"));
    assert!(is_archive_name("file.tar.gz"));
    assert!(is_archive_name("file.tgz"));
    assert!(is_archive_name("file.tar.bz2"));
    assert!(is_archive_name("file.tar.xz"));
    assert!(is_archive_name("file.tar.zst"));
}

#[test]
fn is_archive_name_zip_variants() {
    assert!(is_zip_name("file.zip"));
    assert!(is_zip_name("app.jar"));
    assert!(is_zip_name("deploy.war"));
    assert!(is_zip_name("app.apk"));
}

#[test]
fn is_archive_name_case_insensitive() {
    assert!(is_archive_name("FILE.TAR.GZ"));
    assert!(is_zip_name("FILE.ZIP"));
}

#[test]
fn is_archive_name_non_archive() {
    assert!(!is_archive_name("file.txt"));
    assert!(!is_archive_name("file.rs"));
    assert!(!is_archive_name("tarfile"));
}

// ---------------------------------------------------------------------------
// VfsPath
// ---------------------------------------------------------------------------

use crate::vfs::{VfsId, VfsPath};

#[test]
fn vfs_path_display_root() {
    let p = VfsPath::root("/home/user");
    assert_eq!(format!("{}", p), "/home/user");
}

#[test]
fn vfs_path_display_non_root() {
    let p = VfsPath::new(VfsId(5), "/some/path");
    assert_eq!(format!("{}", p), "vfs://5:/some/path");
}

#[test]
fn vfs_path_join() {
    let p = VfsPath::root("/home");
    let joined = p.join("user");
    assert_eq!(joined.path, PathBuf::from("/home/user"));
    assert_eq!(joined.vfs_id, VfsId::ROOT);
}

#[test]
fn vfs_path_parent() {
    let p = VfsPath::root("/home/user");
    let parent = p.parent().unwrap();
    assert_eq!(parent.path, PathBuf::from("/home"));
}

#[test]
#[should_panic(expected = "VfsPath must be absolute")]
fn vfs_path_rejects_relative() {
    VfsPath::root("relative/path");
}

// ---------------------------------------------------------------------------
// VfsRegistry
// ---------------------------------------------------------------------------

// VfsRegistry tests require a mock Vfs. Since we can't easily construct one
// without the full test_support infrastructure, we test the simpler logic:
// mount/unmount/get.

use crate::vfs::VfsRegistry;
use std::sync::Arc;

// Minimal mock Vfs for registry tests
struct DummyVfs;

#[async_trait::async_trait]
impl crate::vfs::Vfs for DummyVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &S3VfsDescriptor // reuse; descriptor type doesn't matter for registry tests
    }
    async fn list_files(
        &self,
        _path: &Path,
        _batch_tx: Option<tokio::sync::mpsc::Sender<Vec<crate::filesystem::File>>>,
    ) -> Result<Vec<crate::filesystem::File>, crate::Error> {
        Ok(vec![])
    }
    async fn poll_changes(&self, _path: &Path) -> Result<(), crate::Error> {
        Ok(())
    }
    async fn fs_stats(
        &self,
        _path: &Path,
    ) -> Result<Option<crate::filesystem::FsStats>, crate::Error> {
        Ok(None)
    }
}

#[test]
fn registry_mount_returns_incrementing_ids() {
    let registry = VfsRegistry::with_root(Arc::new(DummyVfs));

    let id1 = registry.mount(Arc::new(DummyVfs));
    let id2 = registry.mount(Arc::new(DummyVfs));
    let id3 = registry.mount(Arc::new(DummyVfs));

    assert_eq!(id1, VfsId(1));
    assert_eq!(id2, VfsId(2));
    assert_eq!(id3, VfsId(3));
}

#[test]
fn registry_get_returns_mounted_vfs() {
    let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
    assert!(registry.get(VfsId::ROOT).is_some());
    assert!(registry.get(VfsId(99)).is_none());

    let id = registry.mount(Arc::new(DummyVfs));
    assert!(registry.get(id).is_some());
}

#[test]
fn registry_unmount_removes_vfs() {
    let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
    let id = registry.mount(Arc::new(DummyVfs));
    assert!(registry.get(id).is_some());

    registry.unmount(id);
    assert!(registry.get(id).is_none());
}

#[test]
fn registry_cannot_unmount_root() {
    let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
    let result = registry.unmount(VfsId::ROOT);
    assert!(result.is_none()); // refused
    assert!(registry.get(VfsId::ROOT).is_some()); // still there
}

#[test]
fn registry_resolve_returns_error_for_missing_vfs() {
    let registry = VfsRegistry::with_root(Arc::new(DummyVfs));
    let result = registry.resolve(&VfsPath::new(VfsId(999), "/"));
    assert!(result.is_err());
}
