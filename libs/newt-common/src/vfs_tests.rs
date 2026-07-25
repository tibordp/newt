use crate::vfs::path::PathBuf;
use crate::vfs::s3::S3VfsDescriptor;
use crate::vfs::{DisplayPathPriority, VfsDescriptor};

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
    let p = VfsPath::from_wire_str(VfsId::ROOT, "/home/user");
    assert_eq!(format!("{}", p), "/home/user");
}

#[test]
fn vfs_path_display_non_root() {
    let p = VfsPath::from_wire_str(VfsId(5), "/some/path");
    assert_eq!(format!("{}", p), "vfs://5:/some/path");
}

#[test]
fn vfs_path_join() {
    let p = VfsPath::from_wire_str(VfsId::ROOT, "/home");
    let joined = p.join("user");
    assert_eq!(joined.path, PathBuf::from_wire_str("/home/user"));
    assert_eq!(joined.vfs_id, VfsId::ROOT);
}

#[test]
fn vfs_path_parent() {
    let p = VfsPath::from_wire_str(VfsId::ROOT, "/home/user");
    let parent = p.parent().unwrap();
    assert_eq!(parent.path, PathBuf::from_wire_str("/home"));
}

// ---------------------------------------------------------------------------
// VfsRegistry
// ---------------------------------------------------------------------------

// VfsRegistry tests require a mock Vfs. Since we can't easily construct one
// without the full test_support infrastructure, we test the simpler logic:
// mount/unmount/get.

use crate::vfs::VfsRegistry;
use crate::vfs::path::Path;
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
    ) -> Result<crate::vfs::VfsFileList, crate::Error> {
        Ok(crate::vfs::VfsFileList::default())
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
    let result = registry.resolve(&VfsPath::root(VfsId(999)));
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// LocalVfs::same_file — real filesystem
// ---------------------------------------------------------------------------

mod same_file {
    use std::sync::Arc;

    use crate::vfs::Vfs;
    use crate::vfs::local::{LocalVfs, local_path_from_native};
    use crate::vfs::path::PathBuf;

    struct Fixture {
        _dir: tempfile::TempDir,
        vfs: Arc<LocalVfs>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                _dir: tempfile::tempdir().expect("tempdir"),
                vfs: Arc::new(LocalVfs::new()),
            }
        }

        /// VFS path of `name` inside the fixture directory.
        fn path(&self, name: &str) -> PathBuf {
            local_path_from_native(&self._dir.path().join(name))
        }

        fn write(&self, name: &str) -> PathBuf {
            std::fs::write(self._dir.path().join(name), b"x").expect("write");
            self.path(name)
        }

        /// Whether the volume under test folds case — a stock macOS or
        /// Windows volume does, ext4 doesn't, and APFS can be formatted
        /// either way. Decided by asking the filesystem, so the tests
        /// below assert what's true *here* rather than what's true on the
        /// author's laptop.
        fn volume_folds_case(&self) -> bool {
            std::fs::write(self._dir.path().join("CaseProbe"), b"x").expect("write");
            let folds = self._dir.path().join("caseprobe").exists();
            std::fs::remove_file(self._dir.path().join("CaseProbe")).expect("remove");
            folds
        }
    }

    #[tokio::test]
    async fn a_file_is_itself() {
        let fx = Fixture::new();
        let a = fx.write("a.txt");
        assert!(fx.vfs.same_file(&a, &a).await.unwrap());
    }

    #[tokio::test]
    async fn distinct_files_are_not_the_same() {
        let fx = Fixture::new();
        let (a, b) = (fx.write("a.txt"), fx.write("b.txt"));
        assert!(!fx.vfs.same_file(&a, &b).await.unwrap());
    }

    #[tokio::test]
    async fn an_absent_path_is_nothing_s_twin() {
        let fx = Fixture::new();
        let a = fx.write("a.txt");
        let missing = fx.path("missing.txt");
        let also_missing = fx.path("also-missing.txt");

        assert!(!fx.vfs.same_file(&a, &missing).await.unwrap());
        assert!(!fx.vfs.same_file(&missing, &a).await.unwrap());
        // Two absent paths must not compare equal just because both
        // resolve to "no identity".
        assert!(!fx.vfs.same_file(&missing, &also_missing).await.unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hardlinks_are_the_same_file() {
        let fx = Fixture::new();
        let a = fx.write("a.txt");
        std::fs::hard_link(
            fx._dir.path().join("a.txt"),
            fx._dir.path().join("link.txt"),
        )
        .expect("hard_link");
        let link = fx.path("link.txt");

        // Same inode, different name — what `cp` refuses to copy onto itself.
        assert!(fx.vfs.same_file(&a, &link).await.unwrap());
    }

    #[tokio::test]
    async fn case_variants_follow_the_volume() {
        let fx = Fixture::new();
        let folds = fx.volume_folds_case();
        let upper = fx.write("Foo.txt");
        let lower = fx.path("foo.txt");

        assert_eq!(fx.vfs.same_file(&upper, &lower).await.unwrap(), folds);
    }

    /// The whole point of the exercise: on a case-insensitive volume the
    /// rename must actually go through rather than trip over its own
    /// destination.
    #[tokio::test]
    async fn case_only_rename_succeeds_on_a_folding_volume() {
        let fx = Fixture::new();
        if !fx.volume_folds_case() {
            return;
        }
        let upper = fx.write("Foo.txt");
        let lower = fx.path("foo.txt");

        fx.vfs.rename(&upper, &lower).await.expect("rename");

        let names: Vec<String> = std::fs::read_dir(fx._dir.path())
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["foo.txt".to_string()]);
    }
}

