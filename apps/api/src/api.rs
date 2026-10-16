//! HTTP API conventions shared by every route: error envelope, JSON/path/query
//! extractors that map rejections into it, request IDs, and cursor pagination.
//! See docs/api-conventions.md.

use std::collections::BTreeMap;

use axum::{
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Request, rejection::BytesRejection},
    http::{
        HeaderMap, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub const MAX_JSON_BODY: usize = 1024 * 1024;

/// Field name -> stable field error codes (`required`, `invalid`, `unknown_field`, ...).
pub type FieldErrors = BTreeMap<String, Vec<&'static str>>;

#[derive(Clone, Debug)]
pub enum ApiError {
    Validation(FieldErrors),
    InvalidJson,
    UnknownField(String),
    InvalidIdentifier,
    InvalidQuery,
    BadRequest,
    UnsupportedMediaType,
    Unauthorized,
    /// Unknown username, wrong password, or deactivated account: deliberately indistinguishable.
    InvalidCredentials,
    /// State-changing request without the `X-Requested-With: tikkit` header.
    CsrfRejected,
    /// Too many attempts; retry after this many seconds.
    RateLimited(u64),
    SetupAlreadyCompleted,
    /// Invitation token unknown, expired, revoked, or already used.
    InvitationInvalid,
    /// Password reset token unknown, expired, or already used.
    ResetTokenInvalid,
    /// The change would leave no active Admin.
    LastActiveAdmin,
    /// Only where the resource's existence is already known to the caller;
    /// otherwise deny with `NotFound` so existence does not leak.
    Forbidden,
    NotFound,
    MethodNotAllowed,
    Conflict,
    PayloadTooLarge,
    Internal,
}

/// Stable machine-readable error codes. Never translated; clients map them to text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ValidationFailed,
    InvalidJson,
    UnknownField,
    InvalidIdentifier,
    InvalidQuery,
    BadRequest,
    UnsupportedMediaType,
    Unauthorized,
    InvalidCredentials,
    CsrfRejected,
    RateLimited,
    SetupAlreadyCompleted,
    InvitationInvalid,
    ResetTokenInvalid,
    LastActiveAdmin,
    Forbidden,
    NotFound,
    MethodNotAllowed,
    Conflict,
    PayloadTooLarge,
    Internal,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
    /// Same value as the `x-request-id` response header.
    pub request_id: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// Safe English fallback text; translate by `code` instead of showing this.
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<ErrorDetails>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ErrorDetails {
    /// Invalid input fields (dotted paths) and their stable error codes.
    pub fields: BTreeMap<String, Vec<String>>,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, ErrorCode, &'static str) {
        use ApiError::*;
        match self {
            Validation(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::ValidationFailed,
                "Some fields are invalid.",
            ),
            InvalidJson => (
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidJson,
                "The request body is not valid JSON.",
            ),
            UnknownField(_) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::UnknownField,
                "The request contains an unknown field.",
            ),
            InvalidIdentifier => (
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidIdentifier,
                "An identifier in the URL is malformed.",
            ),
            InvalidQuery => (
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidQuery,
                "The query parameters are invalid.",
            ),
            BadRequest => (
                StatusCode::BAD_REQUEST,
                ErrorCode::BadRequest,
                "The request is malformed.",
            ),
            UnsupportedMediaType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                ErrorCode::UnsupportedMediaType,
                "Send the body as application/json.",
            ),
            Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthorized,
                "Sign in to continue.",
            ),
            InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                ErrorCode::InvalidCredentials,
                "The username or password is incorrect.",
            ),
            CsrfRejected => (
                StatusCode::FORBIDDEN,
                ErrorCode::CsrfRejected,
                "Missing the X-Requested-With: tikkit header.",
            ),
            RateLimited(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::RateLimited,
                "Too many attempts; try again later.",
            ),
            SetupAlreadyCompleted => (
                StatusCode::CONFLICT,
                ErrorCode::SetupAlreadyCompleted,
                "Setup has already been completed.",
            ),
            InvitationInvalid => (
                StatusCode::GONE,
                ErrorCode::InvitationInvalid,
                "This invitation link is invalid, expired, revoked, or already used.",
            ),
            ResetTokenInvalid => (
                StatusCode::GONE,
                ErrorCode::ResetTokenInvalid,
                "This password reset link is invalid, expired, or already used.",
            ),
            LastActiveAdmin => (
                StatusCode::CONFLICT,
                ErrorCode::LastActiveAdmin,
                "The last active administrator cannot be deactivated.",
            ),
            Forbidden => (
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "You are not allowed to do this.",
            ),
            NotFound => (
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
                "The requested resource was not found.",
            ),
            MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                ErrorCode::MethodNotAllowed,
                "This method is not allowed here.",
            ),
            Conflict => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "The resource changed; reload and try again.",
            ),
            PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorCode::PayloadTooLarge,
                "The request body is too large.",
            ),
            Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "Something went wrong on our side.",
            ),
        }
    }

    fn from_status(status: StatusCode) -> Self {
        match status {
            StatusCode::NOT_FOUND => Self::NotFound,
            StatusCode::METHOD_NOT_ALLOWED => Self::MethodNotAllowed,
            StatusCode::PAYLOAD_TOO_LARGE => Self::PayloadTooLarge,
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::UnsupportedMediaType,
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            StatusCode::FORBIDDEN => Self::Forbidden,
            StatusCode::CONFLICT => Self::Conflict,
            s if s.is_client_error() => Self::BadRequest,
            _ => Self::Internal,
        }
    }

    fn render(&self, request_id: String) -> Response {
        let (status, code, message) = self.parts();
        let fields = match self {
            Self::Validation(fields) => Some(fields.clone()),
            Self::UnknownField(name) => {
                Some(BTreeMap::from([(name.clone(), vec!["unknown_field"])]))
            }
            _ => None,
        };
        let details = fields.map(|f| ErrorDetails {
            fields: f
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(Into::into).collect()))
                .collect(),
        });
        let error = ErrorBody {
            code,
            message: message.into(),
            details,
        };
        let mut response =
            (status, axum::Json(ErrorEnvelope { error, request_id })).into_response();
        if let Self::RateLimited(secs) = self {
            response.headers_mut().insert(RETRY_AFTER, (*secs).into());
        }
        response
    }
}

