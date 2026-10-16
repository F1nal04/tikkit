//! Request IDs: every request gets one, it is echoed in the `x-request-id`
//! response header, attached to the request's tracing span, included in error
//! bodies, and available to handlers via the [`RequestId`] extension or
//! [`current`].
//!
//! An inbound `x-request-id` (e.g. from the reverse proxy) is kept only if it
//! is 1–64 characters of `[A-Za-z0-9._-]`; otherwise a UUID v7 is generated.

use std::future::Future;

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use tracing::Instrument;

pub const HEADER: HeaderName = HeaderName::from_static("x-request-id");
const MAX_LEN: usize = 64;

tokio::task_local! {
    static CURRENT: String;
}

/// Request extension holding the id of the current request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(pub String);

/// Whether an inbound id is safe to reuse (log/header injection safe).
pub fn is_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn generate() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// The id of the request being handled on this task, if any.
pub fn current() -> Option<String> {
    CURRENT.try_with(Clone::clone).ok()
}

/// The current request id, or a fresh one when called outside a request
/// (so error bodies always carry an id).
pub fn current_or_new() -> String {
    current().unwrap_or_else(generate)
}

/// Runs `fut` with `id` as the current request id.
pub async fn scope<F: Future>(id: String, fut: F) -> F::Output {
    CURRENT.scope(id, fut).await
}

/// Axum middleware (`axum::middleware::from_fn(request_id::middleware)`).
/// Must be the outermost API layer so everything inside sees the id.
pub async fn middleware(mut request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get(&HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_valid(value))
        .map(str::to_owned)
        .unwrap_or_else(generate);

    request.extensions_mut().insert(RequestId(id.clone()));
    let span = tracing::info_span!(
        "request",
        request_id = %id,
        method = %request.method(),
        path = %request.uri().path(),
    );

    let mut response = scope(id.clone(), next.run(request).instrument(span)).await;
    // `id` only contains header-safe characters, so this cannot fail.
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(HEADER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_inbound_ids() {
        assert!(is_valid("abc-123_DEF.4"));
        assert!(is_valid(&"a".repeat(64)));
        assert!(!is_valid(&"a".repeat(65)));
        assert!(!is_valid(""));
        assert!(!is_valid("has space"));
        assert!(!is_valid("new\nline"));
        assert!(!is_valid("ümlaut"));
    }

    #[test]
    fn generated_ids_are_valid() {
        assert!(is_valid(&generate()));
    }
}
