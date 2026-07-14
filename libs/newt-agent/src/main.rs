use std::sync::Arc;

use log::info;
use newt_common::{
    Error,
    agent_resolver::AgentResolver,
    api::{
        FileReaderDispatcher, FilesystemDispatcher, HotPathsDispatcher, OperationDispatcher,
        PendingVfsReadStreams, SftpAskpass, ShellServiceDispatcher, TerminalDispatcher,
        VfsDispatcher, VfsMountDispatcher, VfsReadChunkDispatcher, VfsRegistryManager,
    },
    askpass,
    filesystem::LocalShellService,
    hot_paths,
    operation::OperationContext,
    rpc::{Communicator, DispatcherExt},
    vfs::{LocalVfs, VfsRegistry, VfsRegistryFileReader, VfsRegistryFs},
};

use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_duplex::Duplex;

use clap::{ArgAction, Parser};

#[derive(Parser)]
#[command(author, version = include_str!(concat!(env!("OUT_DIR"), "/long_version.txt")), about, long_about = None)]
struct Args {
    /// Whether to use compression
    #[arg(short, long)]
    compression: bool,

    /// Print the compiled target triple and exit. Useful when verifying
    /// that the right binary made it onto a remote host.
    #[arg(long)]
    print_triple: bool,

    /// Speak RPC over this named pipe instead of stdin/stdout. Used by the
    /// Windows elevated transport, where `ShellExecuteEx "runas"` cannot
    /// redirect stdio.
    #[cfg(windows)]
    #[arg(long, value_name = "NAME")]
    pipe: Option<String>,

    /// Serve only the filesystem (VFS) API — used for pane-scoped agent mounts.
    #[arg(long)]
    serve_vfs: bool,

    /// Increase log verbosity (-v: debug, -vv: trace). Ignored if RUST_LOG is set.
    #[arg(short, long, action = ArgAction::Count, conflicts_with = "quiet")]
    verbose: u8,

    /// Only log errors. Ignored if RUST_LOG is set.
    #[arg(short, long)]
    quiet: bool,
}

/// Apply `-v`/`-q` to the `RUST_LOG` env var if the user hasn't already
/// set one. The explicit env var always wins.
fn apply_log_flags(verbose: u8, quiet: bool) {
    if std::env::var_os("RUST_LOG").is_some() {
        return;
    }
    let level = match (quiet, verbose) {
        (true, _) => "error",
        (_, 0) => "info",
        (_, 1) => "debug",
        (_, _) => "trace",
    };
    // SAFETY: single-threaded startup, before any logger or other env-reader
    // has spawned.
    unsafe { std::env::set_var("RUST_LOG", level) };
}

/// Connect to the parent's askpass endpoint. Unix: a Unix-domain socket.
/// Windows: a named pipe — opened as a r/w file handle, retried briefly
/// while the listener is between instances (busy) or hasn't recreated one
/// yet (not-found).
#[cfg(unix)]
fn connect_askpass(endpoint: &str) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(endpoint)
}

#[cfg(windows)]
fn connect_askpass(endpoint: &str) -> std::io::Result<std::fs::File> {
    // ERROR_FILE_NOT_FOUND (2): listener hasn't recreated an instance yet.
    // ERROR_PIPE_BUSY (231): all instances are serving other clients.
    for _ in 0..50 {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(endpoint)
        {
            Ok(f) => return Ok(f),
            Err(e) if matches!(e.raw_os_error(), Some(2) | Some(231)) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "askpass pipe unavailable",
    ))
}

/// SSH_ASKPASS mode: connect to the parent process over the askpass
/// endpoint, send the prompt, read the response, print it to stdout for SSH.
fn run_askpass(sock_path: &str) -> i32 {
    use newt_common::askpass::{AskpassRequest, AskpassResponse, PromptType};

    let prompt_type_str = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    let prompt = std::env::args().nth(1).unwrap_or_default();

    let prompt_type = match prompt_type_str.as_str() {
        "confirm" => PromptType::Confirm,
        "none" => PromptType::Info,
        _ => PromptType::Secret,
    };

    let mut stream = match connect_askpass(sock_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("newt-askpass: connect failed: {}", e);
            return 1;
        }
    };

    let request = AskpassRequest {
        prompt_type,
        prompt,
    };

    if newt_common::askpass::write_msg(&mut stream, &request).is_err() {
        return 1;
    }

    let response: AskpassResponse = match newt_common::askpass::read_msg(&mut stream) {
        Ok(r) => r,
        Err(_) => return 1,
    };

    match response.0 {
        Some(value) => {
            print!("{}", value);
            0
        }
        None => 1, // cancelled
    }
}

fn main() {
    // Askpass mode short-circuits before any heavy init.
    if let Ok(sock_path) = std::env::var("NEWT_ASKPASS_SOCK") {
        std::process::exit(run_askpass(&sock_path));
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run_agent()) {
        eprintln!("agent error: {}", e);
        std::process::exit(1);
    }
}

/// Connect to the host's named pipe, retrying briefly while it's busy
/// (ERROR_PIPE_BUSY = 231) — the host creates the server right before
/// launching us, so a short race is expected.
#[cfg(windows)]
async fn connect_pipe(
    name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, Error> {
    use std::time::Duration;
    use tokio::net::windows::named_pipe::ClientOptions;

    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..200 {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => {
                return Err(Error::custom(format!(
                    "failed to open named pipe {:?}: {}",
                    name, e
                )));
            }
        }
    }
    Err(Error::custom(format!(
        "timed out connecting to named pipe {:?}",
        name
    )))
}

