use super::*;
use crate::vfs::{Vfs, local::LocalVfs};

fn copy_request(options: CopyOptions) -> OperationRequest {
    OperationRequest::Copy {
        sources: vec![vfs_path("/src")],
        destination: vfs_path("/dst"),
        options,
        rename_to: None,
    }
}

#[tokio::test]
async fn new_directories_preserve_attributes_and_merged_directories_are_opt_in() {
    for preserve_merged_directories in [false, true] {
        let vfs = MockVfs::builder()
            .dir_with_mode("/src", 0o700)
            .dir_with_mode("/src/nested", 0o750)
            .file("/src/nested/a", b"hello")
            .dir_with_mode("/dst/src", 0o755)
            .build();
        let result = run_operation(
            vfs,
            copy_request(CopyOptions {
                preserve_merged_directories,
                ..Default::default()
            }),
            |issue| panic!("unexpected issue: {}", issue.message),
        )
        .await;
        assert!(has_completed(&result.events));
        assert_eq!(
            result.vfs.get_mode("/dst/src"),
            Some(if preserve_merged_directories {
                0o700
            } else {
                0o755
            })
        );
        assert_eq!(result.vfs.get_mode("/dst/src/nested"), Some(0o750));
        assert_eq!(result.vfs.read_content("/dst/src/nested/a"), b"hello");
    }
}

#[tokio::test]
async fn preservation_retry_does_not_recopy_contents() {
    let vfs = MockVfs::builder()
        .file_with_mode("/src/a", b"hello", 0o700)
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dst/src/a"),
            operation: "set_metadata",
            error: crate::Error::custom("try again"),
            remaining: Some(1),
        })
        .build();
    let control = vfs.clone();
    let mut issues = 0;
    let result = run_operation(vfs, copy_request(Default::default()), |issue| {
        issues += 1;
        assert_eq!(
            issue.kind,
            IssueKind::PreservationFailed(AttributeKind::Permissions)
        );
        control.inject_failure(FailureSpec {
            path: PathBuf::from_wire_str("/src/a"),
            operation: "open_read_async",
            error: crate::Error::custom("contents must not be read again"),
            remaining: None,
        });
        IssueResponse {
            action: IssueAction::Retry,
            apply_to_all: true,
        }
    })
    .await;
    assert!(has_completed(&result.events));
    assert_eq!(issues, 1);
    assert_eq!(result.vfs.read_content("/dst/src/a"), b"hello");
    assert_eq!(result.vfs.get_mode("/dst/src/a"), Some(0o700));
}

#[tokio::test]
async fn preservation_all_is_scoped_to_the_attribute() {
    let vfs = MockVfs::builder()
        .file_with_owner("/src/a", b"a", 0o700, 1000, 1000)
        .file_with_owner("/src/b", b"b", 0o700, 1000, 1000)
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dst/src/a"),
            operation: "set_metadata",
            error: crate::Error::custom("unsupported"),
            remaining: None,
        })
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dst/src/b"),
            operation: "set_metadata",
            error: crate::Error::custom("unsupported"),
            remaining: None,
        })
        .build();
    let mut kinds = Vec::new();
    let result = run_operation(
        vfs,
        copy_request(CopyOptions {
            preserve_owner: true,
            ..Default::default()
        }),
        |issue| {
            kinds.push(issue.kind.clone());
            IssueResponse {
                action: IssueAction::Skip,
                apply_to_all: true,
            }
        },
    )
    .await;
    assert!(has_completed(&result.events));
    assert_eq!(
        kinds,
        vec![
            IssueKind::PreservationFailed(AttributeKind::Owner { by_name: false }),
            IssueKind::PreservationFailed(AttributeKind::Permissions)
        ]
    );
    assert_eq!(result.vfs.read_content("/dst/src/b"), b"b");
}

