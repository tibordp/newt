use std::sync::Arc;

use tokio::sync::mpsc;

use super::git::{GitEnricher, dir_statuses, parse_porcelain_v2};
use super::*;
use crate::vfs::{LocalVfs, VfsId, VfsRegistry, local::local_path_from_native};

// ---------------------------------------------------------------------------
// Porcelain v2 parsing
// ---------------------------------------------------------------------------

fn z(records: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in records {
        out.extend_from_slice(r.as_bytes());
        out.push(0);
    }
    out
}

#[test]
fn parse_porcelain_v2_full() {
    let bytes = z(&[
        "# branch.oid 1234567890abcdef",
        "# branch.head main",
        "# branch.ab +2 -1",
        "1 .M N... 100644 100644 100644 h1 h2 modified.txt",
        "1 A. N... 000000 100644 100644 0000000 h2 staged file.txt",
        "2 R. N... 100644 100644 100644 h1 h2 R100 new name.txt",
        "old.txt", // rename origin — must be skipped, not parsed as a record
        "u UU N... 100644 100644 100644 100644 h1 h2 h3 conflict.txt",
        "? untracked.txt",
        "? newdir/",
        "! ignored.txt",
        "! node_modules/",
    ]);
    let status = parse_porcelain_v2(&bytes);
    assert_eq!(status.branch_head.as_deref(), Some("main"));
    assert_eq!(status.branch_oid.as_deref(), Some("1234567890abcdef"));
    assert_eq!((status.ahead, status.behind), (2, 1));
    assert_eq!(
        status.entries,
        vec![
            ("modified.txt".to_string(), GitEntryStatus::Modified),
            ("staged file.txt".to_string(), GitEntryStatus::Added),
            ("new name.txt".to_string(), GitEntryStatus::Renamed),
            ("conflict.txt".to_string(), GitEntryStatus::Conflicted),
            ("untracked.txt".to_string(), GitEntryStatus::Untracked),
            ("newdir/".to_string(), GitEntryStatus::Untracked),
            ("ignored.txt".to_string(), GitEntryStatus::Ignored),
            ("node_modules/".to_string(), GitEntryStatus::Ignored),
        ]
    );
}

#[test]
fn parse_porcelain_v2_detached() {
    let bytes = z(&["# branch.oid deadbeef00", "# branch.head (detached)"]);
    let status = parse_porcelain_v2(&bytes);
    assert_eq!(status.branch_head.as_deref(), Some("(detached)"));
    assert_eq!((status.ahead, status.behind), (0, 0));
}

// ---------------------------------------------------------------------------
// Directory rollups
// ---------------------------------------------------------------------------

#[test]
fn dir_statuses_root_prefix() {
    let entries = vec![
        ("modified.txt".to_string(), GitEntryStatus::Modified),
        ("newdir/".to_string(), GitEntryStatus::Untracked),
        ("src/deep/file.rs".to_string(), GitEntryStatus::Modified),
        ("src/other.rs".to_string(), GitEntryStatus::Untracked),
        ("vendor/ignored.o".to_string(), GitEntryStatus::Ignored),
        ("target/".to_string(), GitEntryStatus::Ignored),
    ];
    let map = dir_statuses(&entries, "");
    assert_eq!(map.get("modified.txt"), Some(&GitEntryStatus::Modified));
    assert_eq!(map.get("newdir"), Some(&GitEntryStatus::Untracked));
    // Rollup keeps the highest-precedence status.
    assert_eq!(map.get("src"), Some(&GitEntryStatus::Modified));
    // Ignored never rolls up…
    assert_eq!(map.get("vendor"), None);
    // …but a fully-ignored (collapsed) directory is a direct entry.
    assert_eq!(map.get("target"), Some(&GitEntryStatus::Ignored));
}

#[test]
fn dir_statuses_subdir_prefix() {
    let entries = vec![
        ("sub/x.txt".to_string(), GitEntryStatus::Modified),
        ("sub/inner/y.txt".to_string(), GitEntryStatus::Conflicted),
        ("other/z.txt".to_string(), GitEntryStatus::Modified),
        // Shares the string prefix but is a different directory.
        ("subx/w.txt".to_string(), GitEntryStatus::Modified),
    ];
    let map = dir_statuses(&entries, "sub");
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("x.txt"), Some(&GitEntryStatus::Modified));
    assert_eq!(map.get("inner"), Some(&GitEntryStatus::Conflicted));
}

