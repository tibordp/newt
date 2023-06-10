use newt_common::{
    api::{FilesystemDispatcher, TerminalDispatcher},
    rpc::DispatcherExt,
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

#[tokio::main]
async fn main() -> Result<(), Error> {
    pretty_env_logger::init();

    let args = Args::parse();

    let mut rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(tokio::io::stdin());
    let mut tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(tokio::io::stdout());

    if args.compression {
        rx = Box::new(ZstdDecoder::new(tokio::io::BufReader::new(rx)));
        tx = Box::new(ZstdEncoder::new(tx));
    }

    let stream = Duplex::new(rx, tx);
    let dispatcher = FilesystemDispatcher::new(newt_common::filesystem::Local::new())
        .chain(TerminalDispatcher::new(newt_common::terminal::Local::new()));

    let rpc = newt_common::rpc::Communicator::with_dispatcher(dispatcher, stream);
    rpc.closed().await;

    Ok(())
}
