use std::sync::Arc;

use log::info;
use newt_common::Error;
use newt_engine::{
    agent_resolver::AgentResolver,
    api::{
        EnricherDispatcher, FilesystemDispatcher, HotPathsDispatcher, OperationDispatcher,
        ShellServiceDispatcher, TerminalDispatcher, VfsDispatcher, VfsMountDispatcher,
    },
    askpass,
    enrich::{Enrichers, du::DuEnricher, git::GitEnricher, package::PackageEnricher},
    hot_paths,
    operation::OperationContext,
    shell::LocalShellService,
    vfs::{
        LocalVfs, VfsRegistry, VfsRegistryFs,
        mount::{SftpAskpass, VfsRegistryManager},
    },
};
use newt_rpc::{Communicator, DispatcherExt};

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

    /// Started by the macOS elevated transport (Authorization Services): finish
    /// becoming root before anything else.
    #[cfg(target_os = "macos")]
    #[arg(long)]
    macos_elevated: bool,

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
    use newt_engine::askpass::{AskpassRequest, AskpassResponse, PromptType};

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

    if newt_engine::askpass::write_msg(&mut stream, &request).is_err() {
        return 1;
    }

    let response: AskpassResponse = match newt_engine::askpass::read_msg(&mut stream) {
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
    // Shell-integration CLI mode short-circuits first: invoked as `newt`
    // through the per-session shim (argv[0] symlink on Unix, NEWT_CLI from
    // the .cmd shim on Windows). Invoked as `newt-agent`, the binary always
    // behaves as the agent — so inspecting it from inside an integrated
    // terminal (where NEWT_SHELL_SOCK is always set) stays unsurprising.
    let invoked_as_newt = std::env::args()
        .next()
        .map(|argv0| {
            std::path::Path::new(&argv0)
                .file_stem()
                .is_some_and(|stem| stem == "newt")
        })
        .unwrap_or(false);
    if newt_engine::shell_control::is_cli_invocation(invoked_as_newt, false) {
        std::process::exit(newt_engine::shell_control::run_cli());
    }

    // Askpass mode short-circuits before any heavy init.
    if let Ok(sock_path) = std::env::var("NEWT_ASKPASS_SOCK") {
        std::process::exit(run_askpass(&sock_path));
    }

    let args = Args::parse();

    #[cfg(target_os = "macos")]
    if args.macos_elevated
        && let Err(e) = newt_engine::elevation::macos::adopt()
    {
        eprintln!("failed to become root: {}", e);
        std::process::exit(1);
    }

    // Before the runtime spawns its workers — `set_var` is not thread-safe.
    // A Mac agent gets its locale here for the same reason the host does; a
    // Linux one already has one from pam_env and this is a no-op.
    newt_engine::locale::ensure_locale();

    // A panic means this process can't be trusted any more: end it, so the
    // host sees its connection close, rather than run on with a task dead
    // behind a connection that still looks alive.
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info);
        std::process::abort();
    }));

    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run_agent(args)) {
        eprintln!("agent error: {}", e);
        std::process::exit(1);
    }
}

