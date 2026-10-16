//! Cross-cutting HTTP contract layers shared by every API route.

use std::any::Any;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    response::{IntoResponse, Response},
};
use tower_http::catch_panic::CatchPanicLayer;

use crate::{error::ApiError, extract::DEFAULT_BODY_LIMIT, request_id};

/// Applies the API contract to a fully assembled router:
///
/// - JSON `not_found` fallback for unknown routes and JSON
///   `method_not_allowed` for known routes with the wrong method,
/// - 64 KiB default body limit (`payload_too_large` via the extractors),
/// - panics become `internal_error` (details only in the log),
/// - request ids (outermost, so all of the above carry the id).
///
/// Call this **last**, after all routes are merged/nested; routes added
/// afterwards would not get these layers. Generic over the router state.
pub fn apply_api_layers<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT))
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(middleware::from_fn(request_id::middleware))
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

fn panic_response(panic: Box<dyn Any + Send + 'static>) -> Response {
    let detail = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned());
    ApiError::internal(format!("handler panicked: {detail}")).into_response()
}
