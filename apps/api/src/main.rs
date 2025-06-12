use std::{error::Error, net::SocketAddr};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let address: SocketAddr = std::env::var("API_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3001".to_owned())
        .parse()?;
    let listener = TcpListener::bind(address).await?;
    println!("Tikkit API listening on http://{}", listener.local_addr()?);

    axum::serve(listener, tikkit_api::app())
        .with_graceful_shutdown(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("Unable to listen for shutdown: {error}");
            }
        })
        .await?;
    Ok(())
}