impl IntoResponse for ApiError {
    /// The body is rendered by [`error_envelope`], which knows the request ID.
    fn into_response(self) -> Response {
        let mut response = self.parts().0.into_response();
        response.extensions_mut().insert(self);
        response
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "database error");
        Self::Internal
    }
}

/// Middleware rendering every error response as the JSON envelope, including
/// framework responses (unknown route, wrong method) that never saw an `ApiError`.
pub async fn error_envelope(request: Request, next: Next) -> Response {
    let request_id = request_id(request.headers());
    let mut response = next.run(request).await;
    let error = match response.extensions_mut().remove::<ApiError>() {
        Some(error) => error,
        None => {
            let status = response.status();
            let is_json = response
                .headers()
                .get(CONTENT_TYPE)
                .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
            if !(status.is_client_error() || status.is_server_error()) || is_json {
                return response;
            }
            ApiError::from_status(status)
        }
    };
    // Keep headers such as `Allow` (405) or `WWW-Authenticate`; only the body changes.
    let mut rendered = error.render(request_id);
    for (name, value) in response.headers() {
        if name != CONTENT_TYPE && name != CONTENT_LENGTH {
            rendered.headers_mut().append(name, value.clone());
        }
    }
    rendered
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// JSON body extractor/response. Requires `application/json`, rejects unknown
/// fields on types marked `#[serde(deny_unknown_fields)]`, and maps every
/// failure to the error envelope with the offending field path.
pub struct Json<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Json<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let json = request
            .headers()
            .get(CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
        if !json {
            return Err(ApiError::UnsupportedMediaType);
        }
        let bytes =
            Bytes::from_request(request, state)
                .await
                .map_err(|rejection: BytesRejection| match rejection.status() {
                    StatusCode::PAYLOAD_TOO_LARGE => ApiError::PayloadTooLarge,
                    _ => ApiError::InvalidJson,
                })?;
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        let value = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
            let path = error.path().to_string();
            let error = error.into_inner();
            if !error.is_data() {
                return ApiError::InvalidJson;
            }
            let message = error.to_string();
            let field = |name: &str| match path.as_str() {
                "." => name.to_owned(),
                parent => format!("{parent}.{name}"),
            };
            // serde_path_to_error's path already ends in the unknown key.
            if message.starts_with("unknown field `") {
                ApiError::UnknownField(path)
            } else if let Some(name) = backticked(&message, "missing field `") {
                ApiError::Validation(FieldErrors::from([(field(name), vec!["required"])]))
            } else if path == "." {
                ApiError::InvalidJson
            } else {
                ApiError::Validation(FieldErrors::from([(path, vec!["invalid"])]))
            }
        })?;
        deserializer.end().map_err(|_| ApiError::InvalidJson)?;
        Ok(Self(value))
    }
}

