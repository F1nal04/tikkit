//! The single error type returned by every API handler.
//!
//! Every non-2xx response produced by this crate has the body
//!
//! ```json
//! {"error":{"code":"validation_failed","message":"…","requestId":"…",
//!           "fields":[{"field":"username","code":"too_short","params":{"min":3}}]}}
//! ```
//!
//! `code` is a stable machine identifier (the frontend translates by code),
//! `message` is a safe English fallback that never contains internal details,
//! `requestId` matches the `x-request-id` response header and the server logs,
//! and `fields` is omitted when empty.

use std::{borrow::Cow, collections::BTreeMap, fmt};

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::request_id;

/// Stable, language-independent error codes. Serialized as snake_case.
///
/// Adding a variant is a contract change: update `openapi.json`
/// (`UPDATE_OPENAPI=1 cargo test --locked -p tikkit-api openapi`), regenerate
/// the web client and add English and German messages in
/// `apps/web/src/api/errors.ts` (the TypeScript build fails until you do).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 400: malformed syntax, wrong content type, unknown or mistyped fields.
    InvalidRequest,
    /// 422: well-formed request whose values fail validation (see `fields`).
    ValidationFailed,
    /// 401: no valid session.
    Unauthenticated,
    /// 403: authenticated but not allowed.
    Forbidden,
    /// 403: unsafe request failed the CSRF/origin check.
    CsrfRejected,
    /// 404: unknown route, invalid identifier, or a resource the actor may not see.
    NotFound,
    /// 405: the route exists but not for this HTTP method.
    MethodNotAllowed,
    /// 409: the request conflicts with current state (e.g. a concurrent update won).
    Conflict,
    /// 413: request body exceeds the limit.
    PayloadTooLarge,
    /// 429: too many requests.
    RateLimited,
    /// 500: unexpected failure; details are only in the server log.
    InternalError,
    /// 503: a dependency (e.g. the database) is unavailable.
    ServiceUnavailable,
}

impl ErrorCode {
    pub const fn status(self) -> StatusCode {
        match self {
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::ValidationFailed => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::CsrfRejected => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::Conflict => StatusCode::CONFLICT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// Safe English default message. Clients should translate by `code`.
    pub const fn default_message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "The request is malformed.",
            Self::ValidationFailed => "Some fields are invalid.",
            Self::Unauthenticated => "You need to sign in.",
            Self::Forbidden => "You are not allowed to do this.",
            Self::CsrfRejected => "The request was rejected by cross-site request protection.",
            Self::NotFound => "The requested resource was not found.",
            Self::MethodNotAllowed => "This method is not allowed for this resource.",
            Self::Conflict => "The request conflicts with the current state.",
            Self::PayloadTooLarge => "The request body is too large.",
            Self::RateLimited => "Too many requests. Try again later.",
            Self::InternalError => "An unexpected error occurred.",
            Self::ServiceUnavailable => "The service is temporarily unavailable.",
        }
    }
}

/// Stable per-field validation codes. Serialized as snake_case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldErrorCode {
    /// The field is missing or empty.
    Required,
    /// Shorter than `params.min` characters/items.
    TooShort,
    /// Longer than `params.max` characters/items.
    TooLong,
    /// Number outside `params.min`..=`params.max`.
    OutOfRange,
    /// Does not match the expected format (e.g. allowed characters).
    InvalidFormat,
    /// Not one of the allowed values, or otherwise unacceptable.
    InvalidValue,
    /// JSON/query value has the wrong type (e.g. string instead of number).
    InvalidType,
    /// Field is not part of this request's schema.
    UnknownField,
    /// Value is already taken (e.g. a normalized username).
    AlreadyTaken,
}

/// One invalid field. `field` uses the request's camelCase names with `.`
/// for nesting and `[n]` for array items (e.g. `screenshots[0].name`).
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
pub struct FieldError {
    pub field: String,
    pub code: FieldErrorCode,
    /// Interpolation values for the translated message (e.g. `{"min": 3}`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<&'static str, serde_json::Value>,
}

impl FieldError {
    pub fn new(field: impl Into<String>, code: FieldErrorCode) -> Self {
        Self {
            field: field.into(),
            code,
            params: BTreeMap::new(),
        }
    }

    pub fn with_param(mut self, name: &'static str, value: impl Into<serde_json::Value>) -> Self {
        self.params.insert(name, value.into());
        self
    }
}

/// Response body wrapper for every error: `{"error": {...}}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    pub code: ErrorCode,
    /// Safe English fallback message. Never contains internal details.
    pub message: String,
    /// Same value as the `x-request-id` response header.
    pub request_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldError>,
}

