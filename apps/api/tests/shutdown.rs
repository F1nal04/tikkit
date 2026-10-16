//! Graceful shutdown drains in-flight requests within a bound (issue #8).
//! The slow route exists only in this test.

use std::{net::SocketAddr, time::Duration};

use axum::{Router, routing::get};
use tikkit_api::server::{ShutdownOutcome, serve};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};

fn slow_app(delay: Duration) -> Router {
    Router::new().route(
        "/slow",
        get(move || async move {
            tokio::time::sleep(delay).await;
            "done"
        }),
    )
}

async fn start(
    delay: Duration,
    drain: Duration,
) -> (
    SocketAddr,
    oneshot::Sender<()>,
    JoinHandle<std::io::Result<ShutdownOutcome>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(serve(
        listener,
        slow_app(delay),
        async {
            let _ = rx.await;
        },
        drain,
    ));
    (addr, tx, server)
}

/// Sends a raw HTTP/1.1 request and returns the full response text.
async fn request(addr: SocketAddr) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(addr).await?;
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    Ok(response)
}

#[tokio::test]
async fn in_flight_requests_complete_and_new_connections_are_refused() {
    let (addr, stop, server) = start(Duration::from_millis(500), Duration::from_secs(5)).await;
    let in_flight = tokio::spawn(request(addr));
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(()).unwrap();

    let response = in_flight.await.unwrap().unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("done"), "{response}");
    assert_eq!(server.await.unwrap().unwrap(), ShutdownOutcome::Drained);

    // The listener is closed after shutdown.
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn drain_is_bounded_by_the_timeout() {
    let (addr, stop, server) = start(Duration::from_secs(30), Duration::from_millis(300)).await;
    let in_flight = tokio::spawn(request(addr));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    stop.send(()).unwrap();

    assert_eq!(server.await.unwrap().unwrap(), ShutdownOutcome::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3));
    // The binary exits right after this, which drops the abandoned request.
    in_flight.abort();
}
