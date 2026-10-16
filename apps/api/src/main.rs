use std::net::SocketAddr;

use anyhow::Context;
use tikkit_api::{AppState, app, db};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Development convenience: load ./.env (or a parent's) without overriding real env vars.
    dotenvy::dotenv().ok();
    let address: SocketAddr = std::env::var("API_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3001".to_owned())
        .parse()
        .context("API_ADDR must be a socket address such as 127.0.0.1:3001")?;
    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL is required, e.g. postgres://tikkit:tikkit@127.0.0.1:5432/tikkit")?;

    let pool = db::connect(&database_url).await?;
    db::migrate(&pool).await?;

    let listener = TcpListener::bind(address).await?;
    println!("Tikkit API listening on http://{}", listener.local_addr()?);

    axum::serve(listener, app(AppState { pool }))
        .with_graceful_shutdown(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("Unable to listen for shutdown: {error}");
            }
        })
        .await?;
    Ok(())
}
