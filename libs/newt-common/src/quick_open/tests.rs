use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use super::index::Index;
use super::*;
use crate::rpc::Communicator;
use crate::test_support::MockVfs;
use crate::vfs::{VfsId, VfsPath};

fn ranked(index: &mut Index, query: &str) -> Vec<String> {
    index.set_query(query);
    index.catch_up();
    index.top(TOP).into_iter().map(|h| h.rel).collect()
}

fn index(rels: &[&str]) -> Index {
    let mut index = Index::new();
    for rel in rels {
        index.push(rel.to_string(), false);
    }
    index
}

#[test]
fn characters_match_in_order_anywhere() {
    let mut index = index(&["foo_bar.txt", "bar_foo.txt", "fbo.txt"]);
    assert_eq!(ranked(&mut index, "fobr"), ["foo_bar.txt"]);
    assert_eq!(ranked(&mut index, "FOOBAR"), ["foo_bar.txt"]);
    assert!(ranked(&mut index, "zz").is_empty());
}

#[test]
fn a_match_in_the_name_beats_one_that_needs_the_path() {
    let mut index = index(&["pane_helpers/x.ts", "src/main_window/Pane.tsx"]);
    assert_eq!(
        ranked(&mut index, "pane"),
        ["src/main_window/Pane.tsx", "pane_helpers/x.ts"]
    );
    assert_eq!(ranked(&mut index, "mw/pane"), ["src/main_window/Pane.tsx"]);
}

#[test]
fn every_piece_of_the_query_has_to_match() {
    let mut index = index(&["src/pane.tsx", "src/pane.rs", "lib/tsx.rs"]);
    assert_eq!(ranked(&mut index, "pane tsx"), ["src/pane.tsx"]);
}

#[test]
fn an_empty_query_lists_the_nearest_first() {
    let mut index = index(&["a/b/deep.txt", "a/near.txt", "top.txt"]);
    assert_eq!(
        ranked(&mut index, ""),
        ["top.txt", "a/near.txt", "a/b/deep.txt"]
    );
}

#[test]
fn narrowing_the_query_ranks_as_a_fresh_one_would() {
    let rels = ["src/foo.rs", "src/fob.rs", "foo/bar/baz.rs", "other.txt"];
    let mut incremental = index(&rels);
    ranked(&mut incremental, "fo");
    let narrowed = ranked(&mut incremental, "foo");
    let mut fresh = index(&rels);
    assert_eq!(narrowed, ranked(&mut fresh, "foo"));
    // Widening again brings back what narrowing dropped.
    assert_eq!(ranked(&mut incremental, "fo"), ranked(&mut fresh, "fo"));
}

#[test]
fn entries_added_later_are_ranked_too() {
    let mut index = index(&["a.txt"]);
    ranked(&mut index, "b");
    index.push("b.txt".into(), false);
    assert_eq!(ranked(&mut index, "b"), ["b.txt"]);
}

#[test]
fn highlights_index_characters_of_the_whole_path() {
    let mut index = index(&["dir/naïve.txt"]);
    index.set_query("nv");
    index.catch_up();
    // "dir/" is 4 characters; `n` and `v` of the name.
    assert_eq!(index.top(1)[0].highlights, [4, 7]);
}

async fn run(
    vfs: Arc<MockVfs>,
    root: &str,
    options: QuickOpenOptions,
    query: &str,
) -> QuickOpenUpdate {
    let registry = Arc::new(VfsRegistry::with_root(vfs));
    let quick_open = QuickOpen::new(registry);
    let (_query_tx, query_rx) = watch::channel(query.to_string());
    let (tx, mut rx) = mpsc::channel(16);
    let run = quick_open.run(
        VfsPath::new(VfsId::ROOT, PathBuf::from_wire_str(root)),
        options,
        query_rx,
        tx,
    );
    tokio::pin!(run);
    loop {
        tokio::select! {
            result = &mut run => panic!("the run ended: {result:?}"),
            update = rx.recv() => {
                let update = update.unwrap();
                if !update.walking {
                    return update;
                }
            }
        }
    }
}

fn options() -> QuickOpenOptions {
    QuickOpenOptions {
        limit: 1000,
        ..Default::default()
    }
}

fn rels(update: &QuickOpenUpdate) -> Vec<&str> {
    update.results.iter().map(|h| h.rel.as_str()).collect()
}

#[tokio::test]
async fn walks_below_the_root_nearest_first() {
    let vfs = MockVfs::builder()
        .file("/repo/a/b/c.txt", b"")
        .file("/repo/z.txt", b"")
        .build();
    let update = run(vfs, "/repo", options(), "").await;
    assert_eq!(rels(&update), ["a", "z.txt", "a/b", "a/b/c.txt"]);
    assert_eq!(update.walked, 4);
    assert!(!update.truncated);
}

#[tokio::test]
async fn hidden_entries_follow_the_option_and_git_is_never_walked() {
    let build = || {
        MockVfs::builder()
            .file("/r/.hidden/x.txt", b"")
            .file("/r/.git/config", b"")
            .file("/r/y.txt", b"")
            .build()
    };
    let update = run(build(), "/r", options(), "").await;
    assert_eq!(rels(&update), ["y.txt"]);
    let shown = QuickOpenOptions {
        show_hidden: true,
        ..options()
    };
    let update = run(build(), "/r", shown, "").await;
    assert_eq!(rels(&update), ["y.txt", ".hidden", ".hidden/x.txt"]);
}

