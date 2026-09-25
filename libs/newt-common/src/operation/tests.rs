use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::operation::*;
use crate::test_support::{FailureSpec, MockRefusal, MockVfs, MockVfsConfig};
use crate::vfs::path::PathBuf;
use crate::vfs::{VfsId, VfsPath, VfsRegistry};

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

struct RunResult {
    events: Vec<OperationProgress>,
    vfs: Arc<MockVfs>,
}

async fn run_operation(
    vfs: Arc<MockVfs>,
    request: OperationRequest,
    mut issue_responder: impl FnMut(&OperationIssue) -> IssueResponse,
) -> RunResult {
    run_operation_inner(vfs.clone(), None, request, &mut issue_responder).await
}

async fn run_operation_two_vfs(
    src_vfs: Arc<MockVfs>,
    dst_vfs: Arc<MockVfs>,
    request: OperationRequest,
    mut issue_responder: impl FnMut(&OperationIssue) -> IssueResponse,
) -> (RunResult, Arc<MockVfs>) {
    let result = run_operation_inner(
        src_vfs.clone(),
        Some(dst_vfs.clone()),
        request,
        &mut issue_responder,
    )
    .await;
    (result, dst_vfs)
}

async fn run_operation_inner(
    root_vfs: Arc<MockVfs>,
    second_vfs: Option<Arc<MockVfs>>,
    request: OperationRequest,
    issue_responder: &mut dyn FnMut(&OperationIssue) -> IssueResponse,
) -> RunResult {
    let registry = Arc::new(VfsRegistry::with_root(root_vfs.clone()));
    if let Some(vfs2) = &second_vfs {
        let id = registry.mount(vfs2.clone());
        assert_eq!(id, VfsId(1));
    }

    let (progress_tx, mut progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<OperationProgress>();
    let cancel = CancellationToken::new();
    let issue_resolvers: IssueResolvers = Arc::new(Mutex::new(HashMap::new()));
    let next_issue_id = Arc::new(AtomicU64::new(1));
    let context = Arc::new(OperationContext {
        registry: registry.clone(),
        shell_integration: None,
        spooler: crate::spool::Spooler::new(Default::default()),
    });

    let issue_resolvers2 = issue_resolvers.clone();
    let cancel2 = cancel.clone();

    let op_handle = tokio::spawn(execute_operation(
        1,
        request,
        progress_tx,
        cancel2,
        issue_resolvers2.clone(),
        next_issue_id,
        context,
    ));

    let mut events = Vec::new();
    while let Some(event) = progress_rx.recv().await {
        // Handle issues by responding via the resolvers
        if let OperationProgress::Issue { issue, .. } = &event {
            let response = issue_responder(issue);
            if let Some(sender) = issue_resolvers.lock().remove(&issue.issue_id) {
                let _ = sender.send(response);
            }
        }
        let is_terminal = matches!(
            &event,
            OperationProgress::Completed { .. }
                | OperationProgress::Failed { .. }
                | OperationProgress::Cancelled { .. }
        );
        events.push(event);
        if is_terminal {
            break;
        }
    }

    let _ = op_handle.await;

    RunResult {
        events,
        vfs: root_vfs,
    }
}

async fn run_operation_cancellable(
    vfs: Arc<MockVfs>,
    request: OperationRequest,
    cancel: CancellationToken,
) -> Vec<OperationProgress> {
    let registry = Arc::new(VfsRegistry::with_root(vfs));
    let (progress_tx, mut progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<OperationProgress>();
    let issue_resolvers: IssueResolvers = Arc::new(Mutex::new(HashMap::new()));
    let next_issue_id = Arc::new(AtomicU64::new(1));
    let context = Arc::new(OperationContext {
        registry,
        shell_integration: None,
        spooler: crate::spool::Spooler::new(Default::default()),
    });

    let op_handle = tokio::spawn(execute_operation(
        1,
        request,
        progress_tx,
        cancel,
        issue_resolvers,
        next_issue_id,
        context,
    ));

    let mut events = Vec::new();
    while let Some(event) = progress_rx.recv().await {
        let is_terminal = matches!(
            &event,
            OperationProgress::Completed { .. }
                | OperationProgress::Failed { .. }
                | OperationProgress::Cancelled { .. }
        );
        events.push(event);
        if is_terminal {
            break;
        }
    }

    let _ = op_handle.await;
    events
}

fn vfs_path(path: &str) -> VfsPath {
    VfsPath::from_wire_str(VfsId::ROOT, path)
}

fn vfs_path_id(vfs_id: u32, path: &str) -> VfsPath {
    VfsPath::from_wire_str(VfsId(vfs_id), path)
}

fn has_completed(events: &[OperationProgress]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, OperationProgress::Completed { .. }))
}

fn has_cancelled(events: &[OperationProgress]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, OperationProgress::Cancelled { .. }))
}

fn get_prepared(events: &[OperationProgress]) -> Option<(u64, u64)> {
    events.iter().find_map(|e| match e {
        OperationProgress::Prepared {
            total_bytes,
            total_items,
            ..
        } => Some((*total_bytes, *total_items)),
        _ => None,
    })
}

fn skip_all(_issue: &OperationIssue) -> IssueResponse {
    IssueResponse {
        action: IssueAction::Skip,
        apply_to_all: false,
    }
}

fn overwrite_all(_issue: &OperationIssue) -> IssueResponse {
    IssueResponse {
        action: IssueAction::Overwrite,
        apply_to_all: false,
    }
}

fn retry_then_skip(
    count: &std::cell::Cell<u32>,
) -> impl FnMut(&OperationIssue) -> IssueResponse + '_ {
    move |_issue| {
        let n = count.get();
        if n > 0 {
            count.set(n - 1);
            IssueResponse {
                action: IssueAction::Retry,
                apply_to_all: false,
            }
        } else {
            IssueResponse {
                action: IssueAction::Skip,
                apply_to_all: false,
            }
        }
    }
}

// ===========================================================================
// Delete tests
// ===========================================================================

#[tokio::test]
async fn test_delete_single_file() {
    let vfs = MockVfs::builder().file("/a.txt", b"hello").build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/a.txt"));
}

#[tokio::test]
async fn test_delete_directory_with_remove_tree() {
    let vfs = MockVfs::builder()
        .dir("/mydir")
        .file("/mydir/a.txt", b"a")
        .file("/mydir/b.txt", b"b")
        .dir("/mydir/sub")
        .file("/mydir/sub/c.txt", b"c")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/mydir")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/mydir"));
    assert!(!result.vfs.exists("/mydir/a.txt"));
    assert!(!result.vfs.exists("/mydir/sub/c.txt"));
}

#[tokio::test]
async fn test_delete_directory_slow_path() {
    // Disable remove_tree to force the slow recursive walk path
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_remove_tree: false,
            ..Default::default()
        })
        .dir("/mydir")
        .file("/mydir/a.txt", b"a")
        .dir("/mydir/sub")
        .file("/mydir/sub/c.txt", b"c")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/mydir")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/mydir"));
    assert!(!result.vfs.exists("/mydir/a.txt"));
    assert!(!result.vfs.exists("/mydir/sub"));
    assert!(!result.vfs.exists("/mydir/sub/c.txt"));
}

#[tokio::test]
async fn test_delete_error_skip() {
    use crate::test_support::mock_vfs::FailureSpec;

    let vfs = MockVfs::builder()
        .file("/a.txt", b"hello")
        .file("/b.txt", b"world")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/a.txt"),
            operation: "remove_tree",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "permission denied".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt"), vfs_path("/b.txt")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // a.txt should still exist (skip on error), b.txt should be gone
    assert!(result.vfs.exists("/a.txt"));
    assert!(!result.vfs.exists("/b.txt"));
}

#[tokio::test]
async fn test_delete_error_retry() {
    use crate::test_support::mock_vfs::FailureSpec;

    // Transient failure: fails once, then succeeds on retry
    let vfs = MockVfs::builder()
        .file("/a.txt", b"hello")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/a.txt"),
            operation: "remove_tree",
            error: crate::Error {
                kind: crate::ErrorKind::Other,
                message: "transient error".into(),
            },
            remaining: Some(1),
        })
        .build();

    let retry_count = std::cell::Cell::new(1u32);
    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt")],
            to_trash: false,
            cross_mount_points: false,
        },
        retry_then_skip(&retry_count),
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/a.txt"));
}

#[tokio::test]
async fn test_retry_with_apply_to_all_prompts_again() {
    let vfs = MockVfs::builder()
        .file("/a.txt", b"hello")
        .file("/b.txt", b"world")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/a.txt"),
            operation: "remove_tree",
            error: crate::Error::custom("transient error"),
            remaining: Some(2),
        })
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/b.txt"),
            operation: "remove_tree",
            error: crate::Error::custom("transient error"),
            remaining: Some(1),
        })
        .build();

    let mut issue_count = 0;
    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt"), vfs_path("/b.txt")],
            to_trash: false,
            cross_mount_points: false,
        },
        |_| {
            issue_count += 1;
            IssueResponse {
                action: if issue_count == 1 {
                    IssueAction::Retry
                } else {
                    IssueAction::Skip
                },
                apply_to_all: true,
            }
        },
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(issue_count, 2);
    assert!(result.vfs.exists("/a.txt"));
    assert!(result.vfs.exists("/b.txt"));
}

#[tokio::test]
async fn test_delete_multiple_paths() {
    let vfs = MockVfs::builder()
        .file("/a.txt", b"a")
        .file("/b.txt", b"b")
        .dir("/c")
        .file("/c/d.txt", b"d")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt"), vfs_path("/b.txt"), vfs_path("/c")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/a.txt"));
    assert!(!result.vfs.exists("/b.txt"));
    assert!(!result.vfs.exists("/c"));
    assert!(!result.vfs.exists("/c/d.txt"));
}

#[tokio::test]
async fn test_delete_symlink_not_followed_top_level() {
    // A symlink pointing at a directory with files: only the symlink should be removed
    let vfs = MockVfs::builder()
        .dir("/target_dir")
        .file("/target_dir/precious.txt", b"keep me")
        .symlink("/link_to_dir", "/target_dir")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/link_to_dir")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/link_to_dir"));
    // Target directory and its contents must survive
    assert!(result.vfs.exists("/target_dir"));
    assert_eq!(
        result.vfs.read_content("/target_dir/precious.txt"),
        b"keep me"
    );
}

#[tokio::test]
async fn test_delete_symlink_not_followed_inside_dir_fast_path() {
    // remove_tree fast path: symlink inside a deleted directory must not affect target
    let vfs = MockVfs::builder()
        .dir("/target_dir")
        .file("/target_dir/precious.txt", b"keep me")
        .dir("/mydir")
        .file("/mydir/a.txt", b"a")
        .symlink("/mydir/link_to_dir", "/target_dir")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/mydir")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/mydir"));
    assert!(!result.vfs.exists("/mydir/link_to_dir"));
    assert!(result.vfs.exists("/target_dir"));
    assert_eq!(
        result.vfs.read_content("/target_dir/precious.txt"),
        b"keep me"
    );
}

#[tokio::test]
async fn test_delete_symlink_not_followed_inside_dir_slow_path() {
    // Recursive walk (no remove_tree): symlinks inside must be deleted as files, not followed
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_remove_tree: false,
            ..Default::default()
        })
        .dir("/target_dir")
        .file("/target_dir/precious.txt", b"keep me")
        .dir("/mydir")
        .file("/mydir/a.txt", b"a")
        .symlink("/mydir/link_to_dir", "/target_dir")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/mydir")],
            to_trash: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/mydir"));
    assert!(!result.vfs.exists("/mydir/link_to_dir"));
    assert!(result.vfs.exists("/target_dir"));
    assert_eq!(
        result.vfs.read_content("/target_dir/precious.txt"),
        b"keep me"
    );
}

// ===========================================================================
// Trash tests
// ===========================================================================

#[tokio::test]
async fn test_trash_single_file() {
    let vfs = MockVfs::builder().file("/a.txt", b"hello").build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt")],
            to_trash: true,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/a.txt"));
    assert_eq!(
        result.vfs.trashed_paths(),
        vec![PathBuf::from_wire_str("/a.txt")]
    );
}

