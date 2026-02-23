use newt_common::{
    api::{FileReaderDispatcher, FilesystemDispatcher, OperationDispatcher, TerminalDispatcher},
    rpc::{Communicator, DispatcherExt},
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

    // Create outbox channel first so OperationDispatcher can use it
    let (outbox, inbox) = Communicator::create_outbox();

    let dispatcher = FilesystemDispatcher::new(newt_common::filesystem::Local::new())
        .chain(TerminalDispatcher::new(newt_common::terminal::Local::new()))
        .chain(FileReaderDispatcher::new(newt_common::file_reader::Local::new()))
        .chain(OperationDispatcher::new(outbox.clone()));

    let rpc = Communicator::with_dispatcher_and_outbox(dispatcher, stream, outbox, inbox);
    rpc.closed().await;

    Ok(())
}