/// Error returned by handlers (`Result<T, ApiError>`).
///
/// Messages must be safe to show to any caller: never put user input,
/// identifiers of resources the actor may not see, SQL, or secrets in them.
/// Use [`ApiError::internal`] for unexpected failures; its detail is logged
/// together with the request id and never sent to the client.
pub struct ApiError {
    code: ErrorCode,
    message: Cow<'static, str>,
    fields: Vec<FieldError>,
    retry_after_secs: Option<u64>,
    /// Logged on render, never serialized.
    internal_detail: Option<String>,
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            message: Cow::Borrowed(code.default_message()),
            fields: Vec::new(),
            retry_after_secs: None,
            internal_detail: None,
        }
    }

    /// Replace the default message with another *safe* English message.
    pub fn with_message(mut self, message: impl Into<Cow<'static, str>>) -> Self {
        self.message = message.into();
        self
    }

    pub fn with_fields(mut self, fields: Vec<FieldError>) -> Self {
        self.fields = fields;
        self
    }

    /// Adds a `Retry-After` header (useful with `rate_limited` / `service_unavailable`).
    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn fields(&self) -> &[FieldError] {
        &self.fields
    }

    pub fn invalid_request() -> Self {
        Self::new(ErrorCode::InvalidRequest)
    }

    pub fn validation(fields: Vec<FieldError>) -> Self {
        Self::new(ErrorCode::ValidationFailed).with_fields(fields)
    }

    /// Shorthand for a single invalid field.
    pub fn field(field: impl Into<String>, code: FieldErrorCode) -> Self {
        Self::validation(vec![FieldError::new(field, code)])
    }

    pub fn unauthenticated() -> Self {
        Self::new(ErrorCode::Unauthenticated)
    }

    pub fn forbidden() -> Self {
        Self::new(ErrorCode::Forbidden)
    }

    pub fn csrf_rejected() -> Self {
        Self::new(ErrorCode::CsrfRejected)
    }

    /// Use for missing resources *and* for resources the actor may not see,
    /// so existence is never leaked.
    pub fn not_found() -> Self {
        Self::new(ErrorCode::NotFound)
    }

    pub fn method_not_allowed() -> Self {
        Self::new(ErrorCode::MethodNotAllowed)
    }

    pub fn conflict() -> Self {
        Self::new(ErrorCode::Conflict)
    }

    pub fn payload_too_large() -> Self {
        Self::new(ErrorCode::PayloadTooLarge)
    }

    pub fn rate_limited() -> Self {
        Self::new(ErrorCode::RateLimited)
    }

    pub fn service_unavailable() -> Self {
        Self::new(ErrorCode::ServiceUnavailable)
    }

    /// Unexpected failure. `detail` goes to the log (with the request id)
    /// and is never returned to the client. Do not pass secrets.
    pub fn internal(detail: impl fmt::Display) -> Self {
        let mut error = Self::new(ErrorCode::InternalError);
        error.internal_detail = Some(detail.to_string());
        error
    }
}

/// Database failures are never shown to clients: they are logged with the
/// request id and surface as `internal_error`. Handlers that expect a
/// specific constraint violation (e.g. a unique username) must match it
/// before falling back to this conversion.
impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(error)
    }
}

impl fmt::Debug for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiError")
            .field("code", &self.code)
            .field("message", &self.message)
            .field("fields", &self.fields)
            .field("internal_detail", &self.internal_detail)
            .finish()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = request_id::current_or_new();
        let status = self.code.status();

        if let Some(detail) = &self.internal_detail {
            tracing::error!(%request_id, code = ?self.code, detail = %detail, "request failed");
        } else if status.is_server_error() {
            tracing::error!(%request_id, code = ?self.code, "request failed");
        } else {
            tracing::debug!(%request_id, code = ?self.code, "request rejected");
        }

        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code,
                message: self.message.into_owned(),
                request_id,
                fields: self.fields,
            },
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(secs) = self.retry_after_secs {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn render(error: ApiError) -> (StatusCode, serde_json::Value) {
        let response = request_id::scope("req-1".into(), async { error.into_response() }).await;
        let status = response.status();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn renders_contract_body_without_empty_fields() {
        let (status, body) = render(ApiError::not_found()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            serde_json::json!({"error": {
                "code": "not_found",
                "message": "The requested resource was not found.",
                "requestId": "req-1"
            }})
        );
    }

    #[tokio::test]
    async fn renders_field_errors_with_params() {
        let error = ApiError::validation(vec![
            FieldError::new("username", FieldErrorCode::TooShort).with_param("min", 3),
        ]);
        let (status, body) = render(error).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["error"]["fields"],
            serde_json::json!([{"field": "username", "code": "too_short", "params": {"min": 3}}])
        );
    }

    #[tokio::test]
    async fn internal_error_hides_detail() {
        let (status, body) = render(ApiError::internal("password=hunter2 at db.rs:12")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "internal_error");
        assert!(!body.to_string().contains("hunter2"));
    }
}
