//! Correlation IDs and the allowlist for production tracing. Request paths,
//! headers, error payloads and dependency diagnostics can contain private data.

use std::future::Future;

use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderValue, Method},
    middleware::Next,
    response::Response,
};
use tracing::{Instrument, Span};
use tracing_subscriber::{
    EnvFilter, Layer, filter::filter_fn, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};
use uuid::Uuid;

use crate::metrics::Metrics;

tokio::task_local! {
    static REQUEST_ID: Uuid;
}

/// ID for durable events written by this request or background job.
pub fn request_id() -> Option<Uuid> {
    REQUEST_ID.try_with(|id| *id).ok()
}

pub fn init_logging(json: bool) {
    let allowlist = || {
        filter_fn(|metadata| {
            let target = metadata.target();
            target == "tikkit_api" || target.starts_with("tikkit_api::")
        })
    };
    // Keep span context on errors when the operator raises the event threshold.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into())
        .add_directive(
            "tikkit_api::telemetry::span=info"
                .parse()
                .expect("fixed log directive"),
        );
    let subscriber = tracing_subscriber::registry().with(filter);
    if json {
        subscriber
            .with(
                fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_filter(allowlist()),
            )
            .init();
    } else {
        subscriber
            .with(fmt::layer().with_ansi(false).with_filter(allowlist()))
            .init();
    }
    // The default panic hook prints the payload even if CatchPanicLayer catches
    // the unwind. The payload may include a value from a request or dependency.
    std::panic::set_hook(Box::new(|_| tracing::error!("process task panicked")));
}

pub(crate) async fn http_request(
    State(metrics): State<Metrics>,
    mut request: Request,
    next: Next,
) -> Response {
    let id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() == 36)
        .and_then(|value| Uuid::try_parse(value).ok())
        .filter(|id| id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122)
        .unwrap_or_else(Uuid::new_v4);
    request.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID is an ASCII header"),
    );
    let start = std::time::Instant::now();
    let response = REQUEST_ID.scope(id, next.run(request)).await;
    metrics.request(response.status(), start.elapsed());
    response
}

pub(crate) fn request_span(request: &Request) -> Span {
    let method = match *request.method() {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "other",
    };
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok());
    tracing::info_span!(target: "tikkit_api::telemetry::span", "request", method, route, request_id)
}

pub(crate) fn request_completed(response: &Response, latency: std::time::Duration, _: &Span) {
    let status = response.status();
    let latency_ms = latency.as_millis() as u64;
    if status.is_server_error() {
        tracing::error!(status = status.as_u16(), latency_ms, "API request failed");
    } else {
        tracing::info!(
            status = status.as_u16(),
            latency_ms,
            "API request completed"
        );
    }
}

pub(crate) async fn job<T>(
    worker: &'static str,
    origin: Option<Uuid>,
    work: impl Future<Output = T>,
) -> T {
    let id = origin.unwrap_or_else(Uuid::new_v4);
    let span =
        tracing::info_span!(target: "tikkit_api::telemetry::span", "job", worker, request_id = %id);
    REQUEST_ID.scope(id, work.instrument(span)).await
}
