//! An in-process mock AI provider on loopback: scripted replies, recorded
//! requests. Reach it through `Endpoint::Override { allow_private_network: true }`
//! (`test_app_with_provider` for the app). No test talks to a real provider.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use tokio::sync::watch;
use url::Url;

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: vec![("content-type", "application/json".into())],
            body: serde_json::to_vec(&body).unwrap(),
            delay: Duration::ZERO,
        }
    }

    pub fn raw(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

pub struct Seen {
    pub method: Method,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Value,
}

#[derive(Clone)]
struct Script {
    replies: Arc<Mutex<Vec<Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Requests being answered right now, and the most there ever were.
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    /// `false` while [`Mock::hold`] keeps answers back.
    open: watch::Receiver<bool>,
}

/// The longest a held request waits for [`Mock::release`]: below the test
/// workers' call timeout, so a test that never releases fails, not hangs.
const HOLD_LIMIT: Duration = Duration::from_secs(8);

pub struct Mock {
    pub url: Url,
    seen: Arc<Mutex<Vec<Seen>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    open: watch::Sender<bool>,
}

impl Mock {
    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// Requests being answered right now.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// Keeps every answer back until [`Mock::release`], so a test can see
    /// which requests are in flight together without relying on timing.
    pub fn hold(&self) {
        self.open.send_replace(false);
    }

    /// Lets held and later requests be answered.
    pub fn release(&self) {
        self.open.send_replace(true);
    }

    /// The most requests the mock was answering at the same time.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// Every request body received so far, as text.
    pub fn bodies(&self) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|s| s.body.to_string()).collect()
    }

    /// The only request the mock received.
    pub fn only(&self) -> Seen {
        let mut seen = self.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "expected exactly one request");
        seen.pop().unwrap()
    }
}

async fn handler(
    State(script): State<Script>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    script.seen.lock().unwrap().push(Seen {
        method,
        path: uri.path().to_owned(),
        headers,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });
    let reply = {
        let mut replies = script.replies.lock().unwrap();
        // Replies are served in order; the last one repeats.
        if replies.len() > 1 {
            replies.remove(0)
        } else {
            replies[0].clone()
        }
    };
    let now = script.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    script.max_in_flight.fetch_max(now, Ordering::SeqCst);
    let mut open = script.open.clone();
    let _ = tokio::time::timeout(HOLD_LIMIT, open.wait_for(|open| *open)).await;
    tokio::time::sleep(reply.delay).await;
    script.in_flight.fetch_sub(1, Ordering::SeqCst);
    let mut response = (StatusCode::from_u16(reply.status).unwrap(), reply.body).into_response();
    for (name, value) in reply.headers {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    response
}

/// Starts a mock provider on a loopback port. Replies are served in order; the
/// last one repeats.
pub async fn serve(replies: Vec<Reply>) -> Mock {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let (open, open_rx) = watch::channel(true);
    let script = Script {
        replies: Arc::new(Mutex::new(replies)),
        seen: seen.clone(),
        in_flight: in_flight.clone(),
        max_in_flight: max_in_flight.clone(),
        open: open_rx,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let app = Router::new()
        .fallback(handler)
        .layer(DefaultBodyLimit::disable())
        .with_state(script);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Mock {
        url,
        seen,
        in_flight,
        max_in_flight,
        open,
    }
}