async fn run_agent(args: Args) -> Result<(), Error> {
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
            let client = newt_engine::elevation::windows::connect(&name).await?;
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

    // FS-only mode (pane-scoped agent mounts): serve the VFS API over the
    // local filesystem and nothing else. The mode is a soft trust boundary —
    // none of the full-session services below are ever constructed, so this
    // agent structurally cannot spawn terminals, run operations, or mount
    // further VFSes.
    if args.serve_vfs {
        info!("agent started (serve-vfs), entering RPC loop");
        let rpc = Communicator::with_dispatcher(stream, |outbox| {
            VfsDispatcher::new(Arc::new(LocalVfs::new()), outbox.clone())
        });
        rpc.closed().await;
        info!("RPC connection closed, agent exiting");
        return Ok(());
    }

    let root_vfs = Arc::new(LocalVfs::new());
    let registry = Arc::new(VfsRegistry::with_root(root_vfs));
    let filesystem = VfsRegistryFs::new(registry.clone());

    // OnceLock for the host communicator — set after the RPC loop starts,
    // allows RemoteVfs to call back to the host.
    let host_communicator: Arc<std::sync::OnceLock<Communicator>> =
        Arc::new(std::sync::OnceLock::new());

    // RPC-backed resolver: serves its own triple from the running
    // executable, fetches foreign triples (nested agent mounts) from the
    // host, and reports the host's agent hash.
    let resolver = Arc::new(newt_engine::agent_resolver::Remote::new(
        host_communicator.clone(),
    ));
    let askpass_binary = resolver.find_local_agent_binary()?;
    // Shell-integration shim target: this very executable (invoked as `newt`).
    let cli_binary = askpass_binary.clone();

    let askpass_provider: Arc<dyn askpass::AskpassProvider> =
        Arc::new(askpass::Remote::new(host_communicator.clone()));

    let enrichers = Arc::new(
        Enrichers::new(registry.clone())
            .with(Arc::new(GitEnricher::new(Vec::new())))
            .with(Arc::new(DuEnricher))
            .with(Arc::new(PackageEnricher)),
    );

    // Shell integration: the control server the `newt` CLI talks to.
    // Control-plane verbs forward to the host over the RPC channel; the
    // `cat` data plane reads from this side's registry.
    let shell_handler = Arc::new(AgentShellHandler {
        host: host_communicator.clone(),
        fs: Arc::new(VfsRegistryFs::new(registry.clone())),
    });
    let shell_integration =
        match newt_engine::shell_control::ShellIntegration::start(&cli_binary, shell_handler) {
            Ok(si) => Some(si),
            Err(e) => {
                log::warn!("shell integration disabled: {e}");
                None
            }
        };

    let op_context = Arc::new(OperationContext {
        registry: registry.clone(),
        shell_integration: shell_integration.clone(),
        spooler: newt_engine::spool::Spooler::new(Default::default()),
    });

    info!("agent started, entering RPC loop");

    let rpc = Communicator::with_dispatcher(stream, |outbox| {
        // Progress reports from agent-side VFSes (SearchVfs, …) are
        // forwarded over the RPC channel via API_VFS_PROGRESS; the host's
        // LocalProgressSink applies them to MainWindowState. The agent's
        // sink owns a fire-and-forget mpsc forwarder task spawned when
        // constructed.
        let progress_sink: Arc<dyn newt_engine::vfs::VfsProgressSink> =
            Arc::new(newt_engine::vfs::RemoteProgressSink::new(outbox.clone()));
        let vfs_manager = VfsRegistryManager::new_with_host_communicator(
            registry.clone(),
            host_communicator.clone(),
        )
        .with_sftp_askpass(SftpAskpass {
            askpass_binary,
            provider: askpass_provider,
        })
        .with_progress_sink(progress_sink)
        .with_agent_resolver(resolver);

        FilesystemDispatcher::new(filesystem, outbox.clone())
            .chain(ShellServiceDispatcher::new(LocalShellService))
            .chain(EnricherDispatcher::new(outbox.clone(), enrichers))
            .chain(newt_engine::api::QuickOpenDispatcher::new(
                outbox.clone(),
                Arc::new(newt_engine::quick_open::QuickOpen::new(registry.clone())),
            ))
            .chain(TerminalDispatcher::new(
                newt_engine::terminal::Local::with_shell_integration(shell_integration),
                outbox.clone(),
            ))
            .chain(OperationDispatcher::new(outbox.clone(), op_context))
            .chain(VfsMountDispatcher::new(vfs_manager))
            .chain(HotPathsDispatcher::new(hot_paths::Local::new()))
            .chain(newt_engine::api::DiscoveryDispatcher::new(
                newt_engine::discovery::Local::new(Vec::new()),
            ))
    });

    // Now that the communicator exists, make it available for Remote VFS mounts
    let _ = host_communicator.set(rpc.clone());

    rpc.closed().await;

    info!("RPC connection closed, agent exiting");
    Ok(())
}

/// Shell-integration verb handler on the agent: control plane forwards to
/// the host's session state over the RPC channel; the `cat` data plane
/// reads from the agent-side VFS registry (where the session's mounts
/// live), so file bytes never round-trip through the host.
struct AgentShellHandler {
    host: Arc<std::sync::OnceLock<Communicator>>,
    fs: Arc<dyn newt_engine::filesystem::Filesystem>,
}

#[async_trait::async_trait]
impl newt_engine::shell_control::ShellControlHandler for AgentShellHandler {
    async fn control(
        &self,
        req: newt_engine::shell_control::ControlRequest,
    ) -> newt_engine::shell_control::ControlResult {
        let Some(host) = self.host.get() else {
            return Err("session not connected".to_string());
        };
        host.invoke::<_, newt_engine::shell_control::ControlResult>(
            newt_engine::api::API_HOST_SHELL_CONTROL,
            &req,
        )
        .await
        .map_err(|e| e.to_string())?
    }

    async fn read_file(
        &self,
        path: newt_common::path::VfsPath,
    ) -> Result<newt_engine::filesystem::ByteStream, String> {
        Ok(newt_engine::filesystem::file_reader_stream(
            self.fs.clone(),
            path,
        ))
    }
}