#[tokio::test]
async fn gitignore_rules_apply_below_their_directory_and_from_above_the_root() {
    let build = || {
        MockVfs::builder()
            .dir("/repo/.git")
            .file("/repo/.git/info/exclude", b"secret.txt\n")
            .file("/repo/.gitignore", b"target/\n*.log\n")
            .file("/repo/src/.gitignore", b"!keep.log\n")
            .file("/repo/src/keep.log", b"")
            .file("/repo/src/drop.log", b"")
            .file("/repo/src/secret.txt", b"")
            .file("/repo/src/main.rs", b"")
            .file("/repo/src/target/out", b"")
            .build()
    };
    let ignoring = QuickOpenOptions {
        gitignore: true,
        ..options()
    };
    // Rooted below the repository's top: its rules still apply.
    let update = run(build(), "/repo/src", ignoring.clone(), "").await;
    assert_eq!(rels(&update), ["main.rs", "keep.log"]);

    let update = run(build(), "/repo/src", options(), "").await;
    assert_eq!(update.walked, 6);
}

#[tokio::test]
async fn excluded_names_and_the_limit_cut_the_walk_short() {
    let build = || {
        MockVfs::builder()
            .file("/r/node_modules/x/y.js", b"")
            .file("/r/a.txt", b"")
            .file("/r/b.txt", b"")
            .file("/r/c.txt", b"")
            .build()
    };
    let excluding = QuickOpenOptions {
        exclude: vec!["node_*".into()],
        ..options()
    };
    let update = run(build(), "/r", excluding, "").await;
    assert_eq!(rels(&update), ["a.txt", "b.txt", "c.txt"]);

    let limited = QuickOpenOptions {
        limit: 2,
        ..options()
    };
    let update = run(build(), "/r", limited, "").await;
    assert_eq!(update.walked, 2);
    assert!(update.truncated);
}

#[tokio::test]
async fn a_new_query_reranks_what_was_walked() {
    let vfs = MockVfs::builder()
        .file("/r/alpha.txt", b"")
        .file("/r/beta.txt", b"")
        .build();
    let registry = Arc::new(VfsRegistry::with_root(vfs));
    let quick_open = QuickOpen::new(registry);
    let (query_tx, query_rx) = watch::channel(String::new());
    let (tx, mut rx) = mpsc::channel(16);
    let run = quick_open.run(
        VfsPath::new(VfsId::ROOT, PathBuf::from_wire_str("/r")),
        options(),
        query_rx,
        tx,
    );
    tokio::pin!(run);
    let mut asked = false;
    loop {
        tokio::select! {
            result = &mut run => panic!("the run ended: {result:?}"),
            update = rx.recv() => {
                let update = update.unwrap();
                if update.walking {
                    continue;
                }
                if !asked {
                    query_tx.send("bt".into()).unwrap();
                    asked = true;
                } else if update.query == "bt" {
                    assert_eq!(rels(&update), ["beta.txt"]);
                    break;
                }
            }
        }
    }
    drop(query_tx);
    assert!(run.await.is_ok());
}

#[tokio::test]
async fn a_remote_run_streams_updates_and_takes_new_queries() {
    use crate::api::{QuickOpenDispatcher, QuickOpenUpdateDispatcher};

    let vfs = MockVfs::builder()
        .file("/r/alpha.txt", b"")
        .file("/r/beta.txt", b"")
        .build();
    let quick_open = Arc::new(QuickOpen::new(Arc::new(VfsRegistry::with_root(vfs))));
    let (host_stream, agent_stream) = tokio::io::duplex(64 * 1024);
    let (agent_outbox, agent_inbox) = Communicator::create_outbox();
    let _agent = Communicator::with_dispatcher_and_outbox(
        QuickOpenDispatcher::new(agent_outbox.clone(), quick_open),
        agent_stream,
        agent_outbox,
        agent_inbox,
    );
    let pending = PendingQuickOpens::default();
    let host =
        Communicator::with_dispatcher(QuickOpenUpdateDispatcher::new(pending.clone()), host_stream);
    let remote = Remote::new(host, pending.clone());

    let (query_tx, query_rx) = watch::channel(String::new());
    let (tx, mut rx) = mpsc::channel(16);
    let mut run = Box::pin(remote.run(
        VfsPath::new(VfsId::ROOT, PathBuf::from_wire_str("/r")),
        options(),
        query_rx,
        tx,
    ));
    let mut asked = false;
    loop {
        tokio::select! {
            result = &mut run => panic!("the run ended: {result:?}"),
            update = rx.recv() => {
                let update = update.unwrap();
                if update.walking {
                    continue;
                }
                if !asked {
                    assert_eq!(rels(&update), ["beta.txt", "alpha.txt"]);
                    query_tx.send("al".into()).unwrap();
                    asked = true;
                } else if update.query == "al" {
                    assert_eq!(rels(&update), ["alpha.txt"]);
                    break;
                }
            }
        }
    }
    drop(run);
    assert!(pending.lock().is_empty());
}