#[tokio::test]
async fn test_trash_directory_counts_one_item() {
    let vfs = MockVfs::builder()
        .dir("/mydir")
        .file("/mydir/a.txt", b"a")
        .dir("/mydir/sub")
        .file("/mydir/sub/c.txt", b"c")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/mydir")],
            to_trash: true,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // The whole tree is trashed wholesale as a single item — no scan walk.
    assert_eq!(get_prepared(&result.events), Some((0, 1)));
    assert!(!result.vfs.exists("/mydir"));
    assert!(!result.vfs.exists("/mydir/sub/c.txt"));
    assert_eq!(
        result.vfs.trashed_paths(),
        vec![PathBuf::from_wire_str("/mydir")]
    );
}

#[tokio::test]
async fn test_trash_error_skip() {
    let vfs = MockVfs::builder()
        .file("/a.txt", b"hello")
        .file("/b.txt", b"world")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/a.txt"),
            operation: "trash_item",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "permission denied".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt"), vfs_path("/b.txt")],
            to_trash: true,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(result.vfs.exists("/a.txt"));
    assert!(!result.vfs.exists("/b.txt"));
    assert_eq!(
        result.vfs.trashed_paths(),
        vec![PathBuf::from_wire_str("/b.txt")]
    );
}

#[tokio::test]
async fn test_trash_error_retry() {
    let vfs = MockVfs::builder()
        .file("/a.txt", b"hello")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/a.txt"),
            operation: "trash_item",
            error: crate::Error {
                kind: crate::ErrorKind::Other,
                message: "transient error".into(),
            },
            remaining: Some(1),
        })
        .build();

    let retry_count = std::cell::Cell::new(1u32);
    let result = run_operation(
        vfs,
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt")],
            to_trash: true,
            cross_mount_points: false,
        },
        retry_then_skip(&retry_count),
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/a.txt"));
}

#[tokio::test]
async fn test_trash_not_supported_surfaces_issue() {
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_trash: false,
            ..Default::default()
        })
        .file("/a.txt", b"hello")
        .build();

    let issue_count = std::cell::Cell::new(0u32);
    let result = run_operation(
        vfs.clone(),
        OperationRequest::Delete {
            paths: vec![vfs_path("/a.txt")],
            to_trash: true,
            cross_mount_points: false,
        },
        |issue| {
            issue_count.set(issue_count.get() + 1);
            skip_all(issue)
        },
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(issue_count.get(), 1);
    // Nothing deleted, nothing trashed.
    assert!(result.vfs.exists("/a.txt"));
    assert!(result.vfs.trashed_paths().is_empty());
}

// ===========================================================================
// Copy tests
// ===========================================================================

#[tokio::test]
async fn test_copy_single_file() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello world")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"hello world");
}

#[tokio::test]
async fn test_copy_rename_to() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: Some("b.txt".into()),
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.read_content("/dst/b.txt"), b"hello");
    assert!(!result.vfs.exists("/dst/a.txt"));
}

#[tokio::test]
async fn test_copy_directory_rename_to() {
    let vfs = MockVfs::builder()
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .dir("/src/sub")
        .file("/src/sub/b.txt", b"bbb")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: Some("renamed".into()),
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/renamed/a.txt"), b"aaa");
    assert_eq!(result.vfs.read_content("/dst/renamed/sub/b.txt"), b"bbb");
    assert!(!result.vfs.exists("/dst/src"));
}

#[tokio::test]
async fn test_copy_directory_recursive() {
    let vfs = MockVfs::builder()
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .dir("/src/sub")
        .file("/src/sub/b.txt", b"bbb")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(result.vfs.exists("/dst/src"));
    assert_eq!(result.vfs.read_content("/dst/src/a.txt"), b"aaa");
    assert!(result.vfs.exists("/dst/src/sub"));
    assert_eq!(result.vfs.read_content("/dst/src/sub/b.txt"), b"bbb");
    // Source should still exist
    assert!(result.vfs.exists("/src/a.txt"));
}

#[tokio::test]
async fn test_copy_reads_files_in_the_source_order() {
    let vfs = MockVfs::builder()
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .file("/src/c.txt", b"ccc")
        .dir("/src/sub")
        .file("/src/sub/b.txt", b"bbb")
        .read_order(&["/src/sub/b.txt", "/src/c.txt", "/src/a.txt"])
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/src/a.txt"), b"aaa");
    assert_eq!(result.vfs.read_content("/dst/src/sub/b.txt"), b"bbb");
    assert_eq!(
        result.vfs.opened_reads(),
        vec![
            PathBuf::from_wire_str("/src/sub/b.txt"),
            PathBuf::from_wire_str("/src/c.txt"),
            PathBuf::from_wire_str("/src/a.txt")
        ]
    );
}

#[tokio::test]
async fn test_copy_with_symlinks() {
    let vfs = MockVfs::builder()
        .dir("/src")
        .symlink("/src/link", "/somewhere/target")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/link")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // Should have created a symlink, not copied the target
    let snapshot = result.vfs.snapshot();
    let link_entry = snapshot
        .iter()
        .find(|(p, _)| p == &PathBuf::from_wire_str("/dst/link"));
    assert_eq!(link_entry.map(|(_, t)| *t), Some("symlink"));
}

#[tokio::test]
async fn test_copy_conflict_skip() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new content")
        .dir("/dst")
        .file("/dst/a.txt", b"old content")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // Old content should be preserved
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old content");
}

#[tokio::test]
async fn test_copy_conflict_overwrite() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new content")
        .dir("/dst")
        .file("/dst/a.txt", b"old content")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        overwrite_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new content");
}

#[tokio::test]
async fn test_copy_conflict_dir_merge() {
    // When dest dir already exists, should merge (no error)
    let vfs = MockVfs::builder()
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .dir("/dst")
        .dir("/dst/src") // already exists
        .file("/dst/src/existing.txt", b"keep")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // New file should be added
    assert_eq!(result.vfs.read_content("/dst/src/a.txt"), b"aaa");
    // Existing file should be preserved
    assert_eq!(result.vfs.read_content("/dst/src/existing.txt"), b"keep");
}

#[tokio::test]
async fn test_copy_conflict_apply_to_all() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new a")
        .file("/src/b.txt", b"new b")
        .dir("/dst")
        .file("/dst/a.txt", b"old a")
        .file("/dst/b.txt", b"old b")
        .build();

    let mut issue_count = 0u32;
    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt"), vfs_path("/src/b.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        |_issue| {
            issue_count += 1;
            IssueResponse {
                action: IssueAction::Overwrite,
                apply_to_all: true, // sticky
            }
        },
    )
    .await;

    assert!(has_completed(&result.events));
    // Both should be overwritten
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new a");
    assert_eq!(result.vfs.read_content("/dst/b.txt"), b"new b");
    // Only one issue should have been raised (second uses sticky resolution)
    assert_eq!(issue_count, 1);
}

fn answer_all(action: IssueAction) -> impl FnMut(&OperationIssue) -> IssueResponse {
    move |_issue| IssueResponse {
        action,
        apply_to_all: true,
    }
}

/// Copies `/src/<name>.txt` for each name into `/dst`.
fn copy_named(names: &[&str], conflict_resolution: Option<IssueAction>) -> OperationRequest {
    OperationRequest::Copy {
        rename_to: None,
        sources: names
            .iter()
            .map(|n| vfs_path(&format!("/src/{n}.txt")))
            .collect(),
        destination: vfs_path("/dst"),
        options: CopyOptions {
            conflict_resolution,
            ..Default::default()
        },
    }
}

fn never_asked(issue: &OperationIssue) -> IssueResponse {
    panic!("unexpected prompt: {}", issue.message)
}

#[tokio::test]
async fn test_copy_conflict_overwrite_if_newer() {
    let vfs = MockVfs::builder()
        .file("/src/newer.txt", b"new")
        .modified("/src/newer.txt", 5_000_001)
        .file("/dst/newer.txt", b"old")
        .modified("/dst/newer.txt", 5_000_000)
        .file("/src/older.txt", b"new")
        .modified("/src/older.txt", 1_000_000)
        .file("/dst/older.txt", b"old")
        .modified("/dst/older.txt", 5_000_000)
        .file("/src/equal.txt", b"new")
        .modified("/src/equal.txt", 5_000_000)
        .file("/dst/equal.txt", b"old")
        .modified("/dst/equal.txt", 5_000_000)
        .file("/src/unknown.txt", b"new")
        .file("/dst/unknown.txt", b"old")
        .modified("/dst/unknown.txt", 5_000_000)
        .build();

    let mut issues = 0;
    let result = run_operation(
        vfs,
        copy_named(&["newer", "older", "equal", "unknown"], None),
        |issue| {
            issues += 1;
            answer_all(IssueAction::OverwriteIfNewer)(issue)
        },
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(issues, 1);
    assert_eq!(result.vfs.read_content("/dst/newer.txt"), b"new");
    assert_eq!(result.vfs.read_content("/dst/older.txt"), b"old");
    assert_eq!(result.vfs.read_content("/dst/equal.txt"), b"old");
    assert_eq!(result.vfs.read_content("/dst/unknown.txt"), b"old");
}

#[tokio::test]
async fn test_copy_conflict_overwrite_if_size_differs() {
    let vfs = MockVfs::builder()
        .file("/src/same.txt", b"aaaa")
        .modified("/src/same.txt", 1_000_000)
        .file("/dst/same.txt", b"bbbb")
        .modified("/dst/same.txt", 5_000_000)
        .file("/src/resized.txt", b"aaaaa")
        .modified("/src/resized.txt", 5_000_000)
        .file("/dst/resized.txt", b"bbbb")
        .modified("/dst/resized.txt", 5_000_000)
        .build();

    let result = run_operation(
        vfs,
        copy_named(&["same", "resized"], None),
        answer_all(IssueAction::OverwriteIfSizeDiffers),
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/same.txt"), b"bbbb");
    assert_eq!(result.vfs.read_content("/dst/resized.txt"), b"aaaaa");
}

#[tokio::test]
async fn test_copy_conflict_overwrite_if_size_or_date_differs() {
    let vfs = MockVfs::builder()
        .file("/src/same.txt", b"aaaa")
        .modified("/src/same.txt", 5_000_000)
        .file("/dst/same.txt", b"bbbb")
        .modified("/dst/same.txt", 5_000_000)
        .file("/src/resized.txt", b"aaaaa")
        .modified("/src/resized.txt", 5_000_000)
        .file("/dst/resized.txt", b"bbbb")
        .modified("/dst/resized.txt", 5_000_000)
        .file("/src/touched.txt", b"aaaa")
        .modified("/src/touched.txt", 4_999_999)
        .file("/dst/touched.txt", b"bbbb")
        .modified("/dst/touched.txt", 5_000_000)
        .file("/src/unknown.txt", b"aaaa")
        .file("/dst/unknown.txt", b"bbbb")
        .build();

    let result = run_operation(
        vfs,
        copy_named(&["same", "resized", "touched", "unknown"], None),
        answer_all(IssueAction::OverwriteIfSizeOrDateDiffers),
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/same.txt"), b"bbbb");
    assert_eq!(result.vfs.read_content("/dst/resized.txt"), b"aaaaa");
    assert_eq!(result.vfs.read_content("/dst/touched.txt"), b"aaaa");
    assert_eq!(result.vfs.read_content("/dst/unknown.txt"), b"aaaa");
}

#[tokio::test]
async fn test_copy_preset_conflict_resolution_does_not_prompt() {
    let vfs = MockVfs::builder()
        .file("/src/newer.txt", b"new")
        .modified("/src/newer.txt", 10_000_000)
        .file("/dst/newer.txt", b"old")
        .modified("/dst/newer.txt", 5_000_000)
        .file("/src/older.txt", b"new")
        .modified("/src/older.txt", 1_000_000)
        .file("/dst/older.txt", b"old")
        .modified("/dst/older.txt", 5_000_000)
        .build();

    let result = run_operation(
        vfs,
        copy_named(&["newer", "older"], Some(IssueAction::OverwriteIfNewer)),
        never_asked,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/newer.txt"), b"new");
    assert_eq!(result.vfs.read_content("/dst/older.txt"), b"old");
}

#[tokio::test]
async fn test_copy_preset_skip_covers_a_directory_in_the_way() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new")
        .file("/src/b.txt", b"new")
        .file("/dst/a.txt", b"old")
        .dir("/dst/b.txt")
        .build();

    let result = run_operation(
        vfs,
        copy_named(&["a", "b"], Some(IssueAction::Skip)),
        never_asked,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old");
}

#[tokio::test]
async fn test_copy_preset_overwrite_still_asks_about_a_directory_in_the_way() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new")
        .file("/src/b.txt", b"new")
        .file("/dst/a.txt", b"old")
        .dir("/dst/b.txt")
        .build();

    let mut asked = Vec::new();
    let result = run_operation(
        vfs,
        copy_named(&["a", "b"], Some(IssueAction::Overwrite)),
        |issue| {
            asked.push(issue.actions.clone());
            skip_all(issue)
        },
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(asked, vec![vec![IssueAction::Skip]]);
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new");
}

#[tokio::test]
async fn test_move_preset_overwrite_if_newer_keeps_skipped_sources() {
    let vfs = MockVfs::builder()
        .file("/src/newer.txt", b"new")
        .modified("/src/newer.txt", 10_000_000)
        .file("/dst/newer.txt", b"old")
        .modified("/dst/newer.txt", 5_000_000)
        .file("/src/older.txt", b"new")
        .modified("/src/older.txt", 1_000_000)
        .file("/dst/older.txt", b"old")
        .modified("/dst/older.txt", 5_000_000)
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/newer.txt"), vfs_path("/src/older.txt")],
            destination: vfs_path("/dst"),
            options: CopyOptions {
                conflict_resolution: Some(IssueAction::OverwriteIfNewer),
                ..Default::default()
            },
        },
        never_asked,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/newer.txt"), b"new");
    assert!(!result.vfs.exists("/src/newer.txt"));
    assert_eq!(result.vfs.read_content("/dst/older.txt"), b"old");
    assert!(result.vfs.exists("/src/older.txt"));
}

// --- Exclusive writes: the destination refuses, the copy asks only then ---

fn mock_config(change: impl FnOnce(&mut MockVfsConfig)) -> MockVfsConfig {
    let mut config = MockVfsConfig::default();
    change(&mut config);
    config
}

fn issue_messages(events: &[OperationProgress]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            OperationProgress::Issue { issue, .. } => Some(issue.message.clone()),
            _ => None,
        })
        .collect()
}

fn failure(
    path: &str,
    operation: &'static str,
    kind: crate::ErrorKind,
    times: Option<u32>,
) -> FailureSpec {
    FailureSpec {
        path: PathBuf::from_wire_str(path),
        operation,
        error: crate::Error {
            kind,
            message: format!("injected {operation} failure"),
        },
        remaining: times,
    }
}

#[tokio::test]
async fn test_copy_into_an_empty_destination_never_looks_at_it() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"a")
        .file("/src/b.txt", b"b")
        .dir("/dst")
        .build();

    let result = run_operation(vfs, copy_named(&["a", "b"], None), never_asked).await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"a");
    assert_eq!(result.vfs.read_content("/dst/b.txt"), b"b");
    assert_eq!(result.vfs.stats_under("/dst"), Vec::<String>::new());
}