// ---------------------------------------------------------------------------
// SearchVfs — progress reporting
// ---------------------------------------------------------------------------

/// The walker is the only thing that knows a long search is alive: until
/// the first hit there is nothing to put in the pane. These cover the
/// signal it emits meanwhile.
mod search_progress {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use crate::test_support::MockVfs;
    use crate::vfs::path::PathBuf;
    use crate::vfs::search::{SearchParams, SearchVfs};
    use crate::vfs::{ProgressReporter, Vfs, VfsId, VfsPath, VfsProgress, VfsRegistry};

    #[derive(Default)]
    struct Capture(Mutex<Vec<Option<VfsProgress>>>);

    impl ProgressReporter for Capture {
        fn report(&self, progress: Option<VfsProgress>) {
            self.0.lock().push(progress);
        }
    }

    /// Run a search that matches nothing over a small tree, and return
    /// every progress report it emitted.
    async fn reports_for_a_fruitless_search() -> Vec<Option<VfsProgress>> {
        let mut b = MockVfs::builder();
        for d in 0..3 {
            b = b.dir(&format!("/dir{d}"));
            for f in 0..3 {
                b = b.file(&format!("/dir{d}/f{f}.txt"), b"content");
            }
        }
        let source = b.build();
        let registry = Arc::new(VfsRegistry::with_root(source.clone()));
        let reader = Arc::new(crate::vfs::VfsRegistryFileReader::new(registry));
        let capture = Arc::new(Capture::default());

        let vfs = SearchVfs::new(
            source,
            reader,
            VfsPath::new(VfsId::ROOT, PathBuf::root()),
            SearchParams {
                name_pattern: Some("*matches-nothing*".into()),
                ..Default::default()
            },
            Vec::new(),
            capture.clone(),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let _ = vfs.list_files(&PathBuf::root(), Some(tx)).await;
        let _ = drain.await;

        capture.0.lock().clone()
    }

    #[tokio::test]
    async fn a_search_that_finds_nothing_still_reports_where_it_is() {
        let reports = reports_for_a_fruitless_search().await;
        let with_path: Vec<&std::collections::BTreeMap<String, String>> = reports
            .iter()
            .flatten()
            .map(|p| &p.extra)
            .filter(|e| e.contains_key("path"))
            .collect();

        assert!(
            !with_path.is_empty(),
            "the walker never said which directory it was in — a long search \
             with no hits looks hung"
        );
        for extra in with_path {
            let path = &extra["path"];
            // Relative to the search root: the root is already named in the
            // pane header, and an absolute path crowds out the status bar.
            assert!(
                !path.starts_with('/'),
                "progress path should be relative to the search root, got {path}"
            );
            assert!(path.starts_with("dir"), "unexpected progress path {path}");
        }
    }

    #[tokio::test]
    async fn progress_carries_a_running_count_and_is_cleared_at_the_end() {
        let reports = reports_for_a_fruitless_search().await;

        assert!(
            reports.iter().flatten().all(|p| p.stage == "Searching"),
            "every report should name its stage"
        );
        // `processed` must always be set: a counter-less report is treated
        // as a mount-log line by the host sink, and a search would spam it.
        assert!(reports.iter().flatten().all(|p| p.processed.is_some()));

        let scanned: Vec<u64> = reports
            .iter()
            .flatten()
            .filter_map(|p| p.processed)
            .collect();
        assert!(
            scanned.windows(2).all(|w| w[1] >= w[0]),
            "the scanned count should never go backwards: {scanned:?}"
        );
        assert!(
            matches!(reports.last(), Some(None)),
            "the walker must clear its progress when it finishes"
        );
    }
}
