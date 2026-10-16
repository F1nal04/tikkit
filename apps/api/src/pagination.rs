//! Cursor pagination for list endpoints.
//!
//! # Conventions
//!
//! - **Request:** query params `limit` (integer 1..=100, default 25) and
//!   `cursor` (opaque string from a previous `nextCursor`). Invalid values →
//!   422 `validation_failed` on field `limit` / `cursor`.
//! - **Response:** [`Page<T>`] = `{"items": [...], "nextCursor": "…" | null}`.
//!   `nextCursor` is `null` on the last page. There are no total counts.
//! - **Ordering:** every list has one documented, stable order whose last key
//!   is the row id (tiebreak), e.g. `ORDER BY created_at DESC, id DESC`. The
//!   cursor encodes that key tuple of the last returned row; the next page is
//!   `WHERE (created_at, id) < ($1, $2)` (keyset pagination, no OFFSET).
//! - **Filtering:** filters are explicit, documented query params validated
//!   server-side; unknown params are rejected (`deny_unknown_fields` →
//!   400 `invalid_request`). Authorization scoping (requester ownership, team
//!   isolation) is applied in SQL *before* pagination, for items and cursors
//!   alike, never as a post-filter. A cursor carries no authority: a forged
//!   cursor can only move within rows the actor may already see.
//!
//! Endpoints without filters extract `ApiQuery<PageParams>` directly. Because
//! serde cannot combine `flatten` with `deny_unknown_fields`, filtered list
//! queries declare `limit` and `cursor` themselves (same types and
//! `#[param]` attributes as [`PageParams`]) and build a `PageParams` from them.
//!
//! ```
//! use tikkit_api::pagination::{Page, PageParams};
//!
//! // ApiQuery(params): ApiQuery<PageParams>
//! let params = PageParams { limit: Some("2".into()), cursor: None };
//! let request = params.validate::<(i64, String)>().unwrap();
//! // SELECT … WHERE (…) < $after ORDER BY created_at DESC, id DESC LIMIT request.fetch_limit()
//! let rows = vec![(3, "c"), (2, "b"), (1, "a")];
//! let page = Page::from_overfetched(rows, &request, |row| (row.0, row.1.to_owned()));
//! assert_eq!(page.items.len(), 2);
//! assert!(page.next_cursor.is_some());
//! ```

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use utoipa::{IntoParams, ToSchema};

use crate::error::{ApiError, FieldError, FieldErrorCode};

pub const DEFAULT_LIMIT: u32 = 25;
pub const MAX_LIMIT: u32 = 100;
/// Cursors longer than this are rejected without decoding.
pub const MAX_CURSOR_LEN: usize = 512;

/// Raw pagination query params. Kept as strings so that every bad value is
/// reported uniformly as `validation_failed`; see [`PageParams::validate`].
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct PageParams {
    /// Page size (1–100, default 25).
    #[param(value_type = Option<u32>, minimum = 1, maximum = 100)]
    pub limit: Option<String>,
    /// Opaque cursor from a previous response's `nextCursor`.
    pub cursor: Option<String>,
}

/// Validated pagination input; `K` is the endpoint's ordering key tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct PageRequest<K> {
    pub limit: u32,
    /// Key of the last row of the previous page, if any.
    pub after: Option<K>,
}

impl<K> PageRequest<K> {
    /// Rows to fetch: one more than `limit` to learn whether a next page exists.
    pub fn fetch_limit(&self) -> i64 {
        i64::from(self.limit) + 1
    }
}

impl PageParams {
    pub fn validate<K: DeserializeOwned>(&self) -> Result<PageRequest<K>, ApiError> {
        let mut fields = Vec::new();

        let limit = match self.limit.as_deref() {
            None => Some(DEFAULT_LIMIT),
            Some(raw) => raw
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|limit| (1..=MAX_LIMIT).contains(limit)),
        };
        if limit.is_none() {
            fields.push(
                FieldError::new("limit", FieldErrorCode::OutOfRange)
                    .with_param("min", 1)
                    .with_param("max", MAX_LIMIT),
            );
        }

        let after = match self.cursor.as_deref() {
            None | Some("") => None,
            Some(raw) => match decode_cursor(raw) {
                Some(key) => Some(key),
                None => {
                    fields.push(FieldError::new("cursor", FieldErrorCode::InvalidValue));
                    None
                }
            },
        };