async fn run_vfs(
    vfs: Arc<dyn Vfs>,
    request: OperationRequest,
    mut answer: impl FnMut(&OperationIssue) -> Option<IssueResponse>,
) -> Vec<OperationProgress> {
    let registry = Arc::new(VfsRegistry::with_root(vfs.clone()));
    registry.mount(vfs);
    let context = Arc::new(OperationContext {
        registry,
        shell_integration: None,
    });
    let cancel = CancellationToken::new();
    let resolvers: IssueResolvers = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(execute_operation(
        1,
        request,
        tx,
        cancel.clone(),
        resolvers.clone(),
        Arc::new(AtomicU64::new(1)),
        context,
    ));
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        if let OperationProgress::Issue { issue, .. } = &event {
            match answer(issue) {
                Some(response) => {
                    resolvers
                        .lock()
                        .remove(&issue.issue_id)
                        .unwrap()
                        .send(response)
                        .unwrap();
                }
                None => cancel.cancel(),
            }
        }
        events.push(event);
    }
    task.await.unwrap();
    events
}

#[tokio::test]
async fn move_keeps_source_when_preservation_is_cancelled() {
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .file_with_mode("/src/a", b"hello", 0o700)
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dst/a"),
            operation: "set_metadata",
            error: crate::Error::custom("denied"),
            remaining: None,
        })
        .build();
    let events = run_vfs(
        vfs.clone(),
        OperationRequest::Move {
            sources: vec![vfs_path("/src/a")],
            destination: vfs_path("/dst"),
            options: Default::default(),
            rename_to: None,
        },
        |issue| {
            assert_eq!(
                issue.kind,
                IssueKind::PreservationFailed(AttributeKind::Permissions)
            );
            None
        },
    )
    .await;
    assert!(has_cancelled(&events));
    assert!(vfs.exists("/src/a"));
    assert_eq!(vfs.read_content("/dst/a"), b"hello");
}

#[tokio::test]
async fn destinations_without_metadata_support_are_not_prompted() {
    let src = MockVfs::builder()
        .file_with_mode("/src/a", b"hello", 0o700)
        .build();
    let dst = MockVfs::builder()
        .config(MockVfsConfig {
            can_set_metadata: false,
            ..Default::default()
        })
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dst/src/a"),
            operation: "set_metadata",
            error: crate::Error::not_supported(),
            remaining: None,
        })
        .build();
    let (result, dst) = run_operation_two_vfs(
        src,
        dst,
        OperationRequest::Copy {
            sources: vec![vfs_path("/src")],
            destination: VfsPath::new(VfsId(1), PathBuf::from_wire_str("/dst")),
            options: Default::default(),
            rename_to: None,
        },
        |issue| panic!("unexpected issue: {}", issue.message),
    )
    .await;
    assert!(has_completed(&result.events));
    assert_eq!(dst.read_content("/dst/src/a"), b"hello");
}

