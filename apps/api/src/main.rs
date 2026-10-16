use std::{future::IntoFuture, time::Duration};

use anyhow::Context;
use tikkit_api::{AppState, app, config::Config, db};
use tokio::{net::TcpListener, sync::watch};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Development convenience: load ./.env (or a parent's) without overriding real
    // env vars. Production reads only the real environment.
    if std::env::var("ENVIRONMENT").as_deref() != Ok("production") {
        dotenvy::dotenv().ok();
    }
    let config = Config::from_env()?;

    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    let listener = TcpListener::bind(config.api_addr)
        .await
        .with_context(|| format!("cannot listen on API_ADDR {}", config.api_addr))?;
    println!("Tikkit API listening on http://{}", listener.local_addr()?);

    // Stop accepting on SIGTERM/Ctrl+C, then give in-flight requests a bounded drain window.
    let (stopping, mut stopped) = watch::channel(false);
    let server = axum::serve(listener, app(AppState { pool: pool.clone() }))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            println!(
                "Shutting down; draining requests for up to {:?}",
                config.shutdown_timeout
            );
            stopping.send_replace(true);
        })
        .into_future();
    let deadline = async {
        let _ = stopped.wait_for(|stop| *stop).await;
        tokio::time::sleep(config.shutdown_timeout).await;
    };
    tokio::select! {
        result = server => result?,
        () = deadline => eprintln!("Drain timeout reached; aborting remaining requests"),
    }
    // Bounded so a stuck connection cannot hold the process past the drain window.
    if tokio::time::timeout(Duration::from_secs(5), pool.close())
        .await
        .is_err()
    {
        eprintln!("Timed out closing database connections");
    }
    println!("Tikkit API stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("Unable to listen for Ctrl+C: {error}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                eprintln!("Unable to listen for SIGTERM: {error}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
