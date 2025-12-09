mod backend;
mod lang;
mod state;
mod utils;

use backend::LspBackend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:9257").await?;
    tracing::info!("Starting jrsls at port 9257");

    let (service, socket) = LspService::new(LspBackend::new);
    let (stream, _) = listener.accept().await?;
    let (read, write) = tokio::io::split(stream);

    Server::new(read, write, socket).serve(service).await;

    Ok(())
}
