//! Admin API for AI budgets, limits, retention, and usage reporting. See
//! docs/ai-usage.md. Enforcement lives in [`crate::ai_guard`].
//!
//! Reports are counts and provider-reported token sums over `ai_usage`. They
//! contain no ticket content and no cost: Tikkit has no price information, so
//! it reports tokens exactly as the provider did and nothing derived from them.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::{
    AppState,
    ai::Provider,
    api::{ApiError, FieldErrors, Json, Query},
    auth::{CurrentUser, audit, ident},
    authz::{Permission, require_permission},
};

/// Budgets and operational limits for AI provider calls.
#[derive(Clone, Debug, PartialEq, Serialize, sqlx::FromRow, ToSchema)]
pub struct AiLimits {
    /// Budgets count over a rolling window of this many hours (1 to 744).
    pub window_hours: i32,
    /// Provider calls per window for the whole organization. `null`: unlimited. 0: none.
    #[schema(required = true)]
    pub org_request_limit: Option<i32>,
    /// Provider calls per window for one requester. `null`: unlimited.
    #[schema(required = true)]
    pub user_request_limit: Option<i32>,
    /// Tokens per window for the whole organization. `null`: unlimited.
    #[schema(required = true)]
    pub org_token_limit: Option<i64>,
    /// Tokens per window for one requester. `null`: unlimited.
    #[schema(required = true)]
    pub user_token_limit: Option<i64>,
    /// Provider calls in flight at once (1 to 64).
    pub max_concurrency: i32,
    /// Seconds a call may wait for a free slot before it is refused (0 to 300).
    pub concurrency_wait_seconds: i32,
    /// Consecutive provider failures that open the circuit breaker (1 to 100).
    pub breaker_failure_threshold: i32,
    /// Seconds the circuit stays open before one probe call is allowed (1 to 86400).
    pub breaker_cooldown_seconds: i32,
    /// Days usage records are kept (1 to 3650, and never shorter than the window).
    pub retention_days: i32,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

/// Every field is required; `null` on a `*_limit` removes that limit.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiLimitsWrite {
    pub window_hours: i64,
    #[schema(required = true)]
    pub org_request_limit: Option<i64>,
    #[schema(required = true)]
    pub user_request_limit: Option<i64>,
    #[schema(required = true)]
    pub org_token_limit: Option<i64>,
    #[schema(required = true)]
    pub user_token_limit: Option<i64>,
    pub max_concurrency: i64,
    pub concurrency_wait_seconds: i64,
    pub breaker_failure_threshold: i64,
    pub breaker_cooldown_seconds: i64,
    pub retention_days: i64,
}

const MAX_REQUEST_LIMIT: i64 = 1_000_000;
const MAX_TOKEN_LIMIT: i64 = 1_000_000_000_000;

impl AiLimitsWrite {
    /// The same bounds as the `ai_limits` table constraints.
    fn validate(&self) -> FieldErrors {
        let mut errors = FieldErrors::new();
        let mut range = |field: &str, value: Option<i64>, min: i64, max: i64| {
            if value.is_some_and(|v| !(min..=max).contains(&v)) {
                errors.insert(field.into(), vec!["out_of_range"]);
            }
        };
        range("window_hours", Some(self.window_hours), 1, 744);
        range(
            "org_request_limit",
            self.org_request_limit,
            0,
            MAX_REQUEST_LIMIT,
        );
        range(
            "user_request_limit",
            self.user_request_limit,
            0,
            MAX_REQUEST_LIMIT,
        );
        range("org_token_limit", self.org_token_limit, 0, MAX_TOKEN_LIMIT);
        range(
            "user_token_limit",
            self.user_token_limit,
            0,
            MAX_TOKEN_LIMIT,
        );
        range("max_concurrency", Some(self.max_concurrency), 1, 64);
        range(
            "concurrency_wait_seconds",
            Some(self.concurrency_wait_seconds),
            0,
            300,
        );
        range(
            "breaker_failure_threshold",
            Some(self.breaker_failure_threshold),
            1,
            100,
        );
        range(
            "breaker_cooldown_seconds",
            Some(self.breaker_cooldown_seconds),
            1,
            86_400,
        );
        range("retention_days", Some(self.retention_days), 1, 3650);
        // Budgets need every record of the current window.
        if !errors.contains_key("retention_days")
            && !errors.contains_key("window_hours")
            && self.retention_days * 24 < self.window_hours
        {
            errors.insert("retention_days".into(), vec!["shorter_than_window"]);
        }
        errors
    }
}

