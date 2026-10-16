//! Cross-site request forgery protection.
//!
//! Every unsafe request (`POST`, `PUT`, `PATCH`, `DELETE`, and any other
//! non-safe method) to `/api/v1/*` must come from `PUBLIC_ORIGIN`:
//!
//! 1. If `Origin` is present it must equal `PUBLIC_ORIGIN` (`Origin: null`
//!    is rejected).
//! 2. Otherwise the origin of `Referer` must equal `PUBLIC_ORIGIN`.
//! 3. A request with neither header is rejected.
//!
//! Rejections are `403 csrf_rejected`. Safe methods (`GET`, `HEAD`,
//! `OPTIONS`, `TRACE`) and the unversioned probes are not checked. Browsers
//! always send `Origin` on same-origin `fetch` POSTs; non-browser API clients
//! must send `Origin: <PUBLIC_ORIGIN>` themselves. Together with
//! `SameSite=Lax` cookies this is defence in depth.

use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use url::Url;

use crate::{AppState, error::ApiError};

pub const PROTECTED_PREFIX: &str = "/api/v1/";

pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if needs_check(request.method(), request.uri().path())
        && !origin_allowed(request.headers(), &state.config.public_origin)
    {
        tracing::warn!(
            method = %request.method(),
            path = request.uri().path(),
            "rejected cross-origin or origin-less unsafe request"
        );
        return ApiError::csrf_rejected().into_response();
    }
    next.run(request).await
}

fn needs_check(method: &Method, path: &str) -> bool {
    let safe = matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    );
    !safe && (path.starts_with(PROTECTED_PREFIX) || path == "/api/v1")
}

/// Whether the request's `Origin` (or, without it, `Referer`) is `expected`
/// (a serialized origin such as `https://help.example.com`).
pub fn origin_allowed(headers: &HeaderMap, expected: &str) -> bool {
    let header_origin = |name| {
        headers
            .get(name)
            .map(|v| v.to_str().ok().and_then(serialized_origin))
    };
    match header_origin(header::ORIGIN).or_else(|| header_origin(header::REFERER)) {
        Some(Some(origin)) => origin == expected,
        _ => false,
    }
}

fn serialized_origin(value: &str) -> Option<String> {
    let url = Url::parse(value.trim()).ok()?;
    let origin = url.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(header::HeaderName, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name.clone(), HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn origin_rules() {
        let expected = "https://help.example.test";
        assert!(origin_allowed(
            &headers(&[(header::ORIGIN, "https://help.example.test")]),
            expected
        ));
        assert!(origin_allowed(
            &headers(&[(header::ORIGIN, "https://HELP.example.test:443")]),
            expected
        ));
        assert!(!origin_allowed(
            &headers(&[(header::ORIGIN, "https://evil.test")]),
            expected
        ));
        assert!(!origin_allowed(
            &headers(&[(header::ORIGIN, "null")]),
            expected
        ));
        assert!(!origin_allowed(
            &headers(&[(header::ORIGIN, "http://help.example.test")]),
            expected
        ));
        // Origin wins over Referer.
        assert!(!origin_allowed(
            &headers(&[
                (header::ORIGIN, "https://evil.test"),
                (header::REFERER, "https://help.example.test/login")
            ]),
            expected
        ));
        assert!(origin_allowed(
            &headers(&[(header::REFERER, "https://help.example.test/login?x=1")]),
            expected
        ));
        assert!(!origin_allowed(
            &headers(&[(header::REFERER, "https://evil.test/")]),
            expected
        ));
        assert!(!origin_allowed(&HeaderMap::new(), expected));
    }

    #[test]
    fn only_unsafe_v1_requests_are_checked() {
        assert!(needs_check(&Method::POST, "/api/v1/auth/login"));
        assert!(needs_check(&Method::DELETE, "/api/v1/x"));
        assert!(!needs_check(&Method::GET, "/api/v1/auth/session"));
        assert!(!needs_check(&Method::POST, "/api/health"));
    }
}
