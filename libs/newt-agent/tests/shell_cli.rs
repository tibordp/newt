//! End-to-end test of the shell-integration CLI modality: the real
//! `newt-agent` binary, invoked through the per-session `newt` shim
//! (argv[0] = "newt"), against a live control server with a mock handler.

#![cfg(unix)]

use std::sync::Arc;

use newt_common::shell_control::{
    ByteStream, ControlRequest, ControlResponse, ControlResult, ENV_SOCK, ShellControlHandler,
    ShellIntegration,
};
use newt_common::vfs::VfsPath;

struct MockHandler;

#[async_trait::async_trait]
impl ShellControlHandler for MockHandler {
    async fn control(&self, req: ControlRequest) -> ControlResult {
        match req {
            ControlRequest::Pwd { .. } => Ok(ControlResponse::Text("/it/works".into())),
            ControlRequest::Navigate { path, .. } if path == "/missing" => {
                Err("no such directory".into())
            }
            ControlRequest::Navigate { .. } => Ok(ControlResponse::Ok),
            ControlRequest::ResolveFile { .. } => Ok(ControlResponse::ResolvedFile(VfsPath::root(
                newt_common::vfs::VfsId::ROOT,
            ))),
            _ => Err("unhandled".into()),
        }
    }

    async fn read_file(&self, _path: VfsPath) -> Result<ByteStream, String> {
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            bytes::Bytes::from_static(b"file contents"),
        )])))
    }
}

async fn run_shim(si: &ShellIntegration, args: &[&str], sock: &str) -> std::process::Output {
    let shim = std::path::Path::new(si.sock_addr())
        .parent()
        .unwrap()
        .join("newt");
    tokio::process::Command::new(&shim)
        .args(args)
        .env(ENV_SOCK, sock)
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn cli_through_shim() {
    let si = ShellIntegration::start(
        std::path::Path::new(env!("CARGO_BIN_EXE_newt-agent")),
        Arc::new(MockHandler),
    )
    .unwrap();
    let sock = si.sock_addr().to_string();

    // pwd prints the handler's answer and exits 0.
    let out = run_shim(&si, &["pwd"], &sock).await;
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "/it/works");

    // cd errors surface on stderr with exit 1.
    let out = run_shim(&si, &["cd", "/missing"], &sock).await;
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such directory"));

    // cat streams raw bytes to stdout.
    let out = run_shim(&si, &["cat", "/some/file"], &sock).await;
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"file contents");

    // Unknown verb: usage error, exit 1 (invoked as `newt` is CLI intent).
    let out = run_shim(&si, &["frobnicate"], &sock).await;
    assert_eq!(out.status.code(), Some(1));

    // Stale socket (session gone): exit 2.
    let out = run_shim(&si, &["pwd"], "/nonexistent/newt.sock").await;
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no Newt session"));

    // --help works without a connection.
    let out = run_shim(&si, &["--help"], "/nonexistent/newt.sock").await;
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("newt cd"));
}