#[tokio::test]
async fn test_copy_of_a_tree_into_prefix_directories_never_looks_at_the_destination() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.can_stat_directories = false))
        .file("/src/tree/x.txt", b"x")
        .file("/src/tree/sub/y.txt", b"y")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/tree")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        never_asked,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/tree/sub/y.txt"), b"y");
    assert_eq!(result.vfs.stats_under("/dst"), Vec::<String>::new());
}

#[tokio::test]
async fn test_copy_refused_on_finish_prompts_and_reads_the_source_again() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.create_new = Some(MockRefusal::AtFinish)))
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), overwrite_all).await;

    assert!(has_completed(&result.events));
    assert_eq!(issue_messages(&result.events).len(), 1);
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new");
    assert_eq!(result.vfs.opened_reads().len(), 2);
}

#[tokio::test]
async fn test_copy_to_a_destination_that_cannot_refuse_looks_first() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.create_new = None))
        .file("/src/a.txt", b"new")
        .file("/src/b.txt", b"new")
        .file("/dst/a.txt", b"old")
        .build();

    let result = run_operation(vfs, copy_named(&["a", "b"], None), skip_all).await;

    assert!(has_completed(&result.events));
    assert_eq!(issue_messages(&result.events).len(), 1);
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old");
    assert_eq!(result.vfs.read_content("/dst/b.txt"), b"new");
    assert_eq!(
        result.vfs.stats_under("/dst"),
        vec!["/dst/a.txt", "/dst/b.txt"]
    );
}

#[tokio::test]
async fn test_copy_within_that_cannot_refuse_is_not_given_up_for_streaming() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| {
            c.can_copy_within = true;
            c.create_new = None;
        }))
        .file("/src/a.txt", b"a")
        .dir("/dst")
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), never_asked).await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"a");
    assert!(result.vfs.opened_reads().is_empty());
}

#[tokio::test]
async fn test_copy_within_refusal_prompts() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.can_copy_within = true))
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), overwrite_all).await;

    assert!(has_completed(&result.events));
    assert_eq!(issue_messages(&result.events).len(), 1);
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new");
    assert!(result.vfs.opened_reads().is_empty());
}

#[tokio::test]
async fn test_a_refusal_the_destination_contradicts_is_tried_once_more() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"a")
        .dir("/dst")
        .failure(failure(
            "/dst/a.txt",
            "overwrite_async",
            crate::ErrorKind::AlreadyExists,
            Some(1),
        ))
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), never_asked).await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"a");
}

#[tokio::test]
async fn test_a_refusal_the_destination_keeps_contradicting_is_an_issue() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"a")
        .dir("/dst")
        .failure(failure(
            "/dst/a.txt",
            "overwrite_async",
            crate::ErrorKind::AlreadyExists,
            None,
        ))
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), skip_all).await;

    assert!(has_completed(&result.events));
    let messages = issue_messages(&result.events);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("refused as existing"), "{messages:?}");
    assert!(!result.vfs.exists("/dst/a.txt"));
}

#[tokio::test]
async fn test_a_destination_that_cannot_be_looked_at_is_not_taken_for_empty() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.create_new = None))
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .failure(failure(
            "/dst/a.txt",
            "file_info",
            crate::ErrorKind::PermissionDenied,
            None,
        ))
        .build();

    let mut kinds = Vec::new();
    let result = run_operation(vfs, copy_named(&["a"], None), |issue| {
        kinds.push(issue.kind.clone());
        skip_all(issue)
    })
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(kinds, vec![IssueKind::PermissionDenied]);
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old");
}

#[tokio::test]
async fn test_a_retry_is_not_refused_by_its_own_leftovers() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"data")
        .dir("/dst")
        .failure(failure(
            "/dst/a.txt",
            "finish",
            crate::ErrorKind::Other,
            Some(1),
        ))
        .build();

    let mut messages = Vec::new();
    let result = run_operation(vfs, copy_named(&["a"], None), |issue| {
        messages.push(issue.message.clone());
        IssueResponse {
            action: IssueAction::Retry,
            apply_to_all: false,
        }
    })
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].contains("injected finish failure"),
        "{messages:?}"
    );
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"data");
}

#[tokio::test]
async fn test_an_unreadable_source_leaves_no_exclusive_create_behind() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"a")
        .dir("/dst")
        .failure(failure(
            "/src/a.txt",
            "open_read_async",
            crate::ErrorKind::PermissionDenied,
            None,
        ))
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), skip_all).await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dst/a.txt"));
}

#[tokio::test]
async fn test_a_destination_that_cannot_refuse_costs_no_second_source_open() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.create_new = None))
        .file("/src/a.txt", b"a")
        .dir("/dst")
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), never_asked).await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.opened_reads().len(), 1);
}

#[tokio::test]
async fn test_a_failed_exclusive_write_leaves_nothing() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"data")
        .dir("/dst")
        .failure(failure(
            "/dst/a.txt",
            "finish",
            crate::ErrorKind::Other,
            None,
        ))
        .build();

    let result = run_operation(vfs, copy_named(&["a"], None), skip_all).await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dst/a.txt"));
}

#[tokio::test]
async fn test_a_retry_after_a_failed_write_is_still_exclusive() {
    for refusal in [MockRefusal::AtOpen, MockRefusal::AtFinish] {
        let vfs = MockVfs::builder()
            .config(mock_config(|c| c.create_new = Some(refusal)))
            .file("/src/a.txt", b"mine")
            .dir("/dst")
            .failure(failure(
                "/dst/a.txt",
                "finish",
                crate::ErrorKind::Other,
                Some(1),
            ))
            .build();

        // Someone else writes the destination while the failure is shown.
        let planter = vfs.clone();
        let mut messages = Vec::new();
        let result = run_operation(vfs, copy_named(&["a"], None), |issue| {
            messages.push(issue.message.clone());
            if messages.len() == 1 {
                planter.put_file("/dst/a.txt", b"theirs");
                IssueResponse {
                    action: IssueAction::Retry,
                    apply_to_all: false,
                }
            } else {
                skip_all(issue)
            }
        })
        .await;

        assert!(has_completed(&result.events), "{refusal:?}");
        assert_eq!(messages.len(), 2, "{refusal:?}: {messages:?}");
        assert!(
            messages[1].contains("already exists"),
            "{refusal:?}: {messages:?}"
        );
        assert_eq!(
            result.vfs.read_content("/dst/a.txt"),
            b"theirs",
            "{refusal:?}"
        );
    }
}

#[tokio::test]
async fn test_a_failed_identity_check_is_an_issue() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .failure(failure(
            "/dst/a.txt",
            "same_file",
            crate::ErrorKind::PermissionDenied,
            Some(1),
        ))
        .build();

    let mut kinds = Vec::new();
    let result = run_operation(vfs, copy_named(&["a"], None), |issue| {
        kinds.push(issue.kind.clone());
        IssueResponse {
            action: if kinds.len() == 1 {
                IssueAction::Retry
            } else {
                IssueAction::Overwrite
            },
            apply_to_all: false,
        }
    })
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(
        kinds,
        vec![IssueKind::PermissionDenied, IssueKind::AlreadyExists]
    );
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new");
}

#[tokio::test]
async fn test_a_move_whose_identity_check_fails_can_be_skipped() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .failure(failure(
            "/dst/a.txt",
            "same_file",
            crate::ErrorKind::PermissionDenied,
            None,
        ))
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old");
    assert!(result.vfs.exists("/src/a.txt"));
}

#[tokio::test]
async fn test_a_known_skip_skips_a_refusal_without_looking() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .build();

    let result = run_operation(
        vfs,
        copy_named(&["a"], Some(IssueAction::Skip)),
        never_asked,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old");
    assert_eq!(result.vfs.stats_under("/dst"), Vec::<String>::new());
}

#[tokio::test]
async fn test_move_into_an_empty_destination_never_looks_at_it() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"a")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        never_asked,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"a");
    assert!(!result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.stats_under("/dst"), Vec::<String>::new());
}

#[tokio::test]
async fn test_move_refused_by_a_no_replace_rename_prompts() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        overwrite_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(issue_messages(&result.events).len(), 1);
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new");
    assert!(!result.vfs.exists("/src/a.txt"));
}

#[tokio::test]
async fn test_move_without_a_no_replace_rename_looks_first() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.rename_no_replace = false))
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old");
    assert!(result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.stats_under("/dst"), vec!["/dst/a.txt"]);
}