async fn run_agent() -> Result<(), Error> {
    let args = Args::parse();
    if args.print_triple {
        println!("{}", env!("NEWT_TARGET_TRIPLE"));
        return Ok(());
    }
    apply_log_flags(args.verbose, args.quiet);
    pretty_env_logger::init();

    #[cfg(windows)]
    let pipe = args.pipe.clone();
    #[cfg(not(windows))]
    let pipe: Option<String> = None;

    let (mut rx, mut tx): (
        Box<dyn AsyncRead + Send + Unpin>,
        Box<dyn AsyncWrite + Send + Unpin>,
    ) = match pipe {
        #[cfg(windows)]
        Some(name) => {
            let client = connect_pipe(&name).await?;
            let (r, w) = tokio::io::split(client);
            (Box::new(r), Box::new(w))
        }
        #[cfg(not(windows))]
        Some(_) => unreachable!("pipe is always None on non-Windows"),
        None => (Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout())),
    };

    if args.compression {
        rx = Box::new(ZstdDecoder::new(tokio::io::BufReader::new(rx)));
        tx = Box::new(ZstdEncoder::new(tx));
    }

    let stream = Duplex::new(rx, tx);

    // Outbox first so OperationDispatcher can use it.
    let (outbox, inbox) = Communicator::create_outbox();

    // FS-only mode (pane-scoped agent mounts): serve the VFS API over the
    // local filesystem and nothing else. The mode is a soft trust boundary —
    // none of the full-session services below are ever constructed, so this
    // agent structurally cannot spawn terminals, run operations, or mount
    // further VFSes.
    if args.serve_vfs {
        let dispatcher = VfsDispatcher::new(Arc::new(LocalVfs::new()), outbox.clone());
        info!("agent started (serve-vfs), entering RPC loop");
        let rpc = Communicator::with_dispatcher_and_outbox(dispatcher, stream, outbox, inbox);
        rpc.closed().await;
        info!("RPC connection closed, agent exiting");
        return Ok(());
    }

    let root_vfs = Arc::new(LocalVfs::new());
    let registry = Arc::new(VfsRegistry::with_root(root_vfs));
    let op_context = Arc::new(OperationContext {
        registry: registry.clone(),
    });
    let filesystem = VfsRegistryFs::new(registry.clone());

    // OnceLock for the host communicator — set after the RPC loop starts,
    // allows RemoteVfs to call back to the host.
    let host_communicator: Arc<std::sync::OnceLock<Communicator>> =
        Arc::new(std::sync::OnceLock::new());

    // Shared map for routing read-chunk notifications from the host to
    // the correct RemoteVfs read stream.
    let pending_read_streams: PendingVfsReadStreams = Default::default();

    // RPC-backed resolver: serves its own triple from the running
    // executable, fetches foreign triples (nested agent mounts) from the
    // host, and reports the host's agent hash.
    let fetch_streams: PendingVfsReadStreams = Default::default();
    let resolver = Arc::new(newt_common::agent_resolver::Remote::new(
        host_communicator.clone(),
        fetch_streams.clone(),
    ));
    let askpass_binary = resolver.find_local_agent_binary()?;

    let askpass_provider: Arc<dyn askpass::AskpassProvider> =
        Arc::new(askpass::Remote::new(host_communicator.clone()));

    // Progress reports from agent-side VFSes (SearchVfs, …) are
    // forwarded over the RPC channel via API_VFS_PROGRESS; the host's
    // LocalProgressSink applies them to MainWindowState. The agent's
    // sink owns a fire-and-forget mpsc forwarder task spawned when
    // constructed.
    let progress_sink: Arc<dyn newt_common::vfs::VfsProgressSink> =
        Arc::new(newt_common::vfs::RemoteProgressSink::new(outbox.clone()));

    let vfs_manager = VfsRegistryManager::new_with_host_communicator(
        registry.clone(),
        host_communicator.clone(),
        pending_read_streams.clone(),
    )
    .with_sftp_askpass(SftpAskpass {
        askpass_binary,
        provider: askpass_provider,
    })
    .with_progress_sink(progress_sink)
    .with_agent_resolver(resolver);

    let dispatcher = FilesystemDispatcher::new(filesystem, outbox.clone())
        .chain(ShellServiceDispatcher::new(LocalShellService))
        .chain(TerminalDispatcher::new(newt_common::terminal::Local::new()))
        .chain(FileReaderDispatcher::new(VfsRegistryFileReader::new(
            registry.clone(),
        )))
        .chain(OperationDispatcher::new(outbox.clone(), op_context))
        .chain(VfsMountDispatcher::new(vfs_manager))
        .chain(VfsReadChunkDispatcher::new(pending_read_streams))
        .chain(VfsReadChunkDispatcher::for_api(
            newt_common::api::API_HOST_FETCH_AGENT_CHUNK,
            fetch_streams,
        ))
        .chain(HotPathsDispatcher::new(hot_paths::Local::new()));

    info!("agent started, entering RPC loop");

    let rpc = Communicator::with_dispatcher_and_outbox(dispatcher, stream, outbox, inbox);

    // Now that the communicator exists, make it available for Remote VFS mounts
    let _ = host_communicator.set(rpc.clone());

    rpc.closed().await;

    info!("RPC connection closed, agent exiting");
    Ok(())
}
