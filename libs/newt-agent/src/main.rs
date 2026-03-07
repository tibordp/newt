use std::sync::Arc;

use log::info;
use newt_common::{
    api::{
        FileReaderDispatcher, FilesystemDispatcher, HotPathsDispatcher, OperationDispatcher,
        ShellServiceDispatcher, TerminalDispatcher, VfsDispatcher, VfsRegistryManager,
    },
    filesystem::LocalShellService,
    hot_paths,
    operation::OperationContext,
    rpc::{Communicator, DispatcherExt},
    vfs::{LocalVfs, VfsRegistry, VfsRegistryFileReader, VfsRegistryFs},
    Error,
};

use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_duplex::Duplex;

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Whether to use compression
    #[arg(short, long)]
    compression: bool,
}

/// SSH_ASKPASS mode: connect to the parent process via a Unix domain socket,
/// send the prompt, read the response, and print it to stdout for SSH.
fn run_askpass(sock_path: &str) -> i32 {
    use newt_common::askpass::{AskpassRequest, AskpassResponse, PromptType};

    let prompt_type_str = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    let prompt = std::env::args().nth(1).unwrap_or_default();

    let prompt_type = match prompt_type_str.as_str() {
        "confirm" => PromptType::Confirm,
        "none" => PromptType::Info,
        _ => PromptType::Secret,
    };

    let mut stream = match std::os::unix::net::UnixStream::connect(sock_path) {
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
    // Check for askpass mode before initializing anything heavy
    if let Ok(sock_path) = std::env::var("NEWT_ASKPASS_SOCK") {
        std::process::exit(run_askpass(&sock_path));
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    if let Err(e) = rt.block_on(run_agent()) {
        eprintln!("agent error: {}", e);
        std::process::exit(1);
    }
}

async fn run_agent() -> Result<(), Error> {
    pretty_env_logger::init();
    let args = Args::parse();

    let mut rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(tokio::io::stdin());
    let mut tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(tokio::io::stdout());

    if args.compression {
        rx = Box::new(ZstdDecoder::new(tokio::io::BufReader::new(rx)));
        tx = Box::new(ZstdEncoder::new(tx));
    }

    let stream = Duplex::new(rx, tx);

    // Create outbox channel first so OperationDispatcher can use it
    let (outbox, inbox) = Communicator::create_outbox();

    let root_vfs = Arc::new(LocalVfs::new());
    let registry = Arc::new(VfsRegistry::with_root(root_vfs));
    let op_context = Arc::new(OperationContext {
        registry: registry.clone(),
    });
    let filesystem = VfsRegistryFs::new(registry.clone());

    let dispatcher = FilesystemDispatcher::new(filesystem, outbox.clone())
        .chain(ShellServiceDispatcher::new(LocalShellService))
        .chain(TerminalDispatcher::new(newt_common::terminal::Local::new()))
        .chain(FileReaderDispatcher::new(VfsRegistryFileReader::new(
            registry.clone(),
        )))
        .chain(OperationDispatcher::new(outbox.clone(), op_context))
        .chain(VfsDispatcher::new(VfsRegistryManager::new(
            registry.clone(),
        )))
        .chain(HotPathsDispatcher::new(hot_paths::Local::new()));

    info!("agent started, entering RPC loop");

    let rpc = Communicator::with_dispatcher_and_outbox(dispatcher, stream, outbox, inbox);
    rpc.closed().await;

    info!("RPC connection closed, agent exiting");
    Ok(())
}