#[tokio::test]
async fn test_move_that_cannot_look_at_the_destination_does_not_rename_over_it() {
    let vfs = MockVfs::builder()
        .config(mock_config(|c| c.rename_no_replace = false))
        .file("/src/a.txt", b"new")
        .file("/dst/a.txt", b"old")
        .failure(failure(
            "/dst/a.txt",
            "file_info",
            crate::ErrorKind::PermissionDenied,
            None,
        ))
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"old");
    assert!(result.vfs.exists("/src/a.txt"));
}

#[tokio::test]
async fn test_rename_to_a_free_name_never_looks_at_it() {
    let vfs = MockVfs::builder().file("/dir/a.txt", b"a").build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/a.txt"),
            new_name: "b.txt".into(),
        },
        never_asked,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dir/b.txt"), b"a");
    assert_eq!(result.vfs.stats_under("/dir/b.txt"), Vec::<String>::new());
}

#[tokio::test]
async fn test_sticky_answer_only_covers_issues_that_offer_it() {
    // "Overwrite all" on a file conflict, then a directory in the way of a
    // file: the second prompt offers only Skip and must still be asked.
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"new")
        .file("/src/b.txt", b"new")
        .file("/dst/a.txt", b"old")
        .dir("/dst/b.txt")
        .build();

    let mut asked = Vec::new();
    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt"), vfs_path("/src/b.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        |issue| {
            asked.push(issue.actions.clone());
            let action = if issue.actions.contains(&IssueAction::Overwrite) {
                IssueAction::Overwrite
            } else {
                IssueAction::Skip
            };
            IssueResponse {
                action,
                apply_to_all: true,
            }
        },
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1], vec![IssueAction::Skip]);
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"new");
}

#[tokio::test]
async fn test_copy_preserves_metadata() {
    let vfs = MockVfs::builder()
        .file_with_mode("/src/a.txt", b"hello", 0o755)
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: CopyOptions {
                preserve_timestamps: true,
                ..Default::default()
            },
            rename_to: None,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.get_mode("/dst/a.txt"), Some(0o755));
}

#[tokio::test]
async fn test_copy_create_symlink_option() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: CopyOptions {
                create_symlink: true,
                ..Default::default()
            },
            rename_to: None,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    let snapshot = result.vfs.snapshot();
    let link_entry = snapshot
        .iter()
        .find(|(p, _)| p == &PathBuf::from_wire_str("/dst/a.txt"));
    assert_eq!(link_entry.map(|(_, t)| *t), Some("symlink"));
}

// ===========================================================================
// Move tests
// ===========================================================================

#[tokio::test]
async fn test_move_same_vfs_rename() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"hello");
}

#[tokio::test]
async fn test_move_rename_to_fast_path() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: Some("b.txt".into()),
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.read_content("/dst/b.txt"), b"hello");
    assert!(!result.vfs.exists("/dst/a.txt"));
}

#[tokio::test]
async fn test_move_rename_to_copy_fallback() {
    use crate::test_support::mock_vfs::FailureSpec;

    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/src/a.txt"),
            operation: "rename",
            error: crate::Error {
                kind: crate::ErrorKind::NotSupported,
                message: "cross-device link".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: Some("b.txt".into()),
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.read_content("/dst/b.txt"), b"hello");
}

#[tokio::test]
async fn test_move_cross_vfs() {
    let src_vfs = MockVfs::builder().file("/data/a.txt", b"cross-vfs").build();

    let dst_vfs = MockVfs::builder().dir("/target").build();

    let (result, dst) = run_operation_two_vfs(
        src_vfs,
        dst_vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path_id(0, "/data/a.txt")],
            destination: vfs_path_id(1, "/target"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/data/a.txt"));
    assert_eq!(dst.read_content("/target/a.txt"), b"cross-vfs");
}

#[tokio::test]
async fn test_move_rename_fails_fallback() {
    use crate::test_support::mock_vfs::FailureSpec;

    // Cross-device rename (EXDEV) maps to NotSupported — the only error
    // kind that cascades to copy+delete.
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/src/a.txt"),
            operation: "rename",
            error: crate::Error {
                kind: crate::ErrorKind::NotSupported,
                message: "cross-device link".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"hello");
}

#[tokio::test]
async fn test_move_rename_real_error_raises_issue() {
    use crate::test_support::mock_vfs::FailureSpec;

    // A non-NotSupported rename failure must NOT silently degrade to
    // copy+delete — it surfaces as an issue (Skip leaves the source put).
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/src/a.txt"),
            operation: "rename",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "permission denied".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, OperationProgress::Issue { .. }))
    );
    assert_eq!(result.vfs.read_content("/src/a.txt"), b"hello");
    assert!(!result.vfs.exists("/dst/a.txt"));
}

#[tokio::test]
async fn test_move_file_overwrite() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"source")
        .dir("/dst")
        .file("/dst/a.txt", b"existing")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        overwrite_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"source");
}

#[tokio::test]
async fn test_move_overwrite_no_replace_backend() {
    use crate::test_support::mock_vfs::FailureSpec;

    // Backend whose rename refuses to replace an existing destination
    // (AlreadyExists): after the approved overwrite, the destination is
    // cleared and the rename retried — no copy+delete degradation.
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"source")
        .dir("/dst")
        .file("/dst/a.txt", b"existing")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/src/a.txt"),
            operation: "rename",
            error: crate::Error {
                kind: crate::ErrorKind::AlreadyExists,
                message: "destination exists".into(),
            },
            remaining: Some(1),
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        overwrite_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/src/a.txt"));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"source");
}

#[tokio::test]
async fn test_move_directory_merge_into_existing() {
    // Directory onto existing directory: merged via the copy machinery
    // (rename is never attempted over an existing destination).
    let vfs = MockVfs::builder()
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .dir("/dst")
        .dir("/dst/src")
        .file("/dst/src/existing.txt", b"keep")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/src"));
    assert_eq!(result.vfs.read_content("/dst/src/a.txt"), b"aaa");
    assert_eq!(result.vfs.read_content("/dst/src/existing.txt"), b"keep");
}

#[tokio::test]
async fn test_move_directory_cleanup() {
    // Use a config without rename to force copy+delete path
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .dir("/src/sub")
        .file("/src/sub/b.txt", b"bbb")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // Source should be fully removed
    assert!(!result.vfs.exists("/src"));
    assert!(!result.vfs.exists("/src/a.txt"));
    assert!(!result.vfs.exists("/src/sub"));
    // Destination should have the files
    assert_eq!(result.vfs.read_content("/dst/src/a.txt"), b"aaa");
    assert_eq!(result.vfs.read_content("/dst/src/sub/b.txt"), b"bbb");
}

#[tokio::test]
async fn test_move_partial_skip() {
    // When a file copy is skipped during move, the source file+dir should remain
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .file("/src/b.txt", b"bbb")
        .dir("/dst")
        .file("/dst/src/a.txt", b"existing") // conflict for a.txt
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all, // skip on conflict
    )
    .await;

    assert!(has_completed(&result.events));
    // a.txt was skipped, so source a.txt should remain
    assert!(result.vfs.exists("/src/a.txt"));
    // b.txt should have been moved
    assert!(!result.vfs.exists("/src/b.txt"));
    assert_eq!(result.vfs.read_content("/dst/src/b.txt"), b"bbb");
    // /src dir should still exist (has a.txt in it)
    assert!(result.vfs.exists("/src"));
}

#[tokio::test]
async fn test_move_symlink_not_followed_top_level() {
    // Moving a symlink should move the link itself, not follow it
    let vfs = MockVfs::builder()
        .dir("/target_dir")
        .file("/target_dir/precious.txt", b"keep me")
        .dir("/src")
        .symlink("/src/link_to_dir", "/target_dir")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/link_to_dir")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // Link should be moved, not the target
    assert!(!result.vfs.exists("/src/link_to_dir"));
    let snapshot = result.vfs.snapshot();
    let moved = snapshot
        .iter()
        .find(|(p, _)| p == &PathBuf::from_wire_str("/dst/link_to_dir"));
    assert_eq!(moved.map(|(_, t)| *t), Some("symlink"));
    // Target must be untouched
    assert!(result.vfs.exists("/target_dir"));
    assert_eq!(
        result.vfs.read_content("/target_dir/precious.txt"),
        b"keep me"
    );
}

#[tokio::test]
async fn test_move_symlink_not_followed_inside_dir() {
    // Moving a directory containing a symlink: the symlink should be
    // recreated at the destination, not followed and copied recursively
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false, // force copy+delete path
            ..Default::default()
        })
        .dir("/target_dir")
        .file("/target_dir/precious.txt", b"keep me")
        .dir("/src")
        .file("/src/a.txt", b"aaa")
        .symlink("/src/link_to_dir", "/target_dir")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // Source should be fully removed
    assert!(!result.vfs.exists("/src"));
    // Regular file should be moved
    assert_eq!(result.vfs.read_content("/dst/src/a.txt"), b"aaa");
    // Symlink should be recreated as a symlink, not as a directory copy
    let snapshot = result.vfs.snapshot();
    let moved = snapshot
        .iter()
        .find(|(p, _)| p == &PathBuf::from_wire_str("/dst/src/link_to_dir"));
    assert_eq!(moved.map(|(_, t)| *t), Some("symlink"));
    // The target directory contents at /dst/src/ should NOT contain target_dir's children
    assert!(!result.vfs.exists("/dst/src/link_to_dir/precious.txt"));
    // Original target must be untouched
    assert!(result.vfs.exists("/target_dir"));
    assert_eq!(
        result.vfs.read_content("/target_dir/precious.txt"),
        b"keep me"
    );
}

// ===========================================================================
// copy_within strategy cascade
// ===========================================================================

#[tokio::test]
async fn test_copy_within_unsupported_falls_back_to_streaming() {
    use crate::test_support::mock_vfs::FailureSpec;

    // copy_within advertised but NotSupported for this pair (e.g. a
    // cross-filesystem pair inside a RootVfs) — cascade to streaming.
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_copy_within: true,
            ..Default::default()
        })
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/src/a.txt"),
            operation: "copy_within",
            error: crate::Error {
                kind: crate::ErrorKind::NotSupported,
                message: "cross-device copy".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"hello");
    assert_eq!(result.vfs.read_content("/src/a.txt"), b"hello");
}

#[tokio::test]
async fn test_copy_within_real_error_raises_issue() {
    use crate::test_support::mock_vfs::FailureSpec;

    // A real copy_within failure (throttling, permissions) must surface
    // as an issue instead of silently re-streaming the file.
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_copy_within: true,
            ..Default::default()
        })
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/src/a.txt"),
            operation: "copy_within",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "access denied".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, OperationProgress::Issue { .. }))
    );
    assert!(!result.vfs.exists("/dst/a.txt"));
}

// ===========================================================================
// Rename tests
// ===========================================================================

#[tokio::test]
async fn test_rename_native() {
    let vfs = MockVfs::builder().file("/dir/old.txt", b"hello").build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dir/old.txt"));
    assert_eq!(result.vfs.read_content("/dir/new.txt"), b"hello");
}

#[tokio::test]
async fn test_rename_fallback_file() {
    // No native rename (S3-like) — copy+delete under the hood.
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .file("/dir/old.txt", b"hello")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dir/old.txt"));
    assert_eq!(result.vfs.read_content("/dir/new.txt"), b"hello");
}

#[tokio::test]
async fn test_rename_fallback_directory() {
    // Directory rename without native rename walks and re-creates the
    // whole tree under the new name, then removes the source.
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .dir("/data/old")
        .file("/data/old/a.txt", b"aaa")
        .dir("/data/old/sub")
        .file("/data/old/sub/b.txt", b"bbb")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/data/old"),
            new_name: "new".into(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/data/old"));
    assert!(!result.vfs.exists("/data/old/a.txt"));
    assert_eq!(result.vfs.read_content("/data/new/a.txt"), b"aaa");
    assert_eq!(result.vfs.read_content("/data/new/sub/b.txt"), b"bbb");
}

#[tokio::test]
async fn test_rename_native_fails_fallback() {
    // can_rename is true but the call reports NotSupported (e.g. a
    // cross-device pair inside a RootVfs) — fall back to copy+delete.
    let vfs = MockVfs::builder()
        .file("/dir/old.txt", b"hello")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dir/old.txt"),
            operation: "rename",
            error: crate::Error {
                kind: crate::ErrorKind::NotSupported,
                message: "simulated cross-device rename".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dir/old.txt"));
    assert_eq!(result.vfs.read_content("/dir/new.txt"), b"hello");
}

#[tokio::test]
async fn test_rename_real_error_raises_issue() {
    // A non-NotSupported rename failure surfaces as an issue; Skip
    // completes the operation with the source untouched.
    let vfs = MockVfs::builder()
        .file("/dir/old.txt", b"hello")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dir/old.txt"),
            operation: "rename",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "permission denied".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, OperationProgress::Issue { .. }))
    );
    assert_eq!(result.vfs.read_content("/dir/old.txt"), b"hello");
    assert!(!result.vfs.exists("/dir/new.txt"));
}