#[tokio::test]
async fn permissions_can_be_left_to_destination_defaults() {
    let vfs = MockVfs::builder()
        .file_with_mode("/src/a", b"hello", 0o700)
        .dir("/dst")
        .build();
    let result = run_operation(
        vfs,
        copy_request(CopyOptions {
            preserve_permissions: false,
            ..Default::default()
        }),
        |issue| panic!("unexpected issue: {}", issue.message),
    )
    .await;
    assert_eq!(result.vfs.get_mode("/dst/src/a"), Some(0o644));
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    fn path(path: &std::path::Path) -> VfsPath {
        VfsPath::new(VfsId::ROOT, PathBuf::from_native(path))
    }
    async fn copy(source: &std::path::Path, destination: &std::path::Path, options: CopyOptions) {
        let events = run_vfs(
            Arc::new(LocalVfs::new()),
            OperationRequest::Copy {
                sources: vec![path(source)],
                destination: VfsPath::new(VfsId(1), PathBuf::from_native(destination)),
                options,
                rename_to: None,
            },
            |issue| panic!("unexpected issue: {}", issue.message),
        )
        .await;
        assert!(has_completed(&events));
    }
    #[tokio::test]
    async fn directory_times_and_permissions_are_finalized_after_children() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("child")).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(src.join("child/file"), b"hello").unwrap();
        let stamp = filetime::FileTime::from_unix_time(123456789, 123000000);
        for dir in [&src, &src.join("child")] {
            filetime::set_file_times(dir, stamp, stamp).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        copy(
            &src,
            &dst,
            CopyOptions {
                preserve_timestamps: true,
                ..Default::default()
            },
        )
        .await;
        for dir in [dst.join("src"), dst.join("src/child")] {
            let meta = std::fs::metadata(&dir).unwrap();
            assert_eq!(meta.mode() & 0o777, 0o500);
            assert_eq!(
                filetime::FileTime::from_last_modification_time(&meta),
                stamp
            );
        }
        // Restore write access for TempDir cleanup.
        for dir in [
            &src,
            &src.join("child"),
            &dst.join("src"),
            &dst.join("src/child"),
        ] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    #[tokio::test]
    async fn hard_links_retain_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(src.join("a"), b"hello").unwrap();
        std::fs::hard_link(src.join("a"), src.join("b")).unwrap();
        copy(
            &src,
            &dst,
            CopyOptions {
                preserve_hard_links: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            std::fs::metadata(dst.join("src/a")).unwrap().ino(),
            std::fs::metadata(dst.join("src/b")).unwrap().ino()
        );
    }
    #[tokio::test]
    async fn symlink_targets_are_followed_only_when_requested() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(src.join("a"), b"hello").unwrap();
        symlink("a", src.join("b")).unwrap();
        symlink("b", src.join("c")).unwrap();
        copy(
            &src,
            &dst,
            CopyOptions {
                follow_symlinks: true,
                ..Default::default()
            },
        )
        .await;
        assert!(
            !std::fs::symlink_metadata(dst.join("src/c"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(std::fs::read(dst.join("src/c")).unwrap(), b"hello");
    }
    #[tokio::test]
    async fn extended_attributes_survive_streaming_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::write(&src, b"hello").unwrap();
        std::fs::create_dir(&dst).unwrap();
        xattr::set(&src, "user.newt-test", b"attribute").unwrap();
        copy(
            &src,
            &dst,
            CopyOptions {
                preserve_xattrs: true,
                preserve_sparse: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            xattr::get(dst.join("src"), "user.newt-test").unwrap(),
            Some(b"attribute".to_vec())
        );
    }
    #[tokio::test]
    async fn sparse_copy_keeps_length_and_contents() {
        use std::io::{Seek, SeekFrom, Write};
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir(&dst).unwrap();
        let mut f = std::fs::File::create(&src).unwrap();
        f.write_all(b"start").unwrap();
        f.seek(SeekFrom::Start(4 * 1024 * 1024)).unwrap();
        f.write_all(b"end").unwrap();
        f.set_len(8 * 1024 * 1024).unwrap();
        crate::vfs::local_attributes::punch_holes(
            &f,
            &[
                (4096, 4 * 1024 * 1024 - 4096),
                (4 * 1024 * 1024 + 4096, 4 * 1024 * 1024 - 4096),
            ],
        )
        .unwrap();
        drop(f);
        copy(
            &src,
            &dst,
            CopyOptions {
                preserve_sparse: true,
                ..Default::default()
            },
        )
        .await;
        let output = dst.join("src");
        assert_eq!(
            std::fs::read(&src).unwrap(),
            std::fs::read(&output).unwrap()
        );
        let source_meta = std::fs::metadata(&src).unwrap();
        let dest_meta = std::fs::metadata(output).unwrap();
        assert!(
            dest_meta.blocks() * 512 < 8 * 1024 * 1024,
            "source allocation {}, destination allocation {}",
            source_meta.blocks() * 512,
            dest_meta.blocks() * 512
        );
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn resource_fork_and_acl_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::write(&src, b"hello").unwrap();
        std::fs::create_dir(&dst).unwrap();
        xattr::set(&src, "com.apple.ResourceFork", b"fork contents").unwrap();
        copy(
            &src,
            &dst,
            CopyOptions {
                preserve_streams: true,
                preserve_acl: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            xattr::get(dst.join("src"), "com.apple.ResourceFork").unwrap(),
            Some(b"fork contents".to_vec())
        );
    }
}