const COLUMNS: &str = "window_hours, org_request_limit, user_request_limit, org_token_limit,
    user_token_limit, max_concurrency, concurrency_wait_seconds, breaker_failure_threshold,
    breaker_cooldown_seconds, retention_days, updated_at";

async fn load_limits(db: &mut PgConnection, lock: bool) -> Result<AiLimits, sqlx::Error> {
    let lock = if lock { "FOR UPDATE" } else { "" };
    sqlx::query_as(&format!("SELECT {COLUMNS} FROM ai_limits {lock}"))
        .fetch_one(db)
        .await
}

/// AI budgets and limits (Admin only).
#[utoipa::path(get, path = "/api/settings/ai/limits", tag = "settings",
    responses(
        (status = 200, body = AiLimits),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn get_limits(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<AiLimits>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let mut db = state.pool.acquire().await?;
    Ok(Json(load_limits(&mut db, false).await?))
}

/// Replaces the AI budgets and limits (Admin only). They apply to the next
/// provider call. They never affect whether tickets can be submitted: a call
/// that is refused sends its ticket to human support.
#[utoipa::path(put, path = "/api/settings/ai/limits", tag = "settings", request_body = AiLimitsWrite,
    responses(
        (status = 200, body = AiLimits),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (any field: `out_of_range`; `retention_days`: `shorter_than_window`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn put_limits(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<AiLimitsWrite>,
) -> Result<Json<AiLimits>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let errors = body.validate();
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    // Validated above: every value fits its column type.
    let int = |value: i64| i32::try_from(value).unwrap_or(i32::MAX);
    let mut tx = state.pool.begin().await?;
    // The same lock admissions take, so a change never lands mid-admission.
    let old = load_limits(&mut tx, true).await?;
    sqlx::query(
        "UPDATE ai_limits SET window_hours = $1, org_request_limit = $2, user_request_limit = $3,
                org_token_limit = $4, user_token_limit = $5, max_concurrency = $6,
                concurrency_wait_seconds = $7, breaker_failure_threshold = $8,
                breaker_cooldown_seconds = $9, retention_days = $10,
                updated_at = now(), updated_by = $11",
    )
    .bind(int(body.window_hours))
    .bind(body.org_request_limit.map(int))
    .bind(body.user_request_limit.map(int))
    .bind(body.org_token_limit)
    .bind(body.user_token_limit)
    .bind(int(body.max_concurrency))
    .bind(int(body.concurrency_wait_seconds))
    .bind(int(body.breaker_failure_threshold))
    .bind(int(body.breaker_cooldown_seconds))
    .bind(int(body.retention_days))
    .bind(user.account_id())
    .execute(&mut *tx)
    .await?;
    let new = load_limits(&mut tx, false).await?;
    let without_time = |limits: &AiLimits| {
        let mut value = json!(limits);
        value.as_object_mut().map(|o| o.remove("updated_at"));
        value
    };
    audit(
        &mut *tx,
        "ai_limits_updated",
        Some(user.account_id()),
        None,
        json!({ "from": without_time(&old), "to": without_time(&new) }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(new))
}

// --- Usage report -----------------------------------------------------------------

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UsageQuery {
    /// Report period in days, 1 to 3650 (default 30). Records older than the
    /// retention are gone whatever the period.
    pub days: Option<i64>,
}

/// AI usage over the report period, and where the budgets stand right now.
#[derive(Serialize, ToSchema)]
pub struct AiUsageReport {
    pub days: i32,
    pub window: AiBudgetWindow,
    pub totals: AiUsageCounts,
    /// Every supported provider, in a fixed order.
    pub providers: Vec<AiProviderUsage>,
    /// Failed and refused calls by stable outcome code, most frequent first.
    pub outcomes: Vec<AiOutcomeCount>,
}

/// The organization budgets in the current rolling window.
#[derive(Serialize, ToSchema)]
pub struct AiBudgetWindow {
    pub window_hours: i32,
    /// Provider calls counted against the request budget.
    pub requests: i64,
    #[schema(required = true)]
    pub request_limit: Option<i32>,
    /// Tokens counted against the token budget: provider-reported where the
    /// provider reported them, Tikkit's admission estimate otherwise.
    pub budget_tokens: i64,
    /// The part of `budget_tokens` that is an estimate, not a provider figure.
    pub estimated_tokens: i64,
    #[schema(required = true)]
    pub token_limit: Option<i64>,
    /// Provider calls in flight right now.
    pub in_flight: i64,
    pub max_concurrency: i32,
}

/// Counts over the report period. Token sums are exactly what the provider
/// reported; `null` means no call in the period reported that value.
#[derive(Default, Serialize, sqlx::FromRow, ToSchema)]
pub struct AiUsageCounts {
    /// Admitted provider calls (succeeded, failed, or in flight).
    pub requests: i64,
    pub succeeded: i64,
    pub failed: i64,
    /// Stopped by a budget, the circuit breaker, or the concurrency limit
    /// before any provider call.
    pub refused: i64,
    /// Screenshots sent with admitted calls.
    pub images: i64,
    #[schema(required = true)]
    pub input_tokens: Option<i64>,
    #[schema(required = true)]
    pub output_tokens: Option<i64>,
    /// Sum of provider-reported totals. Not every provider reports a total.
    #[schema(required = true)]
    pub total_tokens: Option<i64>,
    /// Successful calls for which the provider reported no token count at all.
    pub usage_unreported: i64,
}

#[derive(Serialize, ToSchema)]
pub struct AiProviderUsage {
    pub provider: Provider,
    pub usage: AiUsageCounts,
    pub circuit: AiCircuit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiCircuitState {
    Closed,
    /// Calls are refused until `open_until`.
    Open,
    /// The cooldown has passed; the next call is let through as a probe.
    HalfOpen,
}

#[derive(Serialize, ToSchema)]
pub struct AiCircuit {
    pub state: AiCircuitState,
    pub consecutive_failures: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub open_until: Option<OffsetDateTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiOutcomeStatus {
    Failed,
    Refused,
}

#[derive(Serialize, ToSchema)]
pub struct AiOutcomeCount {
    pub status: AiOutcomeStatus,
    /// Stable code: an adapter failure kind (`timeout`, `rate_limited`, ...),
    /// `abandoned`, or a refusal (`budget_org_requests`, `circuit_open`, ...).
    pub outcome: String,
    pub count: i64,
}

const COUNTS: &str = "count(*) FILTER (WHERE status <> 'refused') AS requests,
    count(*) FILTER (WHERE status = 'succeeded') AS succeeded,
    count(*) FILTER (WHERE status = 'failed') AS failed,
    count(*) FILTER (WHERE status = 'refused') AS refused,
    COALESCE(sum(image_count) FILTER (WHERE status <> 'refused'), 0)::bigint AS images,
    sum(input_tokens)::bigint AS input_tokens,
    sum(output_tokens)::bigint AS output_tokens,
    sum(total_tokens)::bigint AS total_tokens,
    count(*) FILTER (WHERE status = 'succeeded' AND input_tokens IS NULL
                       AND output_tokens IS NULL AND total_tokens IS NULL) AS usage_unreported";

/// AI usage and failure counts (Admin only): provider calls, refusals, failures
/// by stable code, tokens as reported by each provider, the circuit breaker
/// state, and the current budget window. No ticket content and no cost figures.
#[utoipa::path(get, path = "/api/settings/ai/usage", tag = "settings", params(UsageQuery),
    responses(
        (status = 200, body = AiUsageReport),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`days`: `out_of_range`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn usage(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<UsageQuery>,
) -> Result<Json<AiUsageReport>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let days = query.days.unwrap_or(30);
    if !(1..=3650).contains(&days) {
        return Err(ApiError::Validation(FieldErrors::from([(
            "days".into(),
            vec!["out_of_range"],
        )])));
    }
    let days = i32::try_from(days).unwrap_or(3650);
    // One snapshot, so the parts of the report agree with each other.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let limits = load_limits(&mut tx, false).await?;
    let (requests, budget_tokens, estimated_tokens, in_flight): (i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT count(*),
                    COALESCE(sum(budget_tokens), 0)::bigint,
                    COALESCE(sum(budget_tokens) FILTER (WHERE budget_basis = 'estimated'), 0)::bigint,
                    count(*) FILTER (WHERE status = 'reserved' AND lease_expires_at > now())
             FROM ai_usage
             WHERE status <> 'refused' AND started_at > now() - make_interval(hours => $1)",
        )
        .bind(limits.window_hours)
        .fetch_one(&mut *tx)
        .await?;
    let period = "started_at > now() - make_interval(days => $1)";
    let totals: AiUsageCounts =
        sqlx::query_as(&format!("SELECT {COUNTS} FROM ai_usage WHERE {period}"))
            .bind(days)
            .fetch_one(&mut *tx)
            .await?;
    let mut by_provider: Vec<AiUsageCounts> = Vec::new();
    for provider in Provider::ALL {
        let counts = sqlx::query_as(&format!(
            "SELECT {COUNTS} FROM ai_usage WHERE {period} AND provider = $2"
        ))
        .bind(days)
        .bind(provider.as_str())
        .fetch_one(&mut *tx)
        .await?;
        by_provider.push(counts);
    }
    let circuits: Vec<(String, i32, Option<OffsetDateTime>, bool)> = sqlx::query_as(
        "SELECT provider, consecutive_failures, open_until, COALESCE(open_until > now(), false)
         FROM ai_circuit",
    )
    .fetch_all(&mut *tx)
    .await?;
    let outcomes: Vec<(String, String, i64)> = sqlx::query_as(&format!(
        "SELECT status, outcome, count(*) FROM ai_usage
         WHERE {period} AND status IN ('failed', 'refused')
         GROUP BY status, outcome ORDER BY 3 DESC, 1, 2"
    ))
    .bind(days)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let providers = Provider::ALL
        .into_iter()
        .zip(by_provider)
        .map(|(provider, usage)| {
            let circuit = circuits
                .iter()
                .find(|(name, ..)| name == provider.as_str())
                .map_or(
                    AiCircuit {
                        state: AiCircuitState::Closed,
                        consecutive_failures: 0,
                        open_until: None,
                    },
                    |(_, failures, open_until, still_open)| AiCircuit {
                        state: match (open_until, still_open) {
                            (None, _) => AiCircuitState::Closed,
                            (Some(_), true) => AiCircuitState::Open,
                            (Some(_), false) => AiCircuitState::HalfOpen,
                        },
                        consecutive_failures: *failures,
                        open_until: *open_until,
                    },
                );
            AiProviderUsage {
                provider,
                usage,
                circuit,
            }
        })
        .collect();
    Ok(Json(AiUsageReport {
        days,
        window: AiBudgetWindow {
            window_hours: limits.window_hours,
            requests,
            request_limit: limits.org_request_limit,
            budget_tokens,
            estimated_tokens,
            token_limit: limits.org_token_limit,
            in_flight,
            max_concurrency: limits.max_concurrency,
        },
        totals,
        providers,
        outcomes: outcomes
            .into_iter()
            .map(|(status, outcome, count)| {
                Ok(AiOutcomeCount {
                    status: ident(status)?,
                    outcome,
                    count,
                })
            })
            .collect::<Result<_, ApiError>>()?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> AiLimitsWrite {
        AiLimitsWrite {
            window_hours: 24,
            org_request_limit: Some(500),
            user_request_limit: Some(10),
            org_token_limit: None,
            user_token_limit: None,
            max_concurrency: 2,
            concurrency_wait_seconds: 30,
            breaker_failure_threshold: 5,
            breaker_cooldown_seconds: 60,
            retention_days: 90,
        }
    }

    #[test]
    fn limits_are_bounded_at_both_ends() {
        assert!(valid().validate().is_empty());
        let edge = AiLimitsWrite {
            window_hours: 744,
            org_request_limit: Some(0),
            user_request_limit: Some(MAX_REQUEST_LIMIT),
            org_token_limit: Some(0),
            user_token_limit: Some(MAX_TOKEN_LIMIT),
            max_concurrency: 64,
            concurrency_wait_seconds: 0,
            breaker_failure_threshold: 100,
            breaker_cooldown_seconds: 86_400,
            retention_days: 31,
        };
        assert!(edge.validate().is_empty());
        for extreme in [i64::MIN, -1, i64::MAX] {
            let errors = AiLimitsWrite {
                window_hours: extreme,
                org_request_limit: Some(extreme),
                user_request_limit: Some(extreme),
                org_token_limit: Some(extreme),
                user_token_limit: Some(extreme),
                max_concurrency: extreme,
                concurrency_wait_seconds: extreme,
                breaker_failure_threshold: extreme,
                breaker_cooldown_seconds: extreme,
                retention_days: extreme,
            }
            .validate();
            assert_eq!(errors.len(), 10, "{extreme}");
            assert!(errors.values().all(|codes| codes == &vec!["out_of_range"]));
        }
        let zero = AiLimitsWrite {
            max_concurrency: 0,
            window_hours: 0,
            ..valid()
        };
        assert_eq!(zero.validate().len(), 2);
    }

    #[test]
    fn retention_covers_the_budget_window() {
        let short = AiLimitsWrite {
            window_hours: 49,
            retention_days: 2,
            ..valid()
        };
        assert_eq!(
            short.validate().get("retention_days"),
            Some(&vec!["shorter_than_window"])
        );
        let exact = AiLimitsWrite {
            window_hours: 48,
            retention_days: 2,
            ..valid()
        };
        assert!(exact.validate().is_empty());
    }
}
