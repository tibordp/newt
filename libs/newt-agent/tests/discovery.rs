//! A full-session agent answers the discovery verbs over RPC — guards the
//! `DiscoveryDispatcher` registration in the agent's dispatcher chain and
//! the `discovery::Remote` provider path that pane-scoped discovery uses
//! in remote sessions.

use tokio::io::BufReader;

use newt_common::api::{PendingVfsReadStreams, VfsReadChunkDispatcher};
use newt_common::connect::make_stream;
use newt_common::discovery::DiscoveryProvider;
use newt_common::rpc::Communicator;

#[tokio::test]
async fn full_session_agent_serves_discovery() {
    // The agent-side ssh_hosts reads $HOME/.ssh/config — point the agent at
    // a fabricated home so the test is hermetic.
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".ssh")).unwrap();
    std::fs::write(
        home.path().join(".ssh").join("config"),
        "Host testbox\n  HostName 10.0.0.7\n  User dev\n",
    )
    .unwrap();

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_newt-agent"))
        .env("HOME", home.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stream = make_stream(BufReader::new(stdout), stdin);
    let pending: PendingVfsReadStreams = Default::default();
    let (outbox, inbox) = Communicator::create_outbox();
    let communicator = Communicator::with_dispatcher_and_outbox(
        VfsReadChunkDispatcher::new(pending),
        stream,
        outbox,
        inbox,
    );

    let provider = newt_common::discovery::Remote::new(communicator);

    let hosts = provider.ssh_hosts().await.unwrap();
    let testbox = hosts
        .items
        .iter()
        .find(|h| h.host == "testbox")
        .expect("agent-side ssh config host must be discovered");
    assert_eq!(testbox.hostname.as_deref(), Some("10.0.0.7"));
    assert_eq!(testbox.user.as_deref(), Some("dev"));

    // The engine allowlist holds over RPC — this must not reach a shell.
    let err = provider
        .containers("evil-engine".to_string())
        .await
        .err()
        .expect("unknown engine must be rejected");
    assert!(err.message.contains("unknown container engine"), "{}", err);
}
