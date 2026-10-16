//! Serving with bounded graceful shutdown (issue #8).

use std::{future::Future, io, net::SocketAddr, time::Duration};

use axum::Router;
use tokio::{net::TcpListener, sync::watch};

/// How [`serve`] finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// The server stopped on its own (only happens on listener errors).
    Stopped,
    /// The shutdown signal fired and all in-flight requests completed.
    Drained,
    /// The shutdown signal fired and requests were still running when the
    /// drain timeout elapsed; they were dropped.
    TimedOut,
}

/// Serves `app` until `signal` resolves, then stops accepting connections and
/// waits up to `drain_timeout` for in-flight requests before giving up.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    signal: impl Future<Output = ()>,
    drain_timeout: Duration,
) -> io::Result<ShutdownOutcome> {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let mut server = tokio::spawn(
        // Peer addresses feed rate limiting (`auth::ClientIp`).
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = stop_rx.wait_for(|stop| *stop).await;
        })
        .into_future(),
    );

    tokio::select! {
        result = &mut server => {
            return join_result(result).map(|()| ShutdownOutcome::Stopped);
        }
        () = signal => {}
    }

    tracing::info!(
        drain_timeout_secs = drain_timeout.as_secs(),
        "shutdown requested; no longer accepting connections, draining in-flight requests"
    );
    let _ = stop_tx.send(true);
    match tokio::time::timeout(drain_timeout, &mut server).await {
        Ok(result) => join_result(result).map(|()| ShutdownOutcome::Drained),
        Err(_) => {
            server.abort();
            tracing::warn!("drain timeout elapsed; dropping remaining requests");
            Ok(ShutdownOutcome::TimedOut)
        }
    }
}

fn join_result(result: Result<io::Result<()>, tokio::task::JoinError>) -> io::Result<()> {
    result.map_err(io::Error::other)?
}

/// Resolves on Ctrl+C (SIGINT) or, on Unix, SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "unable to listen for Ctrl+C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "unable to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received Ctrl+C"),
        () = terminate => tracing::info!("received SIGTERM"),
    }
}
