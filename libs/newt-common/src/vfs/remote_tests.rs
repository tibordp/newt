use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::*;
use crate::api::VfsDispatcher;
use crate::test_support::mock_vfs::{FailureSpec, MockVfs};
use crate::vfs::attributes::WriteOptions;

/// A `RemoteVfs` driving `vfs` across a connection.
fn connect(vfs: Arc<MockVfs>) -> RemoteVfs {
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    // The serving side lives as long as its connection.
    let _ = Communicator::with_dispatcher(server_stream, |outbox| {
        VfsDispatcher::new(vfs, outbox.clone())
    });
    RemoteVfs::new(Communicator::new(client_stream), Vec::new())
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn reads_a_file_longer_than_the_window() {
    let content = pattern(40 * crate::rpc::CHUNK_SIZE + 17);
    let remote = connect(MockVfs::builder().file("/f", &content).build());

    let mut reader = remote
        .open_read_async(&PathBuf::from_wire_str("/f"))
        .await
        .unwrap();
    let mut out = Vec::new();
    reader.read_to_end(&mut out).await.unwrap();
    assert_eq!(out, content);
}

#[tokio::test]
async fn an_open_error_comes_from_the_open() {
    let vfs = MockVfs::builder()
        .file("/f", b"content")
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/f"),
            operation: "open_read_async",
            error: Error::custom("simulated open failure"),
            remaining: None,
        })
        .build();
    let remote = connect(vfs);

    let Err(e) = remote.open_read_async(&PathBuf::from_wire_str("/f")).await else {
        panic!("the open succeeded");
    };
    assert!(e.message.contains("simulated open failure"), "{e}");
}

#[tokio::test]
async fn writes_and_commits_a_file_longer_than_the_window() {
    let content = pattern(40 * crate::rpc::CHUNK_SIZE + 17);
    let vfs = MockVfs::builder().build();
    let remote = connect(vfs.clone());

    let mut writer = remote
        .overwrite_async(&PathBuf::from_wire_str("/f"), &WriteOptions::default())
        .await
        .unwrap();
    let mut offset = 0;
    while offset < content.len() {
        offset += writer.write(&content[offset..]).await.unwrap();
    }
    writer.finish().await.unwrap();

    assert_eq!(vfs.read_content("/f"), content);
}

#[tokio::test]
async fn a_commit_error_comes_from_finish() {
    let vfs = MockVfs::builder()
        .failure(FailureSpec {
            path: PathBuf::from_wire_str("/f"),
            operation: "finish",
            error: Error::custom("simulated commit failure"),
            remaining: None,
        })
        .build();
    let remote = connect(vfs);

    let mut writer = remote
        .overwrite_async(&PathBuf::from_wire_str("/f"), &WriteOptions::default())
        .await
        .unwrap();
    writer.write(b"data").await.unwrap();
    let e = writer.finish().await.unwrap_err();
    assert!(e.message.contains("simulated commit failure"), "{e}");
}

#[tokio::test]
async fn dropping_the_writer_leaves_the_file_uncommitted() {
    let vfs = MockVfs::builder().build();
    let remote = connect(vfs.clone());
    let options = WriteOptions {
        create_new: true,
        ..Default::default()
    };

    // The exclusive open creates the file; only a commit keeps it.
    let mut writer = remote
        .overwrite_async(&PathBuf::from_wire_str("/f"), &options)
        .await
        .unwrap();
    writer.write(b"partial").await.unwrap();
    assert!(vfs.exists("/f"));
    drop(writer);

    tokio::time::timeout(Duration::from_secs(1), async {
        while vfs.exists("/f") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the other side committed, or never dropped its writer");
}
