//! Extractors with contract-conforming rejections, plus field validation.
//!
//! | Extractor          | Rejection                                                      |
//! |--------------------|----------------------------------------------------------------|
//! | [`ApiJson<T>`]     | 400 `invalid_request` (wrong content type, malformed JSON, unknown/missing/mistyped fields, with `fields` when the location is known); 413 `payload_too_large` |
//! | [`ValidJson<T>`]   | as `ApiJson`, then 422 `validation_failed` from [`Validate`]   |
//! | [`ApiPath<T>`]     | 404 `not_found` for any unparsable path value (e.g. a bad UUID) |
//! | [`ApiQuery<T>`]    | 400 `invalid_request` (unknown/mistyped params)                |
//!
//! Request DTOs must use `#[serde(deny_unknown_fields)]` (bodies and query
//! structs alike) so unknown inputs are rejected rather than ignored.

use axum::{
    Json,
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Path, Request, rejection::PathRejection},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::error::{ApiError, FieldError, FieldErrorCode};

/// Default maximum request body size (64 KiB). Override per route with
/// `axum::extract::DefaultBodyLimit::max(..)` where larger bodies are needed.
pub const DEFAULT_BODY_LIMIT: usize = 64 * 1024;

/// JSON body extractor (requires `content-type: application/json`) and JSON
/// response wrapper.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json_content_type(request.headers()) {
            return Err(ApiError::invalid_request()
                .with_message("Expected a JSON request body (content-type: application/json)."));
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| {
                if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    ApiError::payload_too_large()
                } else {
                    ApiError::invalid_request().with_message("The request body could not be read.")
                }
            })?;
        parse_json(&bytes).map(Self)
    }
}

impl<T: Serialize> IntoResponse for ApiJson<T> {
    fn into_response(self) -> Response {
        Json(self.0).into_response()
    }
}

/// Parses a JSON document into `T`, mapping errors to `invalid_request`.
pub fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        if error.inner().is_data() {
            deserialize_error(&error.path().to_string(), &error.inner().to_string())
        } else {
            ApiError::invalid_request().with_message("The request body is not valid JSON.")
        }
    })?;
    deserializer.end().map_err(|_| {
        ApiError::invalid_request().with_message("The request body is not valid JSON.")
    })?;
    Ok(value)
}

/// JSON body that is additionally checked with [`Validate`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ValidJson<T>(pub T);

impl<T, S> FromRequest<S> for ValidJson<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let ApiJson(value) = ApiJson::<T>::from_request(request, state).await?;
        value.validate()?;
        Ok(Self(value))
    }
}

/// Path extractor. Any value that does not parse (e.g. a malformed UUID) is a
/// 404 `not_found`, exactly like an unknown or invisible resource.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiPath<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Self(value)),
            Err(PathRejection::FailedToDeserializePathParams(_)) => Err(ApiError::not_found()),
            Err(other) => Err(ApiError::internal(format!("path extraction: {other}"))),
        }
    }
}

/// Query-string extractor. Use a struct with `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        let deserializer =
            serde_urlencoded::Deserializer::new(form_urlencoded::parse(query.as_bytes()));
        serde_path_to_error::deserialize(deserializer)
            .map(Self)
            .map_err(|error| {
                deserialize_error(&error.path().to_string(), &error.inner().to_string())
            })
    }
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let essence = value.split(';').next().unwrap_or_default().trim();
    essence.eq_ignore_ascii_case("application/json")
}

/// Maps a serde data error (standard serde messages) to `invalid_request`
/// with a field entry when the location is known. The raw serde message is
/// never returned. `path` is the serde_path_to_error location: it ends at
/// the offending key for unknown fields and at the parent for missing ones.
fn deserialize_error(path: &str, message: &str) -> ApiError {
    let parent = if path == "." { "" } else { path };
    let join = |name: &str| {
        if parent.is_empty() {
            name.to_owned()
        } else {
            format!("{parent}.{name}")
        }
    };
    let backticked = || message.split('`').nth(1).unwrap_or_default();

    let field = if message.starts_with("unknown field") {
        // The path already ends at the unknown key.
        let field = if parent.is_empty() {
            backticked().to_owned()
        } else {
            parent.to_owned()
        };
        Some(FieldError::new(field, FieldErrorCode::UnknownField))
    } else if message.starts_with("missing field") {
        Some(FieldError::new(
            join(backticked()),
            FieldErrorCode::Required,
        ))
    } else if parent.is_empty() {
        None
    } else if message.starts_with("invalid type") {
        Some(FieldError::new(parent, FieldErrorCode::InvalidType))
    } else {
        Some(FieldError::new(parent, FieldErrorCode::InvalidValue))
    };

    let error = ApiError::invalid_request();
    match field {
        Some(field) => error.with_fields(vec![field]),
        None => error,
    }
}