#[tokio::test]
async fn test_rename_conflict_skip() {
    let vfs = MockVfs::builder()
        .file("/dir/old.txt", b"source")
        .file("/dir/new.txt", b"existing")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dir/old.txt"), b"source");
    assert_eq!(result.vfs.read_content("/dir/new.txt"), b"existing");
}

#[tokio::test]
async fn test_rename_conflict_overwrite() {
    let vfs = MockVfs::builder()
        .file("/dir/old.txt", b"source")
        .file("/dir/new.txt", b"existing")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        overwrite_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dir/old.txt"));
    assert_eq!(result.vfs.read_content("/dir/new.txt"), b"source");
}

#[tokio::test]
async fn test_rename_fallback_conflict_overwrite() {
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .file("/dir/old.txt", b"source")
        .file("/dir/new.txt", b"existing")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        overwrite_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dir/old.txt"));
    assert_eq!(result.vfs.read_content("/dir/new.txt"), b"source");
}

#[tokio::test]
async fn test_rename_overwrite_no_replace_backend() {
    // Same clear-and-retry as Move when the backend's rename won't
    // replace an existing destination.
    let vfs = MockVfs::builder()
        .file("/dir/old.txt", b"source")
        .file("/dir/new.txt", b"existing")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/dir/old.txt"),
            operation: "rename",
            error: crate::Error {
                kind: crate::ErrorKind::AlreadyExists,
                message: "destination exists".into(),
            },
            remaining: Some(1),
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "new.txt".into(),
        },
        overwrite_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/dir/old.txt"));
    assert_eq!(result.vfs.read_content("/dir/new.txt"), b"source");
}

#[tokio::test]
async fn test_rename_same_name_noop() {
    // Renaming to the current name must not go anywhere near the
    // copy+delete fallback (copy-onto-self would destroy the file).
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .file("/dir/old.txt", b"hello")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/old.txt"),
            new_name: "old.txt".into(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dir/old.txt"), b"hello");
}

// ===========================================================================
// SetMetadata tests
// ===========================================================================

#[tokio::test]
async fn test_set_permissions_single_file() {
    let vfs = MockVfs::builder()
        .file_with_mode("/a.txt", b"hello", 0o644)
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::SetMetadata {
            paths: vec![vfs_path("/a.txt")],
            mode_set: 0o111,
            mode_clear: 0,
            uid: None,
            gid: None,
            recursive: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.get_mode("/a.txt"), Some(0o755));
}

#[tokio::test]
async fn test_set_permissions_recursive() {
    let vfs = MockVfs::builder()
        .dir_with_mode("/mydir", 0o755)
        .file_with_mode("/mydir/a.txt", b"a", 0o644)
        .dir_with_mode("/mydir/sub", 0o755)
        .file_with_mode("/mydir/sub/b.txt", b"b", 0o644)
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::SetMetadata {
            paths: vec![vfs_path("/mydir")],
            mode_set: 0o700,
            mode_clear: 0o077,
            uid: None,
            gid: None,
            recursive: true,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.get_mode("/mydir"), Some(0o700));
    assert_eq!(result.vfs.get_mode("/mydir/a.txt"), Some(0o700));
    assert_eq!(result.vfs.get_mode("/mydir/sub"), Some(0o700));
    assert_eq!(result.vfs.get_mode("/mydir/sub/b.txt"), Some(0o700));
}

#[tokio::test]
async fn test_set_permissions_error_skip() {
    use crate::test_support::mock_vfs::FailureSpec;

    let vfs = MockVfs::builder()
        .file_with_mode("/a.txt", b"a", 0o644)
        .file_with_mode("/b.txt", b"b", 0o644)
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/a.txt"),
            operation: "set_metadata",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "permission denied".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::SetMetadata {
            paths: vec![vfs_path("/a.txt"), vfs_path("/b.txt")],
            mode_set: 0o111,
            mode_clear: 0,
            uid: None,
            gid: None,
            recursive: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // a.txt should be unchanged (error skipped), b.txt should be updated
    assert_eq!(result.vfs.get_mode("/a.txt"), Some(0o644));
    assert_eq!(result.vfs.get_mode("/b.txt"), Some(0o755));
}

#[tokio::test]
async fn test_set_metadata_mask() {
    // Verify that only specified bits change, others preserved
    // File has 0o644, mode_set=0o100 (add owner execute), mode_clear=0o004 (remove other read)
    // Result: (0o644 | 0o100) & !0o004 = 0o740
    let vfs = MockVfs::builder()
        .file_with_mode("/a.txt", b"hello", 0o644)
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::SetMetadata {
            paths: vec![vfs_path("/a.txt")],
            mode_set: 0o100,
            mode_clear: 0o004,
            uid: None,
            gid: None,
            recursive: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.get_mode("/a.txt"), Some(0o740));
}

#[tokio::test]
async fn test_set_metadata_uid_gid() {
    let vfs = MockVfs::builder()
        .file_with_owner("/a.txt", b"hello", 0o644, 1000, 1000)
        .file_with_owner("/b.txt", b"world", 0o644, 1000, 1000)
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::SetMetadata {
            paths: vec![vfs_path("/a.txt"), vfs_path("/b.txt")],
            mode_set: 0,
            mode_clear: 0,
            uid: Some(500),
            gid: Some(600),
            recursive: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // Mode should be unchanged
    assert_eq!(result.vfs.get_mode("/a.txt"), Some(0o644));
    assert_eq!(result.vfs.get_mode("/b.txt"), Some(0o644));
    // uid/gid should be updated
    assert_eq!(result.vfs.get_uid("/a.txt"), Some(500));
    assert_eq!(result.vfs.get_gid("/a.txt"), Some(600));
    assert_eq!(result.vfs.get_uid("/b.txt"), Some(500));
    assert_eq!(result.vfs.get_gid("/b.txt"), Some(600));
}

#[tokio::test]
async fn test_set_metadata_uid_gid_recursive() {
    let vfs = MockVfs::builder()
        .dir_with_owner("/mydir", 0o755, 1000, 1000)
        .file_with_owner("/mydir/a.txt", b"a", 0o644, 1000, 1000)
        .dir_with_owner("/mydir/sub", 0o755, 1000, 1000)
        .file_with_owner("/mydir/sub/b.txt", b"b", 0o644, 1000, 1000)
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::SetMetadata {
            paths: vec![vfs_path("/mydir")],
            mode_set: 0,
            mode_clear: 0,
            uid: Some(500),
            gid: Some(600),
            recursive: true,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // Mode should be unchanged
    assert_eq!(result.vfs.get_mode("/mydir"), Some(0o755));
    assert_eq!(result.vfs.get_mode("/mydir/a.txt"), Some(0o644));
    assert_eq!(result.vfs.get_mode("/mydir/sub"), Some(0o755));
    assert_eq!(result.vfs.get_mode("/mydir/sub/b.txt"), Some(0o644));
    // uid/gid should be updated
    assert_eq!(result.vfs.get_uid("/mydir"), Some(500));
    assert_eq!(result.vfs.get_gid("/mydir"), Some(600));
    assert_eq!(result.vfs.get_uid("/mydir/a.txt"), Some(500));
    assert_eq!(result.vfs.get_gid("/mydir/a.txt"), Some(600));
    assert_eq!(result.vfs.get_uid("/mydir/sub"), Some(500));
    assert_eq!(result.vfs.get_gid("/mydir/sub"), Some(600));
    assert_eq!(result.vfs.get_uid("/mydir/sub/b.txt"), Some(500));
    assert_eq!(result.vfs.get_gid("/mydir/sub/b.txt"), Some(600));
}

#[tokio::test]
async fn test_set_metadata_uid_gid_error_skip() {
    use crate::test_support::mock_vfs::FailureSpec;

    let vfs = MockVfs::builder()
        .file_with_owner("/a.txt", b"a", 0o644, 1000, 1000)
        .file_with_owner("/b.txt", b"b", 0o644, 1000, 1000)
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/a.txt"),
            operation: "set_metadata",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "permission denied".into(),
            },
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::SetMetadata {
            paths: vec![vfs_path("/a.txt"), vfs_path("/b.txt")],
            mode_set: 0,
            mode_clear: 0,
            uid: Some(500),
            gid: Some(600),
            recursive: false,
            cross_mount_points: false,
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    // a.txt should be unchanged (error skipped)
    assert_eq!(result.vfs.get_uid("/a.txt"), Some(1000));
    assert_eq!(result.vfs.get_gid("/a.txt"), Some(1000));
    // b.txt should be updated
    assert_eq!(result.vfs.get_uid("/b.txt"), Some(500));
    assert_eq!(result.vfs.get_gid("/b.txt"), Some(600));
}

// ===========================================================================
// Cancellation & Progress tests
// ===========================================================================

#[tokio::test]
async fn test_copy_cancelled() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .build();

    let cancel = CancellationToken::new();

    // Cancel immediately before starting
    cancel.cancel();

    let events = run_operation_cancellable(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        cancel,
    )
    .await;

    assert!(has_cancelled(&events));
}

#[tokio::test]
async fn test_progress_events_correct() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .file("/src/b.txt", b"world")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt"), vfs_path("/src/b.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));

    // Check Prepared event has correct totals
    let (total_bytes, total_items) = get_prepared(&result.events).unwrap();
    assert_eq!(total_bytes, 10); // "hello" + "world"
    assert_eq!(total_items, 2);
}

// ===========================================================================
// CreateArchive tests
// ===========================================================================

fn archive_options(format: ArchiveFormat) -> ArchiveOptions {
    ArchiveOptions {
        format,
        level: Some(1),
        preserve_symlinks: true,
        password: None,
    }
}

fn archive_source_vfs() -> Arc<MockVfs> {
    MockVfs::builder()
        .dir("/data")
        .file_with_mode("/data/hello.txt", b"hello world", 0o640)
        .dir("/data/sub")
        .file("/data/sub/nested.txt", b"nested content")
        .dir("/data/empty")
        .symlink("/data/link", "hello.txt")
        .build()
}

struct TarCheck {
    is_dir: bool,
    is_symlink: bool,
    symlink_target: Option<String>,
    mode: Option<u32>,
    content: Option<Vec<u8>>,
}

/// Read an archive produced by CreateArchive back through the read-side
/// `TarArchiveVfs` — the strongest oracle we have in-tree.
async fn read_back_tar(bytes: &[u8], name: &str) -> std::collections::BTreeMap<String, TarCheck> {
    use crate::vfs::{NoopProgressSink, ScopedReporter, TarArchiveVfs, Vfs};

    let path = format!("/{name}");
    let upstream = MockVfs::builder().file(&path, bytes).build();
    let reporter: Arc<dyn crate::vfs::ProgressReporter> =
        Arc::new(ScopedReporter::new(Arc::new(NoopProgressSink), VfsId(1)));
    let vfs = Arc::new(TarArchiveVfs::new(
        upstream,
        PathBuf::from_wire_str(&path),
        VfsPath::root(VfsId(1)),
        Vec::new(),
        reporter,
    ));

    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![PathBuf::root()];
    while let Some(dir) = stack.pop() {
        let list = vfs.list_files(&dir, None).await.unwrap();
        for f in list.files {
            if f.name == ".." {
                continue;
            }
            let p = dir.join(&f.name);
            if f.is_dir && !f.is_symlink {
                stack.push(p.clone());
            }
            let content = if !f.is_dir && !f.is_symlink {
                let mut reader = vfs.open_read_async(&p).await.unwrap();
                let mut data = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut data)
                    .await
                    .unwrap();
                Some(data)
            } else {
                None
            };
            out.insert(
                p.as_wire_str().trim_start_matches('/').to_string(),
                TarCheck {
                    is_dir: f.is_dir,
                    is_symlink: f.is_symlink,
                    symlink_target: f.symlink_target.clone(),
                    mode: f.mode.as_ref().map(|m| m.0),
                    content,
                },
            );
        }
    }
    out
}