fn backticked<'a>(message: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = &message[message.find(prefix)? + prefix.len()..];
    Some(&rest[..rest.find('`')?])
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Path extractor: malformed identifiers (e.g. a bad UUID) become `invalid_identifier`.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct Path<T>(pub T);

impl From<axum::extract::rejection::PathRejection> for ApiError {
    fn from(_: axum::extract::rejection::PathRejection) -> Self {
        Self::InvalidIdentifier
    }
}

/// Query extractor: unparsable parameters become `invalid_query`.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Query<T>(pub T);

impl From<axum::extract::rejection::QueryRejection> for ApiError {
    fn from(_: axum::extract::rejection::QueryRejection) -> Self {
        Self::InvalidQuery
    }
}

/// `?limit=&cursor=` for every list endpoint. Lists are ordered newest first by
/// `(created_at, id)`; the cursor is opaque to clients.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Pagination {
    /// Page size, 1-100 (default 25). Out-of-range values are clamped.
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub created_at: OffsetDateTime,
    pub id: Uuid,
}

impl Pagination {
    pub const DEFAULT_LIMIT: i64 = 25;
    pub const MAX_LIMIT: i64 = 100;

    pub fn limit(&self) -> i64 {
        self.limit
            .unwrap_or(Self::DEFAULT_LIMIT)
            .clamp(1, Self::MAX_LIMIT)
    }

    /// Decoded position after which the next page starts, if any.
    pub fn cursor(&self) -> Result<Option<Cursor>, ApiError> {
        let Some(raw) = &self.cursor else {
            return Ok(None);
        };
        let invalid = || {
            ApiError::Validation(FieldErrors::from([(
                "cursor".into(),
                vec!["invalid_cursor"],
            )]))
        };
        let text = URL_SAFE_NO_PAD
            .decode(raw)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(invalid)?;
        let (nanos, id) = text.split_once('|').ok_or_else(invalid)?;
        let created_at = nanos
            .parse()
            .ok()
            .and_then(|n| OffsetDateTime::from_unix_timestamp_nanos(n).ok())
            .ok_or_else(invalid)?;
        Ok(Some(Cursor {
            created_at,
            id: id.parse().map_err(|_| invalid())?,
        }))
    }
}

impl Cursor {
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(format!(
            "{}|{}",
            self.created_at.unix_timestamp_nanos(),
            self.id
        ))
    }
}

/// One page of a list. `next_cursor` is null on the last page.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Build from rows fetched with `LIMIT limit + 1`; the extra row only signals another page.
    pub fn from_rows(mut rows: Vec<T>, limit: i64, key: impl Fn(&T) -> Cursor) -> Self {
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        let next_cursor = more.then(|| rows.last().map(|r| key(r).encode())).flatten();
        Self {
            items: rows,
            next_cursor,
        }
    }
}

/// Machine identifiers published in the contract; display text is localized by clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    En,
    De,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Operator,
    Specialist,
    TeamLead,
    Admin,
}

impl Role {
    pub fn is_staff(self) -> bool {
        self != Self::User
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    Draft,
    AiProcessing,
    AwaitingAiDecision,
    Queued,
    InProgress,
    WaitingForRequester,
    AwaitingConfirmation,
    Closed,
    Cancelled,
    AiSolved,
}

/// Public handling details shown to a ticket's requester. `team_name: null`
/// means the shared general queue; `assignee_display_name: null` means unassigned.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TicketAssignmentView {
    #[schema(required = true)]
    pub team_name: Option<String>,
    #[schema(required = true)]
    pub assignee_display_name: Option<String>,
}
