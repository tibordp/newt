use newt_common::{filesystem::Local, Error};

use tokio_duplex::Duplex;

#[tokio::main]
async fn main() -> Result<(), Error> {
    pretty_env_logger::init();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let stream = Duplex::new(stdin, stdout);
    let dispatcher = newt_common::rpc::FilesystemDispatcher::new(Local::new());
    let rpc = newt_common::rpc::Communicator::with_dispatcher(dispatcher);
    rpc.handle_connection(stream).await?;
    Ok(())
}