#[tokio::test]
async fn test_create_archive_tar_round_trip() {
    let vfs = archive_source_vfs();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data")],
            destination: vfs_path("/out.tar.zst"),
            options: archive_options(ArchiveFormat::TarZst),
        },
        |issue| panic!("unexpected issue: {}", issue.message),
    )
    .await;
    assert!(has_completed(&result.events), "{:?}", result.events);

    let (total_bytes, total_items) = get_prepared(&result.events).unwrap();
    assert_eq!(
        total_bytes,
        ("hello world".len() + "nested content".len()) as u64
    );
    assert_eq!(total_items, 6);

    let bytes = result.vfs.read_content("/out.tar.zst");
    let entries = read_back_tar(&bytes, "check.tar.zst").await;

    assert!(entries["data"].is_dir);
    assert!(entries["data/empty"].is_dir);
    assert_eq!(
        entries["data/hello.txt"].content.as_deref(),
        Some(&b"hello world"[..])
    );
    assert_eq!(entries["data/hello.txt"].mode, Some(0o640));
    assert_eq!(
        entries["data/sub/nested.txt"].content.as_deref(),
        Some(&b"nested content"[..])
    );
    assert!(entries["data/link"].is_symlink);
    assert_eq!(
        entries["data/link"].symlink_target.as_deref(),
        Some("hello.txt")
    );
}

/// Read a 7z produced by CreateArchive back through the 7z VFS.
async fn read_back_7z(
    bytes: &[u8],
    password: Option<&str>,
) -> std::collections::BTreeMap<String, TarCheck> {
    use crate::vfs::{NoopProgressSink, ScopedReporter, SevenZArchiveVfs, Vfs};

    struct Password(Option<String>);
    #[async_trait::async_trait]
    impl crate::askpass::AskpassProvider for Password {
        async fn prompt(
            &self,
            _req: crate::askpass::AskpassRequest,
        ) -> crate::askpass::AskpassResponse {
            crate::askpass::AskpassResponse(self.0.clone())
        }
    }

    let upstream = MockVfs::builder().file("/out.7z", bytes).build();
    let reporter: Arc<dyn crate::vfs::ProgressReporter> =
        Arc::new(ScopedReporter::new(Arc::new(NoopProgressSink), VfsId(1)));
    let askpass: Arc<dyn crate::askpass::AskpassProvider> =
        Arc::new(Password(password.map(str::to_owned)));
    let vfs = Arc::new(SevenZArchiveVfs::new(
        upstream,
        PathBuf::from_wire_str("/out.7z"),
        VfsPath::root(VfsId(1)),
        Vec::new(),
        "out.7z".to_string(),
        Some(askpass),
        reporter,
    ));

    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![PathBuf::root()];
    while let Some(dir) = stack.pop() {
        let list = vfs.list_files(&dir, None).await.unwrap();
        for f in list.files {
            if f.name == ".." {
                continue;
            }
            let p = dir.join(&f.name);
            if f.is_dir {
                stack.push(p.clone());
            }
            let content = if !f.is_dir && !f.is_symlink {
                let mut reader = vfs.open_read_async(&p).await.unwrap();
                let mut data = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut data)
                    .await
                    .unwrap();
                Some(data)
            } else {
                None
            };
            out.insert(
                p.as_wire_str().trim_start_matches('/').to_string(),
                TarCheck {
                    is_dir: f.is_dir,
                    is_symlink: f.is_symlink,
                    symlink_target: f.symlink_target.clone(),
                    mode: f.mode.as_ref().map(|m| m.0),
                    content,
                },
            );
        }
    }
    out
}

/// 7z lands its start header last: straight into a destination that
/// overwrites in place, or spooled and uploaded whole otherwise. Either
/// way the archive reads back the same.
#[tokio::test]
async fn test_create_archive_7z_round_trip_direct_and_spooled() {
    for (can_write_range, password) in [(true, None), (false, None), (true, Some("secret"))] {
        let vfs = MockVfs::builder()
            .config(MockVfsConfig {
                can_write_range,
                ..MockVfsConfig::default()
            })
            .dir("/data")
            .file_with_mode("/data/hello.txt", b"hello world", 0o640)
            .dir("/data/sub")
            .file("/data/sub/nested.txt", b"nested content")
            .file("/data/empty.txt", b"")
            .dir("/data/empty")
            .symlink("/data/link", "hello.txt")
            .build();
        let result = run_operation(
            vfs,
            OperationRequest::CreateArchive {
                sources: vec![vfs_path("/data")],
                destination: vfs_path("/out.7z"),
                options: ArchiveOptions {
                    level: Some(3),
                    password: password.map(str::to_owned),
                    ..archive_options(ArchiveFormat::SevenZ)
                },
            },
            |issue| panic!("unexpected issue: {}", issue.message),
        )
        .await;
        assert!(
            has_completed(&result.events),
            "write_range={} {:?}",
            can_write_range,
            result.events
        );

        let bytes = result.vfs.read_content("/out.7z");
        assert_eq!(&bytes[..6], b"7z\xbc\xaf\x27\x1c");
        let entries = read_back_7z(&bytes, password).await;
        assert!(entries["data"].is_dir);
        assert!(entries["data/empty"].is_dir);
        assert_eq!(
            entries["data/hello.txt"].content.as_deref(),
            Some(&b"hello world"[..])
        );
        assert_eq!(entries["data/hello.txt"].mode, Some(0o640));
        assert_eq!(entries["data/empty.txt"].content.as_deref(), Some(&b""[..]));
        assert_eq!(
            entries["data/sub/nested.txt"].content.as_deref(),
            Some(&b"nested content"[..])
        );
        assert!(entries["data/link"].is_symlink);
        // Link targets are entry content; the VFS leaves those in
        // encrypted folders unresolved rather than prompting at mount.
        if password.is_none() {
            assert_eq!(
                entries["data/link"].symlink_target.as_deref(),
                Some("hello.txt")
            );
        }
    }
}

#[tokio::test]
async fn test_create_archive_zip_round_trip() {
    use std::io::Read;

    let vfs = archive_source_vfs();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data")],
            destination: vfs_path("/out.zip"),
            options: ArchiveOptions {
                level: Some(6),
                ..archive_options(ArchiveFormat::Zip)
            },
        },
        |issue| panic!("unexpected issue: {}", issue.message),
    )
    .await;
    assert!(has_completed(&result.events), "{:?}", result.events);

    let bytes = result.vfs.read_content("/out.zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();

    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(names.contains(&"data/".to_string()));
    assert!(names.contains(&"data/empty/".to_string()));

    let mut hello = archive.by_name("data/hello.txt").unwrap();
    assert_eq!(hello.unix_mode().unwrap() & 0o7777, 0o640);
    let mut content = Vec::new();
    hello.read_to_end(&mut content).unwrap();
    assert_eq!(content, b"hello world");
    drop(hello);

    let mut link = archive.by_name("data/link").unwrap();
    assert_eq!(link.unix_mode().unwrap() & 0o170000, 0o120000);
    let mut target = String::new();
    link.read_to_string(&mut target).unwrap();
    assert_eq!(target, "hello.txt");
}

#[tokio::test]
async fn test_create_archive_zip_encrypted() {
    use std::io::Read;

    let vfs = archive_source_vfs();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data")],
            destination: vfs_path("/out.zip"),
            options: ArchiveOptions {
                password: Some("hunter2".to_string()),
                ..archive_options(ArchiveFormat::Zip)
            },
        },
        |issue| panic!("unexpected issue: {}", issue.message),
    )
    .await;
    assert!(has_completed(&result.events), "{:?}", result.events);

    let bytes = result.vfs.read_content("/out.zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();

    let index = archive.index_for_name("data/hello.txt").unwrap();
    let mut file = archive.by_index_decrypt(index, b"hunter2").unwrap();
    let mut content = Vec::new();
    file.read_to_end(&mut content).unwrap();
    assert_eq!(content, b"hello world");
    drop(file);

    assert!(archive.by_index_decrypt(index, b"wrong").is_err());
}

#[tokio::test]
async fn test_create_archive_password_rejected_for_tar() {
    let vfs = archive_source_vfs();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data")],
            destination: vfs_path("/out.tar"),
            options: ArchiveOptions {
                password: Some("nope".to_string()),
                ..archive_options(ArchiveFormat::Tar)
            },
        },
        skip_all,
    )
    .await;
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, OperationProgress::Failed { .. }))
    );
}

#[tokio::test]
async fn test_create_archive_dest_exists_skip_cancels() {
    let vfs = MockVfs::builder()
        .dir("/data")
        .file("/data/hello.txt", b"hello world")
        // Pre-existing artifact at the destination.
        .file("/out.tar", b"old bytes")
        .build();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data")],
            destination: vfs_path("/out.tar"),
            options: archive_options(ArchiveFormat::Tar),
        },
        skip_all,
    )
    .await;
    assert!(has_cancelled(&result.events), "{:?}", result.events);
    assert_eq!(result.vfs.read_content("/out.tar"), b"old bytes");
}

#[tokio::test]
async fn test_create_archive_dest_exists_overwrite_excludes_old_archive() {
    // The stale artifact sits INSIDE the tree being archived: overwrite must
    // replace it and the walk must not pack the old archive into the new one.
    let vfs = MockVfs::builder()
        .dir("/data")
        .file("/data/hello.txt", b"hello world")
        .file("/data/out.tar", b"stale archive")
        .build();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data/hello.txt"), vfs_path("/data/out.tar")],
            destination: vfs_path("/data/out.tar"),
            options: archive_options(ArchiveFormat::Tar),
        },
        overwrite_all,
    )
    .await;
    assert!(has_completed(&result.events), "{:?}", result.events);

    let entries = read_back_tar(&result.vfs.read_content("/data/out.tar"), "check.tar").await;
    assert_eq!(
        entries["hello.txt"].content.as_deref(),
        Some(&b"hello world"[..])
    );
    assert!(!entries.contains_key("out.tar"));
}

#[tokio::test]
async fn test_create_archive_duplicate_top_level_fails() {
    let vfs = MockVfs::builder()
        .dir("/a")
        .dir("/b")
        .file("/a/x.txt", b"one")
        .file("/b/x.txt", b"two")
        .build();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/a/x.txt"), vfs_path("/b/x.txt")],
            destination: vfs_path("/out.tar"),
            options: archive_options(ArchiveFormat::Tar),
        },
        skip_all,
    )
    .await;
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, OperationProgress::Failed { .. })),
        "{:?}",
        result.events
    );
    assert!(!result.vfs.exists("/out.tar"));
}

#[tokio::test]
async fn test_create_archive_follow_symlinks() {
    let vfs = MockVfs::builder()
        .dir("/data")
        .file("/data/file.txt", b"real content")
        .symlink("/data/link", "file.txt")
        .symlink("/data/loop", "/data")
        .build();

    let issues = std::cell::Cell::new(0u32);
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data")],
            destination: vfs_path("/out.tar"),
            options: ArchiveOptions {
                preserve_symlinks: false,
                ..archive_options(ArchiveFormat::Tar)
            },
        },
        |_issue| {
            issues.set(issues.get() + 1);
            IssueResponse {
                action: IssueAction::Skip,
                apply_to_all: false,
            }
        },
    )
    .await;
    assert!(has_completed(&result.events), "{:?}", result.events);
    // The /data → /data cycle raised exactly one skip issue.
    assert_eq!(issues.get(), 1);

    let entries = read_back_tar(&result.vfs.read_content("/out.tar"), "check.tar").await;
    // The followed link is stored as a regular file with the target's bytes.
    assert!(!entries["data/link"].is_symlink);
    assert_eq!(
        entries["data/link"].content.as_deref(),
        Some(&b"real content"[..])
    );
    assert!(!entries.contains_key("data/loop"));
}