/// Field validation for request DTOs. Implement with [`FieldErrors`]:
///
/// ```
/// use tikkit_api::extract::{FieldErrors, Validate};
/// use tikkit_api::error::ApiError;
///
/// struct CreateTeam { name: String }
///
/// impl Validate for CreateTeam {
///     fn validate(&self) -> Result<(), ApiError> {
///         let mut errors = FieldErrors::new();
///         errors.length("name", self.name.trim(), 1, 80);
///         errors.finish()
///     }
/// }
/// ```
pub trait Validate {
    fn validate(&self) -> Result<(), ApiError>;
}

/// Collects field errors; [`FieldErrors::finish`] turns them into a 422.
#[derive(Debug, Default)]
pub struct FieldErrors(Vec<FieldError>);

impl FieldErrors {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, error: FieldError) -> &mut Self {
        self.0.push(error);
        self
    }

    pub fn add(&mut self, field: impl Into<String>, code: FieldErrorCode) -> &mut Self {
        self.push(FieldError::new(field, code))
    }

    /// Adds `code` for `field` unless `ok`.
    pub fn check(&mut self, ok: bool, field: impl Into<String>, code: FieldErrorCode) -> &mut Self {
        if !ok {
            self.add(field, code);
        }
        self
    }

    /// Character-count bounds: empty → `required` (when `min > 0`),
    /// short → `too_short {min}`, long → `too_long {max}`.
    pub fn length(&mut self, field: &str, value: &str, min: usize, max: usize) -> &mut Self {
        let len = value.chars().count();
        if len == 0 && min > 0 {
            self.add(field, FieldErrorCode::Required)
        } else if len < min {
            self.push(FieldError::new(field, FieldErrorCode::TooShort).with_param("min", min))
        } else if len > max {
            self.push(FieldError::new(field, FieldErrorCode::TooLong).with_param("max", max))
        } else {
            self
        }
    }

    /// Inclusive numeric range → `out_of_range {min, max}`.
    pub fn range(&mut self, field: &str, value: i64, min: i64, max: i64) -> &mut Self {
        if !(min..=max).contains(&value) {
            self.push(
                FieldError::new(field, FieldErrorCode::OutOfRange)
                    .with_param("min", min)
                    .with_param("max", max),
            );
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn finish(&mut self) -> Result<(), ApiError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ApiError::validation(std::mem::take(&mut self.0)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[allow(dead_code)]
    struct Body {
        display_name: String,
        inner: Option<Inner>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Inner {
        count: u32,
    }

    fn fields(result: Result<Body, ApiError>) -> Vec<(String, FieldErrorCode)> {
        let error = result.unwrap_err();
        assert_eq!(error.code(), crate::error::ErrorCode::InvalidRequest);
        error
            .fields()
            .iter()
            .map(|f| (f.field.clone(), f.code))
            .collect()
    }

    #[test]
    fn json_errors_point_at_fields() {
        assert_eq!(
            fields(parse_json(br#"{"displayName":"a","extra":1}"#)),
            vec![("extra".into(), FieldErrorCode::UnknownField)]
        );
        assert_eq!(
            fields(parse_json(br#"{}"#)),
            vec![("displayName".into(), FieldErrorCode::Required)]
        );
        assert_eq!(
            fields(parse_json(br#"{"displayName":1}"#)),
            vec![("displayName".into(), FieldErrorCode::InvalidType)]
        );
        assert_eq!(
            fields(parse_json(br#"{"displayName":"a","inner":{"count":-1}}"#)),
            vec![("inner.count".into(), FieldErrorCode::InvalidValue)]
        );
        assert_eq!(
            fields(parse_json(br#"{"displayName":"a","inner":{"nope":1}}"#)),
            vec![("inner.nope".into(), FieldErrorCode::UnknownField)]
        );
        assert!(fields(parse_json(br#"{"displayName":"a"} trailing"#)).is_empty());
        assert!(fields(parse_json(br#"{"displayName":"#)).is_empty());
    }

    #[test]
    fn field_errors_builder() {
        let mut errors = FieldErrors::new();
        errors
            .length("a", "", 1, 5)
            .length("b", "ab", 3, 5)
            .length("c", "abcdef", 1, 5)
            .length("d", "äöü", 3, 3)
            .range("e", 0, 1, 100)
            .check(false, "f", FieldErrorCode::InvalidFormat);
        let error = errors.finish().unwrap_err();
        let codes: Vec<_> = error
            .fields()
            .iter()
            .map(|f| (f.field.as_str(), f.code))
            .collect();
        assert_eq!(
            codes,
            vec![
                ("a", FieldErrorCode::Required),
                ("b", FieldErrorCode::TooShort),
                ("c", FieldErrorCode::TooLong),
                ("e", FieldErrorCode::OutOfRange),
                ("f", FieldErrorCode::InvalidFormat),
            ]
        );
        assert!(FieldErrors::new().finish().is_ok());
    }
}