        match limit {
            Some(limit) if fields.is_empty() => Ok(PageRequest { limit, after }),
            _ => Err(ApiError::validation(fields)),
        }
    }
}

/// One page of results.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass as `cursor` to fetch the next page; `null` on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Builds a page from up to `request.fetch_limit()` rows in list order.
    /// `key` extracts the ordering key tuple (ending with the id).
    pub fn from_overfetched<K, R, F>(mut rows: Vec<T>, request: &PageRequest<R>, key: F) -> Self
    where
        K: Serialize,
        F: Fn(&T) -> K,
    {
        let limit = request.limit as usize;
        let next_cursor = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|row| encode_cursor(&key(row)))
        } else {
            None
        };
        Self {
            items: rows,
            next_cursor,
        }
    }
}

/// Encodes an ordering key as an opaque cursor (base64url of JSON).
pub fn encode_cursor<K: Serialize>(key: &K) -> String {
    // Serializing plain key tuples (timestamps, ids, numbers) cannot fail.
    let json = serde_json::to_vec(key).expect("cursor keys serialize to JSON");
    URL_SAFE_NO_PAD.encode(json)
}

/// Decodes a cursor; `None` when it is malformed or tampered with.
pub fn decode_cursor<K: DeserializeOwned>(cursor: &str) -> Option<K> {
    if cursor.len() > MAX_CURSOR_LEN {
        return None;
    }
    let json = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    serde_json::from_slice(&json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    type Key = (String, u64);

    fn params(limit: Option<&str>, cursor: Option<&str>) -> PageParams {
        PageParams {
            limit: limit.map(Into::into),
            cursor: cursor.map(Into::into),
        }
    }

    fn rejected_fields(params: PageParams) -> Vec<(String, FieldErrorCode)> {
        let error = params.validate::<Key>().unwrap_err();
        assert_eq!(error.code(), ErrorCode::ValidationFailed);
        error
            .fields()
            .iter()
            .map(|f| (f.field.clone(), f.code))
            .collect()
    }

    #[test]
    fn limit_defaults_and_bounds() {
        assert_eq!(params(None, None).validate::<Key>().unwrap().limit, 25);
        assert_eq!(params(Some("1"), None).validate::<Key>().unwrap().limit, 1);
        assert_eq!(
            params(Some("100"), None).validate::<Key>().unwrap().limit,
            100
        );
        for bad in ["0", "101", "-1", "abc", "", "1.5"] {
            assert_eq!(
                rejected_fields(params(Some(bad), None)),
                vec![("limit".into(), FieldErrorCode::OutOfRange)],
                "limit={bad}"
            );
        }
    }

    #[test]
    fn cursor_round_trip_and_tamper() {
        let key: Key = ("2026-01-01T00:00:00Z".into(), 7);
        let cursor = encode_cursor(&key);
        let request = params(None, Some(&cursor)).validate::<Key>().unwrap();
        assert_eq!(request.after, Some(key));

        let mut tampered = cursor.clone();
        tampered.insert(3, '!');
        for bad in [
            tampered.as_str(),
            "bm90IGpzb24",
            &"A".repeat(600),
            &encode_cursor(&"x"),
        ] {
            assert_eq!(
                rejected_fields(params(None, Some(bad))),
                vec![("cursor".into(), FieldErrorCode::InvalidValue)]
            );
        }
        assert_eq!(rejected_fields(params(Some("0"), Some("%%%"))).len(), 2);
    }

    #[test]
    fn page_from_overfetched_rows() {
        let request = params(Some("2"), None).validate::<Key>().unwrap();
        assert_eq!(request.fetch_limit(), 3);

        let page = Page::from_overfetched(vec![3u64, 2, 1], &request, |n| ("t".to_owned(), *n));
        assert_eq!(page.items, vec![3, 2]);
        let next: Key = decode_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
        assert_eq!(next, ("t".to_owned(), 2));

        let last = Page::from_overfetched(vec![1u64], &request, |n| *n);
        assert_eq!(last.next_cursor, None);
        assert_eq!(
            serde_json::to_value(&last).unwrap(),
            serde_json::json!({"items": [1], "nextCursor": null})
        );
    }
}