#[tokio::test]
async fn test_create_archive_open_failure_skips_entry() {
    let vfs = MockVfs::builder()
        .dir("/data")
        .file("/data/good.txt", b"good")
        .file("/data/bad.txt", b"bad")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/data/bad.txt"),
            operation: "open_read_async",
            error: crate::Error {
                kind: crate::ErrorKind::PermissionDenied,
                message: "denied".to_string(),
            },
            remaining: None,
        })
        .build();
    let result = run_operation(
        vfs,
        OperationRequest::CreateArchive {
            sources: vec![vfs_path("/data")],
            destination: vfs_path("/out.tar"),
            options: archive_options(ArchiveFormat::Tar),
        },
        skip_all,
    )
    .await;
    assert!(has_completed(&result.events), "{:?}", result.events);

    let entries = read_back_tar(&result.vfs.read_content("/out.tar"), "check.tar").await;
    // The unreadable file was skipped before its header hit the stream.
    assert!(!entries.contains_key("data/bad.txt"));
    assert_eq!(
        entries["data/good.txt"].content.as_deref(),
        Some(&b"good"[..])
    );
}

// ---------------------------------------------------------------------------
// Self-destination and re-spelling
// ---------------------------------------------------------------------------

fn failure_message(events: &[OperationProgress]) -> Option<&str> {
    events.iter().find_map(|e| match e {
        OperationProgress::Failed { error, .. } => Some(error.as_str()),
        _ => None,
    })
}

fn raised_an_issue(events: &[OperationProgress]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, OperationProgress::Issue { .. }))
}

#[tokio::test]
async fn test_copy_onto_itself_fails_the_operation() {
    let vfs = MockVfs::builder().file("/src/a.txt", b"hello").build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/src"),
            options: Default::default(),
        },
        // Any conflict prompt here would be a bug: overwriting would open
        // the file truncating while still reading it.
        overwrite_all,
    )
    .await;

    assert!(!raised_an_issue(&result.events));
    assert!(failure_message(&result.events).is_some_and(|m| m.contains("onto itself")));
    assert_eq!(result.vfs.read_content("/src/a.txt"), b"hello");
}

#[tokio::test]
async fn test_copy_onto_itself_fails_across_case_on_insensitive_vfs() {
    let vfs = MockVfs::builder()
        .case_insensitive()
        .file("/src/Foo.txt", b"hello")
        .build();

    // Different spelling, same file: `cp Foo.txt foo.txt` is refused.
    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: Some("foo.txt".into()),
            sources: vec![vfs_path("/src/Foo.txt")],
            destination: vfs_path("/src"),
            options: Default::default(),
        },
        overwrite_all,
    )
    .await;

    assert!(failure_message(&result.events).is_some_and(|m| m.contains("onto itself")));
    assert_eq!(result.vfs.read_content("/src/Foo.txt"), b"hello");
}

#[tokio::test]
async fn test_copy_to_a_different_name_in_the_same_directory_is_allowed() {
    let vfs = MockVfs::builder().file("/src/a.txt", b"hello").build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            rename_to: Some("b.txt".into()),
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/src"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/src/b.txt"), b"hello");
}

#[tokio::test]
async fn test_move_onto_itself_fails_the_operation() {
    let vfs = MockVfs::builder().file("/src/a.txt", b"hello").build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/src"),
            options: Default::default(),
        },
        overwrite_all,
    )
    .await;

    assert!(!raised_an_issue(&result.events));
    assert!(failure_message(&result.events).is_some_and(|m| m.contains("onto itself")));
    assert_eq!(result.vfs.read_content("/src/a.txt"), b"hello");
}

#[tokio::test]
async fn test_move_across_case_in_place_is_a_respelling_not_a_conflict() {
    let vfs = MockVfs::builder()
        .case_insensitive()
        .file("/src/Foo.txt", b"hello")
        .build();

    // `mv Foo.txt foo.txt` on a case-insensitive volume: the destination
    // resolves to the source itself, which is the point of the move rather
    // than an obstacle to it.
    let result = run_operation(
        vfs,
        OperationRequest::Move {
            rename_to: Some("foo.txt".into()),
            sources: vec![vfs_path("/src/Foo.txt")],
            destination: vfs_path("/src"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(!raised_an_issue(&result.events));
    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/src/foo.txt"), b"hello");
}

#[tokio::test]
async fn test_rename_across_case_only_is_not_a_conflict() {
    let vfs = MockVfs::builder()
        .case_insensitive()
        .file("/dir/Foo.txt", b"hello")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/Foo.txt"),
            new_name: "foo.txt".into(),
        },
        // The old behaviour raised "File already exists" here, because the
        // destination stats successfully — as the source.
        skip_all,
    )
    .await;

    assert!(!raised_an_issue(&result.events));
    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dir/foo.txt"), b"hello");
    assert_eq!(
        result
            .vfs
            .snapshot()
            .iter()
            .filter(|(_, k)| *k == "file")
            .count(),
        1,
        "the rename must not have left a second entry behind"
    );
}

#[tokio::test]
async fn test_rename_onto_a_genuinely_different_file_still_conflicts() {
    let vfs = MockVfs::builder()
        .case_insensitive()
        .file("/dir/a.txt", b"first")
        .file("/dir/b.txt", b"second")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Rename {
            source: vfs_path("/dir/a.txt"),
            new_name: "b.txt".into(),
        },
        skip_all,
    )
    .await;

    assert!(raised_an_issue(&result.events));
    assert_eq!(result.vfs.read_content("/dir/b.txt"), b"second");
}

// ---------------------------------------------------------------------------
// Symlinked directories are never walked into
// ---------------------------------------------------------------------------

/// Real `LocalVfs` against a temp directory — the mock has no notion of a
/// symlink whose target is a directory, which is precisely the shape that
/// went wrong.
#[cfg(unix)]
mod local_symlink {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use parking_lot::Mutex;
    use tokio_util::sync::CancellationToken;

    use crate::operation::*;
    use crate::vfs::local::LocalVfs;
    use crate::vfs::{VfsId, VfsPath, VfsRegistry};

    async fn run(request: OperationRequest) -> Vec<OperationProgress> {
        run_answering(request, IssueAction::Skip).await
    }

    /// Runs `request`, answering every issue with `action` for all.
    async fn run_answering(
        request: OperationRequest,
        action: IssueAction,
    ) -> Vec<OperationProgress> {
        let registry = Arc::new(VfsRegistry::with_root(Arc::new(LocalVfs::new())));
        let issue_resolvers: IssueResolvers =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::unbounded_channel::<OperationProgress>();
        let context = Arc::new(OperationContext {
            registry,
            shell_integration: None,
            spooler: crate::spool::Spooler::new(Default::default()),
        });

        let handle = tokio::spawn(execute_operation(
            1,
            request,
            progress_tx,
            CancellationToken::new(),
            issue_resolvers.clone(),
            Arc::new(AtomicU64::new(1)),
            context,
        ));

        let mut events = Vec::new();
        while let Some(event) = progress_rx.recv().await {
            // Answer any issue, or the operation blocks on its resolver.
            if let OperationProgress::Issue { issue, .. } = &event
                && let Some(sender) = issue_resolvers.lock().remove(&issue.issue_id)
            {
                let _ = sender.send(IssueResponse {
                    action,
                    apply_to_all: true,
                });
            }
            let terminal = matches!(
                &event,
                OperationProgress::Completed { .. }
                    | OperationProgress::Failed { .. }
                    | OperationProgress::Cancelled { .. }
            );
            events.push(event);
            if terminal {
                break;
            }
        }
        let _ = handle.await;
        events
    }

    fn vfs_path(path: &std::path::Path) -> VfsPath {
        VfsPath::new(VfsId::ROOT, PathBuf::from_native(path))
    }

    fn copy(source: &std::path::Path, destination: &std::path::Path) -> OperationRequest {
        OperationRequest::Copy {
            rename_to: None,
            sources: vec![vfs_path(source)],
            destination: vfs_path(destination),
            options: Default::default(),
        }
    }

    fn move_to(source: &std::path::Path, destination: &std::path::Path) -> OperationRequest {
        OperationRequest::Move {
            rename_to: None,
            sources: vec![vfs_path(source)],
            destination: vfs_path(destination),
            options: Default::default(),
        }
    }

    fn link_target(path: &std::path::Path) -> Option<std::path::PathBuf> {
        std::fs::symlink_metadata(path)
            .ok()
            .filter(|m| m.file_type().is_symlink())
            .and_then(|_| std::fs::read_link(path).ok())
    }

    /// `src/` and `dst/` in a temp directory, with a directory `real`
    /// holding a file, and `src/link` pointing at `new-target`.
    struct Tree {
        tmp: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            for dir in ["src", "dst", "real"] {
                std::fs::create_dir(tmp.path().join(dir)).expect("create_dir");
            }
            std::fs::write(tmp.path().join("real/keep.txt"), b"precious").expect("write");
            std::os::unix::fs::symlink("new-target", tmp.path().join("src/link")).expect("symlink");
            Self { tmp }
        }

        fn path(&self, rel: &str) -> std::path::PathBuf {
            self.tmp.path().join(rel)
        }

        fn link(&self, rel: &str, target: &str) {
            std::os::unix::fs::symlink(target, self.path(rel)).expect("symlink");
        }

