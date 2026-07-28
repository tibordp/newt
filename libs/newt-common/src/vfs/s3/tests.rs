use crate::vfs::path::PathBuf;
use crate::vfs::{DisplayPathPriority, VfsDescriptor};

use super::S3VfsDescriptor;

// ---------------------------------------------------------------------------
// S3VfsDescriptor — try_parse_display_path
// ---------------------------------------------------------------------------

fn pb(s: &str) -> PathBuf {
    PathBuf::from_wire_str(s)
}

#[test]
fn s3_unscoped_matches_any_s3_path() {
    let desc = S3VfsDescriptor;
    let meta = b""; // empty = unscoped

    let m = desc
        .try_parse_display_path("s3://my-bucket/some/key", meta)
        .unwrap();
    assert_eq!(m.path, pb("/my-bucket/some/key"));
    assert_eq!(m.priority, DisplayPathPriority::Generic);
}

#[test]
fn s3_unscoped_root() {
    let desc = S3VfsDescriptor;
    let m = desc.try_parse_display_path("s3://", b"").unwrap();
    assert!(m.path.is_root());
    assert_eq!(m.priority, DisplayPathPriority::Generic);
}

#[test]
fn s3_scoped_exact_match() {
    let desc = S3VfsDescriptor;
    let meta = b"my-bucket";

    let m = desc
        .try_parse_display_path("s3://my-bucket/some/key", meta)
        .unwrap();
    assert_eq!(m.path, pb("/some/key"));
    assert_eq!(m.priority, DisplayPathPriority::Exact);
}

#[test]
fn s3_scoped_bucket_root_with_slash() {
    let desc = S3VfsDescriptor;
    let m = desc
        .try_parse_display_path("s3://my-bucket/", b"my-bucket")
        .unwrap();
    assert!(m.path.is_root());
}

#[test]
fn s3_scoped_bucket_root_without_slash() {
    let desc = S3VfsDescriptor;
    let m = desc
        .try_parse_display_path("s3://my-bucket", b"my-bucket")
        .unwrap();
    assert!(m.path.is_root());
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
        desc.format_path(&PathBuf::root(), b"my-bucket"),
        "s3://my-bucket/"
    );
}

#[test]
fn s3_format_path_scoped_key() {
    let desc = S3VfsDescriptor;
    assert_eq!(
        desc.format_path(&pb("/some/key"), b"my-bucket"),
        "s3://my-bucket/some/key"
    );
}

#[test]
fn s3_format_path_unscoped_root() {
    let desc = S3VfsDescriptor;
    assert_eq!(desc.format_path(&PathBuf::root(), b""), "s3://");
}

#[test]
fn s3_format_path_unscoped_bucket() {
    let desc = S3VfsDescriptor;
    assert_eq!(desc.format_path(&pb("/bucket/key"), b""), "s3://bucket/key");
}

// ---------------------------------------------------------------------------
// S3VfsDescriptor — breadcrumbs
// ---------------------------------------------------------------------------

#[test]
fn s3_breadcrumbs_scoped_root() {
    let desc = S3VfsDescriptor;
    let crumbs = desc.breadcrumbs(&PathBuf::root(), b"my-bucket");
    assert_eq!(crumbs.len(), 1);
    assert_eq!(crumbs[0].label, "s3://my-bucket/");
    assert_eq!(crumbs[0].nav_path, "/");
}

#[test]
fn s3_breadcrumbs_scoped_nested() {
    let desc = S3VfsDescriptor;
    let crumbs = desc.breadcrumbs(&pb("/a/b/c"), b"my-bucket");
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
    let crumbs = desc.breadcrumbs(&PathBuf::root(), b"");
    assert_eq!(crumbs.len(), 1);
    assert_eq!(crumbs[0].label, "s3://");
}

// ---------------------------------------------------------------------------
// Archive — breadcrumbs and display path
// ---------------------------------------------------------------------------