// ---------------------------------------------------------------------------
// Sink semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sink_reset_and_empty_finish() {
    let (tx, mut rx) = mpsc::channel(16);
    let sink = EnrichSink::new("test", tx);
    // finish on a sink that never emitted still sends one reset batch,
    // so a rerun clears the previous generation.
    sink.finish().await;
    let ev = rx.try_recv().unwrap();
    match ev {
        EnrichmentEvent::Batch(b) => {
            assert!(b.reset);
            assert!(b.entries.is_empty() && b.badges.is_empty());
        }
        other => panic!("unexpected event: {:?}", other),
    }

    // A second finish with nothing new pending sends nothing.
    sink.finish().await;
    assert!(rx.try_recv().is_err());

    // Later emits flush as non-reset batches.
    sink.emit_entry("a".into(), Annotation::Git(GitEntryStatus::Modified));
    sink.finish().await;
    match rx.try_recv().unwrap() {
        EnrichmentEvent::Batch(b) => assert!(!b.reset),
        other => panic!("unexpected event: {:?}", other),
    }
}

#[tokio::test]
async fn sink_badge_replaces_same_kind() {
    let (tx, mut rx) = mpsc::channel(16);
    let sink = EnrichSink::new("test", tx);
    let badge = |name: &str| ContextBadge::GitBranch {
        name: name.into(),
        detached: false,
        ahead: 0,
        behind: 0,
        dirty: false,
    };
    sink.emit_badge(badge("one"));
    sink.emit_badge(badge("two"));
    sink.finish().await;
    match rx.try_recv().unwrap() {
        EnrichmentEvent::Batch(b) => assert_eq!(b.badges, vec![badge("two")]),
        other => panic!("unexpected event: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Wire compatibility
// ---------------------------------------------------------------------------

/// Everything in an `EnrichmentEvent` crosses the agent↔host bincode
/// boundary. Bincode cannot deserialize serde's `tag`/`content`/
/// `untagged` enum representations (it dies on
/// `deserialize_identifier`), so the wire types must stick to default
/// external tagging — this round-trips the full event surface to catch
/// a representation attribute sneaking in.
#[test]
fn enrichment_event_bincode_round_trip() {
    let events = vec![
        EnrichmentEvent::Started {
            enricher: "git".into(),
            activity: "git status".into(),
        },
        EnrichmentEvent::Batch(EnrichmentBatch {
            enricher: "git".into(),
            reset: true,
            entries: vec![("a.txt".into(), Annotation::Git(GitEntryStatus::Modified))],
            badges: vec![ContextBadge::GitBranch {
                name: "main".into(),
                detached: false,
                ahead: 1,
                behind: 2,
                dirty: true,
            }],
        }),
        EnrichmentEvent::Finished {
            enricher: "git".into(),
        },
    ];
    for event in events {
        let bytes = bincode::serialize(&(EnrichmentId(7), &event)).unwrap();
        let (id, decoded): (EnrichmentId, EnrichmentEvent) = bincode::deserialize(&bytes).unwrap();
        assert_eq!(id, EnrichmentId(7));
        assert_eq!(format!("{:?}", decoded), format!("{:?}", event));
    }

    let request = (
        EnrichmentId(1),
        VfsPath::root(VfsId::ROOT),
        EnrichScope::Entries(vec!["x".into()]),
        vec!["git".to_string()],
    );
    let bytes = bincode::serialize(&request).unwrap();
    let _: (EnrichmentId, VfsPath, EnrichScope, Vec<String>) =
        bincode::deserialize(&bytes).unwrap();
}

// ---------------------------------------------------------------------------
// End-to-end against a real git repo
// ---------------------------------------------------------------------------

async fn git(dir: &std::path::Path, args: &[&str]) {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .await
        .expect("failed to run git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

async fn collect_events(
    enrichers: &Enrichers,
    path: VfsPath,
) -> (Vec<(String, Annotation)>, Vec<ContextBadge>, Vec<String>) {
    let (tx, mut rx) = mpsc::channel(64);
    enrichers
        .enrich(path, EnrichScope::AllEntries, vec!["git".to_string()], tx)
        .await
        .unwrap();
    let mut entries = Vec::new();
    let mut badges = Vec::new();
    let mut lifecycle = Vec::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            EnrichmentEvent::Started { enricher, .. } => {
                lifecycle.push(format!("start:{enricher}"))
            }
            EnrichmentEvent::Finished { enricher } => lifecycle.push(format!("finish:{enricher}")),
            EnrichmentEvent::Batch(b) => {
                entries.extend(b.entries);
                badges.extend(b.badges);
            }
        }
    }
    (entries, badges, lifecycle)
}

#[tokio::test]
async fn git_enricher_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    // Resolve symlinks (macOS /var → /private/var) so repo-relative
    // prefix computation sees the same root git reports.
    let dir = tmp.path().canonicalize().unwrap();

    git(&dir, &["init", "-b", "main"]).await;
    std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(dir.join("committed.txt"), "one").unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/inner.txt"), "one").unwrap();
    git(&dir, &["add", "."]).await;
    git(&dir, &["commit", "-m", "init"]).await;

    std::fs::write(dir.join("committed.txt"), "two").unwrap(); // Modified
    std::fs::write(dir.join("sub/inner.txt"), "two").unwrap(); // rolls up to sub
    std::fs::write(dir.join("untracked.txt"), "x").unwrap(); // Untracked
    std::fs::write(dir.join("ignored.txt"), "x").unwrap(); // Ignored
    std::fs::create_dir(dir.join("newdir")).unwrap();
    std::fs::write(dir.join("newdir/a.txt"), "x").unwrap(); // collapsed Untracked
    std::fs::write(dir.join("staged.txt"), "x").unwrap();
    git(&dir, &["add", "staged.txt"]).await; // Added

    let registry = Arc::new(VfsRegistry::with_root(Arc::new(LocalVfs::new())));
    let enrichers = Enrichers::new(registry.clone()).with(Arc::new(GitEnricher::new(Vec::new())));

    let root_path = VfsPath::new(VfsId::ROOT, local_path_from_native(&dir));
    let (entries, badges, lifecycle) = collect_events(&enrichers, root_path).await;

    let get = |name: &str| {
        entries.iter().find_map(|(k, a)| {
            (k == name).then(|| match a {
                Annotation::Git(s) => *s,
            })
        })
    };
    assert_eq!(get("committed.txt"), Some(GitEntryStatus::Modified));
    assert_eq!(get("sub"), Some(GitEntryStatus::Modified));
    assert_eq!(get("untracked.txt"), Some(GitEntryStatus::Untracked));
    assert_eq!(get("ignored.txt"), Some(GitEntryStatus::Ignored));
    assert_eq!(get("newdir"), Some(GitEntryStatus::Untracked));
    assert_eq!(get("staged.txt"), Some(GitEntryStatus::Added));

    assert_eq!(
        badges,
        vec![ContextBadge::GitBranch {
            name: "main".into(),
            detached: false,
            ahead: 0,
            behind: 0,
            dirty: true,
        }]
    );
    assert_eq!(lifecycle, vec!["start:git", "finish:git"]);

    // Listing a subdirectory annotates its own entries.
    let sub_path = VfsPath::new(VfsId::ROOT, local_path_from_native(&dir.join("sub")));
    let (entries, _, _) = collect_events(&enrichers, sub_path).await;
    assert_eq!(
        entries
            .iter()
            .find(|(k, _)| k == "inner.txt")
            .map(|(_, a)| a),
        Some(&Annotation::Git(GitEntryStatus::Modified))
    );

    // A non-repo directory: git still runs (the VFS gate is the
    // requesting side's, the repo gate is internal) but produces
    // nothing — the run's empty reset batch clears prior generations.
    let outside = tempfile::tempdir().unwrap();
    let outside_path = VfsPath::new(
        VfsId::ROOT,
        local_path_from_native(&outside.path().canonicalize().unwrap()),
    );
    let (entries, badges, lifecycle) = collect_events(&enrichers, outside_path).await;
    assert!(entries.is_empty() && badges.is_empty());
    assert_eq!(lifecycle, vec!["start:git", "finish:git"]);

    // Unknown enricher ids are skipped without events.
    let (tx, mut rx) = mpsc::channel(16);
    enrichers
        .enrich(
            VfsPath::root(VfsId::ROOT),
            EnrichScope::AllEntries,
            vec!["nonsense".to_string()],
            tx,
        )
        .await
        .unwrap();
    assert!(rx.recv().await.is_none());
}

#[test]
fn git_descriptor_registered() {
    // The host's request-building side discovers enrichers through the
    // inventory-collected descriptor list; git must be on it, automatic,
    // and gated to the local VFS type.
    let descriptor = all_enricher_descriptors()
        .find(|d| d.id() == "git")
        .expect("git enricher descriptor not registered");
    assert!(descriptor.automatic());
    assert!(descriptor.applies_to_vfs(&crate::vfs::LOCAL_VFS_DESCRIPTOR));
    assert!(!descriptor.applies_to_vfs(&crate::vfs::SEARCH_VFS_DESCRIPTOR));
}