        fn real_is_intact(&self) -> bool {
            std::fs::read(self.path("real/keep.txt")).ok().as_deref() == Some(b"precious")
        }
    }

    fn issues(events: &[OperationProgress]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, OperationProgress::Issue { .. }))
            .count()
    }

    fn completed(events: &[OperationProgress]) -> bool {
        events
            .iter()
            .any(|e| matches!(e, OperationProgress::Completed { .. }))
    }

    /// Overwriting would truncate the source before reading it.
    #[tokio::test]
    async fn a_copy_onto_its_own_hard_link_is_already_done() {
        let tree = Tree::new();
        std::fs::write(tree.path("src/x"), b"payload").unwrap();
        std::fs::hard_link(tree.path("src/x"), tree.path("dst/x")).unwrap();

        let events = run_answering(
            copy(&tree.path("src/x"), &tree.path("dst")),
            IssueAction::Overwrite,
        )
        .await;

        assert!(completed(&events));
        assert_eq!(issues(&events), 0);
        assert_eq!(std::fs::read(tree.path("src/x")).unwrap(), b"payload");
        assert_eq!(std::fs::read(tree.path("dst/x")).unwrap(), b"payload");
    }

    #[tokio::test]
    async fn a_copy_over_a_hard_linked_snapshot_asks_nothing_about_unchanged_files() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.path("src/tree")).unwrap();
        std::fs::create_dir_all(tree.path("dst/tree")).unwrap();
        for name in ["a", "b"] {
            std::fs::write(tree.path(&format!("src/tree/{name}")), name).unwrap();
            std::fs::hard_link(
                tree.path(&format!("src/tree/{name}")),
                tree.path(&format!("dst/tree/{name}")),
            )
            .unwrap();
        }
        std::fs::write(tree.path("src/tree/c"), b"c").unwrap();

        let events = run(copy(&tree.path("src/tree"), &tree.path("dst"))).await;

        assert!(completed(&events));
        assert_eq!(issues(&events), 0);
        for name in ["a", "b", "c"] {
            assert_eq!(
                std::fs::read(tree.path(&format!("dst/tree/{name}"))).unwrap(),
                name.as_bytes()
            );
        }
        assert_eq!(std::fs::read(tree.path("src/tree/a")).unwrap(), b"a");
    }

    #[tokio::test]
    async fn a_copy_into_a_link_to_its_own_directory_is_already_done() {
        let tree = Tree::new();
        std::fs::write(tree.path("src/x"), b"payload").unwrap();
        tree.link("dst/alias", "../src");

        let events = run_answering(
            copy(&tree.path("src/x"), &tree.path("dst/alias")),
            IssueAction::Overwrite,
        )
        .await;

        assert!(completed(&events));
        assert_eq!(issues(&events), 0);
        assert_eq!(std::fs::read(tree.path("src/x")).unwrap(), b"payload");
    }

    #[tokio::test]
    async fn a_move_onto_its_own_hard_link_leaves_both() {
        let tree = Tree::new();
        std::fs::write(tree.path("src/x"), b"payload").unwrap();
        std::fs::hard_link(tree.path("src/x"), tree.path("dst/x")).unwrap();

        let events = run_answering(
            move_to(&tree.path("src/x"), &tree.path("dst")),
            IssueAction::Overwrite,
        )
        .await;

        assert!(completed(&events));
        assert_eq!(std::fs::read(tree.path("src/x")).unwrap(), b"payload");
        assert_eq!(std::fs::read(tree.path("dst/x")).unwrap(), b"payload");
    }

    #[tokio::test]
    async fn a_link_copied_over_a_link_replaces_the_link_whatever_it_points_at() {
        for existing in ["real/keep.txt", "nowhere", "../real"] {
            let tree = Tree::new();
            tree.link("dst/link", existing);

            run_answering(
                copy(&tree.path("src/link"), &tree.path("dst")),
                IssueAction::Overwrite,
            )
            .await;

            assert_eq!(
                link_target(&tree.path("dst/link")),
                Some("new-target".into()),
                "over a link to {existing}"
            );
            assert!(tree.real_is_intact(), "over a link to {existing}");
            assert!(!tree.path("dst/nowhere").exists());
        }
    }

    #[tokio::test]
    async fn a_file_copied_over_a_link_to_a_directory_replaces_the_link() {
        let tree = Tree::new();
        std::fs::write(tree.path("src/f.txt"), b"new").expect("write");
        tree.link("dst/f.txt", "../real");

        run_answering(
            copy(&tree.path("src/f.txt"), &tree.path("dst")),
            IssueAction::Overwrite,
        )
        .await;

        assert_eq!(link_target(&tree.path("dst/f.txt")), None);
        assert_eq!(std::fs::read(tree.path("dst/f.txt")).unwrap(), b"new");
        assert!(tree.real_is_intact());
    }

    #[tokio::test]
    async fn a_directory_copied_onto_a_link_to_a_directory_merges_through_it() {
        let tree = Tree::new();
        std::fs::create_dir(tree.path("src/d")).expect("create_dir");
        std::fs::write(tree.path("src/d/x.txt"), b"x").expect("write");
        tree.link("dst/d", "../real");

        let events = run(copy(&tree.path("src/d"), &tree.path("dst"))).await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, OperationProgress::Completed { .. }))
        );
        assert_eq!(link_target(&tree.path("dst/d")), Some("../real".into()));
        assert_eq!(std::fs::read(tree.path("real/x.txt")).unwrap(), b"x");
        assert!(tree.real_is_intact());
    }

    #[tokio::test]
    async fn a_link_moved_over_a_link_to_a_directory_replaces_the_link() {
        let tree = Tree::new();
        tree.link("dst/link", "../real");

        run_answering(
            move_to(&tree.path("src/link"), &tree.path("dst")),
            IssueAction::Overwrite,
        )
        .await;

        assert_eq!(
            link_target(&tree.path("dst/link")),
            Some("new-target".into())
        );
        assert!(std::fs::symlink_metadata(tree.path("src/link")).is_err());
        assert!(tree.real_is_intact());
    }

    #[tokio::test]
    async fn a_file_moved_over_a_link_to_a_directory_replaces_the_link() {
        let tree = Tree::new();
        std::fs::write(tree.path("src/f.txt"), b"new").expect("write");
        tree.link("dst/f.txt", "../real");

        run_answering(
            move_to(&tree.path("src/f.txt"), &tree.path("dst")),
            IssueAction::Overwrite,
        )
        .await;

        assert_eq!(link_target(&tree.path("dst/f.txt")), None);
        assert_eq!(std::fs::read(tree.path("dst/f.txt")).unwrap(), b"new");
        assert!(!tree.path("src/f.txt").exists());
        assert!(tree.real_is_intact());
    }

    #[tokio::test]
    async fn a_directory_moved_onto_a_link_to_a_directory_merges_through_it() {
        let tree = Tree::new();
        std::fs::create_dir(tree.path("src/d")).expect("create_dir");
        std::fs::write(tree.path("src/d/x.txt"), b"x").expect("write");
        tree.link("dst/d", "../real");

        run(move_to(&tree.path("src/d"), &tree.path("dst"))).await;

        assert_eq!(link_target(&tree.path("dst/d")), Some("../real".into()));
        assert_eq!(std::fs::read(tree.path("real/x.txt")).unwrap(), b"x");
        assert!(!tree.path("src/d").exists());
        assert!(tree.real_is_intact());
    }

    /// Deleting a symlink must remove the link, never the tree it points at.
    #[tokio::test]
    async fn delete_does_not_recurse_into_a_symlinked_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).expect("create_dir");
        std::fs::write(target.join("keep.txt"), b"precious").expect("write");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        run(OperationRequest::Delete {
            paths: vec![vfs_path(&link)],
            to_trash: false,
            cross_mount_points: false,
        })
        .await;

        assert!(
            target.join("keep.txt").exists(),
            "delete followed the symlink and destroyed the target's contents"
        );
        assert!(target.exists(), "the target directory itself was removed");
        assert!(
            !link.exists() && std::fs::symlink_metadata(&link).is_err(),
            "the symlink itself should be gone"
        );
    }

    /// Same trap, same fix: a recursive chmod must stop at the link.
    #[tokio::test]
    async fn recursive_set_metadata_does_not_recurse_into_a_symlinked_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).expect("create_dir");
        let inner = target.join("inner.txt");
        std::fs::write(&inner, b"x").expect("write");
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        run(OperationRequest::SetMetadata {
            paths: vec![vfs_path(&link)],
            mode_set: 0o777,
            mode_clear: 0,
            uid: None,
            gid: None,
            recursive: true,
            cross_mount_points: false,
        })
        .await;

        let mode = std::fs::metadata(&inner)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o644,
            "recursive chmod reached through the symlink into the target"
        );
    }
}

// ---------------------------------------------------------------------------
// Mount points
// ---------------------------------------------------------------------------

/// `/top` with a file, a subdirectory, and `/top/mnt` on another
/// filesystem holding a file of its own.
fn tree_with_mount() -> crate::test_support::MockVfsBuilder {
    MockVfs::builder()
        .dir("/top")
        .file("/top/a.txt", b"a")
        .dir("/top/sub")
        .file("/top/sub/c.txt", b"c")
        .mount_point("/top/mnt")
        .file("/top/mnt/b.txt", b"b")
        .dir("/dst")
}

fn delete_request(path: &str, cross_mount_points: bool) -> OperationRequest {
    OperationRequest::Delete {
        paths: vec![vfs_path(path)],
        to_trash: false,
        cross_mount_points,
    }
}

#[tokio::test]
async fn test_delete_stays_out_of_mount_points_and_keeps_the_path_to_them() {
    let vfs = tree_with_mount()
        .config(MockVfsConfig {
            can_remove_tree: false,
            ..Default::default()
        })
        .build();
    let issues = std::cell::RefCell::new(Vec::new());
    let result = run_operation(vfs, delete_request("/top", false), |issue| {
        issues
            .borrow_mut()
            .push((issue.kind.clone(), issue.actions.clone()));
        skip_all(issue)
    })
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(
        issues.into_inner(),
        vec![(
            IssueKind::Other("MountPoint".into()),
            vec![IssueAction::Skip]
        )]
    );
    assert!(!result.vfs.exists("/top/a.txt"));
    assert!(!result.vfs.exists("/top/sub"));
    assert!(result.vfs.exists("/top/mnt/b.txt"));
    assert!(
        result.vfs.exists("/top"),
        "the mount point's parent survives"
    );
}

#[tokio::test]
async fn test_delete_crosses_mount_points_on_request_but_never_removes_them() {
    let vfs = tree_with_mount()
        .config(MockVfsConfig {
            can_remove_tree: false,
            ..Default::default()
        })
        .build();
    let result = run_operation(vfs, delete_request("/top", true), |issue| {
        panic!("unexpected issue: {}", issue.message)
    })
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/top/a.txt"));
    assert!(!result.vfs.exists("/top/mnt/b.txt"));
    assert!(result.vfs.exists("/top/mnt"));
    assert!(result.vfs.exists("/top"));
}

#[tokio::test]
async fn test_delete_of_a_mount_point_itself_empties_it() {
    let vfs = tree_with_mount()
        .config(MockVfsConfig {
            can_remove_tree: false,
            ..Default::default()
        })
        .build();
    let result = run_operation(vfs, delete_request("/top/mnt", false), |issue| {
        panic!("unexpected issue: {}", issue.message)
    })
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/top/mnt/b.txt"));
    assert!(result.vfs.exists("/top/mnt"));
}

#[tokio::test]
async fn test_copy_crosses_mount_points_unless_told_to_stay() {
    for one_file_system in [false, true] {
        let vfs = tree_with_mount().build();
        let result = run_operation(
            vfs,
            OperationRequest::Copy {
                sources: vec![vfs_path("/top")],
                destination: vfs_path("/dst"),
                options: CopyOptions {
                    one_file_system,
                    ..Default::default()
                },
                rename_to: None,
            },
            |issue| panic!("unexpected issue: {}", issue.message),
        )
        .await;

        assert!(has_completed(&result.events));
        assert!(result.vfs.exists("/dst/top/sub/c.txt"));
        assert!(result.vfs.exists("/dst/top/mnt"));
        assert_eq!(result.vfs.exists("/dst/top/mnt/b.txt"), !one_file_system);
    }
}

#[tokio::test]
async fn test_move_leaves_mount_points_standing() {
    let vfs = tree_with_mount()
        .config(MockVfsConfig {
            can_rename: false,
            ..Default::default()
        })
        .build();
    let result = run_operation(
        vfs,
        OperationRequest::Move {
            sources: vec![vfs_path("/top")],
            destination: vfs_path("/dst"),
            options: Default::default(),
            rename_to: None,
        },
        |issue| panic!("unexpected issue: {}", issue.message),
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(result.vfs.exists("/dst/top/mnt/b.txt"));
    assert!(!result.vfs.exists("/top/mnt/b.txt"));
    assert!(!result.vfs.exists("/top/sub"));
    assert!(result.vfs.exists("/top/mnt"));
    assert!(result.vfs.exists("/top"));
}

#[tokio::test]
async fn test_recursive_metadata_stays_out_of_mount_points_unless_told_to_cross() {
    for cross_mount_points in [false, true] {
        let vfs = tree_with_mount().build();
        let issues = std::cell::RefCell::new(Vec::new());
        let result = run_operation(
            vfs,
            OperationRequest::SetMetadata {
                paths: vec![vfs_path("/top")],
                mode_set: 0o007,
                mode_clear: 0,
                uid: None,
                gid: None,
                recursive: true,
                cross_mount_points,
            },
            |issue| {
                issues.borrow_mut().push(issue.kind.clone());
                skip_all(issue)
            },
        )
        .await;

        assert!(has_completed(&result.events));
        assert_eq!(issues.into_inner().len(), usize::from(!cross_mount_points));
        assert_eq!(result.vfs.get_mode("/top/a.txt"), Some(0o647));
        let expected = if cross_mount_points { 0o647 } else { 0o644 };
        assert_eq!(result.vfs.get_mode("/top/mnt/b.txt"), Some(expected));
        let expected = if cross_mount_points { 0o757 } else { 0o755 };
        assert_eq!(result.vfs.get_mode("/top/mnt"), Some(expected));
    }
}

#[path = "preserve_tests.rs"]
mod preserve_tests;
