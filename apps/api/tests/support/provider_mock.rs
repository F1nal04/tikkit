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
}

pub struct Mock {
    pub url: Url,
    seen: Arc<Mutex<Vec<Seen>>>,
    max_in_flight: Arc<AtomicUsize>,
}

impl Mock {
    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
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
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let script = Script {
        replies: Arc::new(Mutex::new(replies)),
        seen: seen.clone(),
        in_flight: Arc::default(),
        max_in_flight: max_in_flight.clone(),
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
        max_in_flight,
    }
}
