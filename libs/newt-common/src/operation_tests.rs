use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::operation::*;
use crate::test_support::{MockVfs, MockVfsConfig};
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
    let context = Arc::new(OperationContext { registry });

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
    VfsPath::root(path)
}

fn vfs_path_id(vfs_id: u32, path: &str) -> VfsPath {
    VfsPath::new(VfsId(vfs_id), path)
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
            path: PathBuf::from("/a.txt"),
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
            path: PathBuf::from("/a.txt"),
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
        },
        retry_then_skip(&retry_count),
    )
    .await;

    assert!(has_completed(&result.events));
    assert!(!result.vfs.exists("/a.txt"));
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
// Copy tests
// ===========================================================================

#[tokio::test]
async fn test_copy_single_file_sync() {
    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello world")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
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
async fn test_copy_single_file_async() {
    let vfs = MockVfs::builder()
        .config(MockVfsConfig {
            can_read_sync: false,
            can_read_async: true,
            can_overwrite_sync: false,
            can_overwrite_async: true,
            ..Default::default()
        })
        .file("/src/a.txt", b"async content")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
            sources: vec![vfs_path("/src/a.txt")],
            destination: vfs_path("/dst"),
            options: Default::default(),
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    assert_eq!(result.vfs.read_content("/dst/a.txt"), b"async content");
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
async fn test_copy_with_symlinks() {
    let vfs = MockVfs::builder()
        .dir("/src")
        .symlink("/src/link", "/somewhere/target")
        .dir("/dst")
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Copy {
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
        .find(|(p, _)| p == &PathBuf::from("/dst/link"));
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
        },
        skip_all,
    )
    .await;

    assert!(has_completed(&result.events));
    let snapshot = result.vfs.snapshot();
    let link_entry = snapshot
        .iter()
        .find(|(p, _)| p == &PathBuf::from("/dst/a.txt"));
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
async fn test_move_cross_vfs() {
    let src_vfs = MockVfs::builder().file("/data/a.txt", b"cross-vfs").build();

    let dst_vfs = MockVfs::builder().dir("/target").build();

    let (result, dst) = run_operation_two_vfs(
        src_vfs,
        dst_vfs,
        OperationRequest::Move {
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

    let vfs = MockVfs::builder()
        .file("/src/a.txt", b"hello")
        .dir("/dst")
        .failure(FailureSpec {
            path: PathBuf::from("/src/a.txt"),
            operation: "rename",
            error: crate::Error::custom("cross-device link"),
            remaining: None,
        })
        .build();

    let result = run_operation(
        vfs,
        OperationRequest::Move {
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
        .find(|(p, _)| p == &PathBuf::from("/dst/link_to_dir"));
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
        .find(|(p, _)| p == &PathBuf::from("/dst/src/link_to_dir"));
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
            path: PathBuf::from("/a.txt"),
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
            path: PathBuf::from("/a.txt"),
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
