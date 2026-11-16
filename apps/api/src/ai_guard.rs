//! The one guarded path to an AI provider. See docs/ai-usage.md.
//!
//! [`AiGuard::suggest`] admits a call against the Admin-set budgets, the
//! provider concurrency limit, and the circuit breaker, makes the call through
//! [`AiClient::suggest`], and records what happened. Everything that decides
//! admission lives in PostgreSQL (`ai_limits`, `ai_usage`, `ai_circuit`), so the
//! limits hold across the API process and any number of workers, and tests can
//! move time by editing rows.
//!
//! Nothing here reads or writes ticket state. A refusal is a value: the caller
//! (the attempt worker, `ai_attempts`) hands the ticket to human support exactly once.
//!
//! `ai_usage` rows and log lines carry metadata only: account id, provider,
//! model, image count, token counts as reported, and stable outcome codes.
//! Never prompt text, ticket content, image bytes, or provider message text.

use std::time::Duration;

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    AppState,
    ai::{
        AiClient, AiError, AiErrorKind, Cancel, Endpoint, MAX_OUTPUT_TOKENS, Provider, Suggestion,
        SuggestionRequest,
    },
    ai_config::ActiveAi,
    api::ApiError,
    auth::audit,
    notifications::poll_loop,
};

/// Admission estimate: tokens for the fixed instructions and output schema.
pub const ESTIMATED_OVERHEAD_TOKENS: u64 = 1_000;
/// Admission estimate: tokens per screenshot, above every provider's usual charge.
pub const ESTIMATED_TOKENS_PER_IMAGE: u64 = 2_000;

/// How long a reservation outlives the client's total timeout before another
/// process may treat the call as abandoned.
const LEASE_MARGIN: Duration = Duration::from_secs(30);
/// How often a call waiting for a concurrency slot looks again.
const SLOT_POLL: Duration = Duration::from_millis(200);
/// A provider's `Retry-After` never keeps the circuit open longer than this.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);
/// Rows deleted per retention step.
const PURGE_BATCH: i64 = 1000;
const PURGE_EVERY: Duration = Duration::from_secs(3600);

/// Which budget is used up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetScope {
    UserRequests,
    OrgRequests,
    UserTokens,
    OrgTokens,
}

/// Why there is no suggestion. Every variant is recoverable for the ticket: the
/// caller routes it to human support. Codes are stable ([`Refusal::code`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A request or token budget is used up for the current window. No provider call.
    BudgetExhausted(BudgetScope),
    /// The circuit breaker is open after repeated provider failures. No provider call.
    CircuitOpen,
    /// No concurrency slot became free within the bounded wait. No provider call.
    ConcurrencySaturated,
    /// The provider answered with its rate limit or quota (429/402).
    RateLimited { retry_after: Option<Duration> },
    /// Any other provider-call failure, by adapter kind.
    ProviderFailed(AiErrorKind),
    /// Stopped through the [`Cancel`] handle, before or during the call.
    Cancelled,
    /// The guard could not reach its own state (database). No provider call.
    GuardUnavailable,
}

impl Refusal {
    /// Stable machine identifier, as stored in `ai_usage.outcome`.
    pub fn code(self) -> &'static str {
        match self {
            Self::BudgetExhausted(BudgetScope::UserRequests) => "budget_user_requests",
            Self::BudgetExhausted(BudgetScope::OrgRequests) => "budget_org_requests",
            Self::BudgetExhausted(BudgetScope::UserTokens) => "budget_user_tokens",
            Self::BudgetExhausted(BudgetScope::OrgTokens) => "budget_org_tokens",
            Self::CircuitOpen => "circuit_open",
            Self::ConcurrencySaturated => "concurrency_saturated",
            Self::RateLimited { .. } => AiErrorKind::RateLimited.as_str(),
            Self::ProviderFailed(kind) => kind.as_str(),
            Self::Cancelled => AiErrorKind::Cancelled.as_str(),
            Self::GuardUnavailable => "guard_unavailable",
        }
    }

    /// Whether a provider call may have been made (and may have been billed).
    pub fn provider_called(self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::ProviderFailed(_) | Self::Cancelled
        )
    }
}

/// A refused or failed guarded call. `usage_id` is its `ai_usage` row, when one
/// was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub refusal: Refusal,
    pub usage_id: Option<Uuid>,
}

/// The one suggestion of a guarded call and its `ai_usage` row.
#[derive(Clone, Debug)]
pub struct Guarded {
    pub suggestion: Suggestion,
    pub usage_id: Uuid,
}

/// Tokens a call is assumed to cost until the provider reports its usage:
/// instructions, two characters per token of requester text, a flat amount per
/// image, and the full output allowance. Deliberately high; it is an estimate
/// and is stored as one.
pub fn estimate_tokens(request: &SuggestionRequest) -> u64 {
    let chars = request.category.chars().count()
        + request.subject.chars().count()
        + request.description.chars().count()
        + request
            .answers
            .iter()
            .map(|a| a.label.chars().count() + a.value.chars().count())
            .sum::<usize>();
    ESTIMATED_OVERHEAD_TOKENS
        + (chars as u64).div_ceil(2)
        + request.images.len() as u64 * ESTIMATED_TOKENS_PER_IMAGE
        + u64::from(MAX_OUTPUT_TOKENS)
}

/// Whether a failure says the provider (or the key) is unhealthy, as opposed
/// to this one request being unacceptable.
fn trips_breaker(kind: AiErrorKind) -> bool {
    matches!(
        kind,
        AiErrorKind::Timeout
            | AiErrorKind::UpstreamUnavailable
            | AiErrorKind::RateLimited
            | AiErrorKind::InvalidCredentials
            | AiErrorKind::DestinationBlocked
    )
}

/// Whether a failed call may still have generated (and billed) tokens. The
/// provider rejected the request outright when it answered with an error status
/// or was never reached; everything else keeps the admission estimate.
fn may_have_generated(error: &AiError) -> bool {
    !matches!(error.status, Some(status) if !(200..300).contains(&status))
        && !matches!(
            error.kind,
            AiErrorKind::InvalidInput
                | AiErrorKind::UpstreamUnavailable
                | AiErrorKind::DestinationBlocked
        )
}

#[derive(sqlx::FromRow)]
struct Limits {
    window_hours: i32,
    org_request_limit: Option<i32>,
    user_request_limit: Option<i32>,
    org_token_limit: Option<i64>,
    user_token_limit: Option<i64>,
    max_concurrency: i32,
}

enum Admission {
    Admitted {
        usage_id: Uuid,
        probe: bool,
    },
    Refused(Refusal, Uuid),
    /// Every concurrency slot is taken right now. Nothing was written.
    Busy,
}

/// Guarded access to the configured provider. Cheap to clone.
#[derive(Clone)]
pub struct AiGuard {
    pool: PgPool,
    client: AiClient,
    endpoint: Endpoint,
}

impl AiGuard {
    pub fn new(pool: PgPool, client: AiClient, endpoint: Endpoint) -> Self {
        Self {
            pool,
            client,
            endpoint,
        }
    }

    pub fn from_state(state: &AppState) -> Self {
        Self::new(
            state.pool.clone(),
            state.ai_client.clone(),
            state.ai_endpoint.clone(),
        )
    }

    /// Asks the configured provider for the one suggestion, on behalf of the
    /// requester `account_id`.
    ///
    /// Admission is atomic: budgets, the circuit breaker, and the concurrency
    /// limit are checked and the call is reserved in one transaction, so
    /// concurrent callers cannot overshoot. A call that finds every slot taken
    /// waits up to the configured time. The outcome is recorded in `ai_usage`
    /// before this returns; an error means no usable suggestion exists.
    pub async fn suggest(
        &self,
        account_id: Uuid,
        active: &ActiveAi,
        request: &SuggestionRequest,
        cancel: &Cancel,
    ) -> Result<Guarded, Refused> {
        let unavailable = |_: sqlx::Error| {
            tracing::error!(
                error_code = "database",
                "AI guard state unavailable; call refused"
            );
            Refused {
                refusal: Refusal::GuardUnavailable,
                usage_id: None,
            }
        };
        // Enforced here, not left to callers: nothing reaches a provider
        // unredacted or unbounded. Minimizing an already minimized request
        // changes nothing.
        let request = &crate::ai_privacy::minimize(request.clone()).request;
        let wait: i32 = sqlx::query_scalar("SELECT concurrency_wait_seconds FROM ai_limits")
            .fetch_one(&self.pool)
            .await
            .map_err(unavailable)?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(wait.max(0).unsigned_abs().into());
        let (usage_id, probe) = loop {
            if cancel.is_cancelled() {
                return Err(Refused {
                    refusal: Refusal::Cancelled,
                    usage_id: None,
                });
            }
            match self
                .admit(account_id, active, request)
                .await
                .map_err(unavailable)?
            {
                Admission::Admitted { usage_id, probe } => break (usage_id, probe),
                Admission::Refused(refusal, usage_id) => {
                    log_outcome(usage_id, active, request, "refused", Some(refusal.code()));
                    return Err(Refused {
                        refusal,
                        usage_id: Some(usage_id),
                    });
                }
                Admission::Busy if tokio::time::Instant::now() >= deadline => {
                    let refusal = Refusal::ConcurrencySaturated;
                    let usage_id = insert_refusal(
                        &mut *self.acquire().await.map_err(unavailable)?,
                        account_id,
                        active,
                        request,
                        refusal,
                    )
                    .await
                    .map_err(unavailable)?;
                    log_outcome(usage_id, active, request, "refused", Some(refusal.code()));
                    return Err(Refused {
                        refusal,
                        usage_id: Some(usage_id),
                    });
                }
                Admission::Busy => tokio::select! {
                    () = tokio::time::sleep(SLOT_POLL) => {}
                    () = cancel.cancelled() => {}
                },
            }
        };

        let config = active.provider_config(&self.endpoint);
        let result = self.client.suggest(&config, request, cancel).await;
        // A failure to record leaves the reservation in place until its lease
        // passes; it then counts as `abandoned` with its estimate. The result
        // of the call stands either way.
        if self
            .record(usage_id, active.provider, probe, &result)
            .await
            .is_err()
        {
            tracing::error!(
                error_code = "database",
                %usage_id,
                "AI usage outcome could not be recorded"
            );
        }
        match result {
            Ok(suggestion) => {
                tracing::info!(
                    %usage_id,
                    provider = active.provider.as_str(),
                    model = %active.model,
                    images = request.images.len(),
                    status = "succeeded",
                    input_tokens = suggestion.usage.input_tokens,
                    output_tokens = suggestion.usage.output_tokens,
                    total_tokens = suggestion.usage.total_tokens,
                    "AI provider call finished"
                );
                Ok(Guarded {
                    suggestion,
                    usage_id,
                })
            }
            Err(error) => {
                log_outcome(
                    usage_id,
                    active,
                    request,
                    "failed",
                    Some(error.kind.as_str()),
                );
                let refusal = match error.kind {
                    AiErrorKind::RateLimited => Refusal::RateLimited {
                        retry_after: error.retry_after,
                    },
                    AiErrorKind::Cancelled => Refusal::Cancelled,
                    kind => Refusal::ProviderFailed(kind),
                };
                Err(Refused {
                    refusal,
                    usage_id: Some(usage_id),
                })
            }
        }
    }

    async fn acquire(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, sqlx::Error> {
        self.pool.acquire().await
    }

    /// One atomic admission attempt. The `ai_limits` row lock serializes every
    /// admission in the deployment, which is what makes the counts race-free.
    async fn admit(
        &self,
        account_id: Uuid,
        active: &ActiveAi,
        request: &SuggestionRequest,
    ) -> Result<Admission, sqlx::Error> {
        let estimate = i64::try_from(estimate_tokens(request)).unwrap_or(i64::MAX);
        let provider = active.provider.as_str();
        let mut tx = self.pool.begin().await?;
        let limits: Limits = sqlx::query_as(
            "SELECT window_hours, org_request_limit, user_request_limit, org_token_limit,
                    user_token_limit, max_concurrency
             FROM ai_limits FOR UPDATE",
        )
        .fetch_one(&mut *tx)
        .await?;
        expire_leases(&mut tx).await?;

        let (org_requests, user_requests, org_tokens, user_tokens): (i64, i64, i64, i64) =
            sqlx::query_as(
                "SELECT count(*),
                        count(*) FILTER (WHERE account_id = $2),
                        COALESCE(sum(budget_tokens), 0)::bigint,
                        COALESCE(sum(budget_tokens) FILTER (WHERE account_id = $2), 0)::bigint
                 FROM ai_usage
                 WHERE status <> 'refused'
                   AND started_at > clock_timestamp() - make_interval(hours => $1)",
            )
            .bind(limits.window_hours)
            .bind(account_id)
            .fetch_one(&mut *tx)
            .await?;
        let over = |used: i64, add: i64, limit: Option<i64>| {
            limit.is_some_and(|limit| used.saturating_add(add) > limit)
        };
        let exhausted = if over(user_requests, 1, limits.user_request_limit.map(i64::from)) {
            Some(BudgetScope::UserRequests)
        } else if over(org_requests, 1, limits.org_request_limit.map(i64::from)) {
            Some(BudgetScope::OrgRequests)
        } else if over(user_tokens, estimate, limits.user_token_limit) {
            Some(BudgetScope::UserTokens)
        } else if over(org_tokens, estimate, limits.org_token_limit) {
            Some(BudgetScope::OrgTokens)
        } else {
            None
        };
        if let Some(scope) = exhausted {
            let refusal = Refusal::BudgetExhausted(scope);
            let id = insert_refusal(&mut tx, account_id, active, request, refusal).await?;
            tx.commit().await?;
            return Ok(Admission::Refused(refusal, id));
        }

        // (open, a probe may go now)
        let circuit: Option<(bool, bool)> = sqlx::query_as(
            "SELECT open_until IS NOT NULL,
                    COALESCE(open_until <= clock_timestamp()
                        AND (probe_expires_at IS NULL OR probe_expires_at <= clock_timestamp()),
                        false)
             FROM ai_circuit WHERE provider = $1 FOR UPDATE",
        )
        .bind(provider)
        .fetch_optional(&mut *tx)
        .await?;
        let probe = match circuit {
            Some((true, false)) => {
                let refusal = Refusal::CircuitOpen;
                let id = insert_refusal(&mut tx, account_id, active, request, refusal).await?;
                tx.commit().await?;
                return Ok(Admission::Refused(refusal, id));
            }
            Some((true, true)) => true,
            _ => false,
        };

        let in_flight: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ai_usage WHERE status = 'reserved'")
                .fetch_one(&mut *tx)
                .await?;
        if in_flight >= i64::from(limits.max_concurrency) {
            return Ok(Admission::Busy);
        }

        let lease = (self.client.total_timeout() + LEASE_MARGIN).as_secs_f64();
        let usage_id: Uuid = sqlx::query_scalar(
            "INSERT INTO ai_usage (account_id, provider, model, image_count, status,
                                   budget_tokens, budget_basis, breaker_probe, started_at,
                                   lease_expires_at)
             VALUES ($1, $2, $3, $4, 'reserved', $5, 'estimated', $6, clock_timestamp(),
                     clock_timestamp() + make_interval(secs => $7))
             RETURNING id",
        )
        .bind(account_id)
        .bind(provider)
        .bind(&active.model)
        .bind(image_count(request))
        .bind(estimate)
        .bind(probe)
        .bind(lease)
        .fetch_one(&mut *tx)
        .await?;
        if probe {
            sqlx::query(
                "UPDATE ai_circuit
                 SET probe_expires_at = clock_timestamp() + make_interval(secs => $2)
                 WHERE provider = $1",
            )
            .bind(provider)
            .bind(lease)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Admission::Admitted { usage_id, probe })
    }

    /// Writes the outcome of an admitted call and updates the circuit breaker.
    async fn record(
        &self,
        usage_id: Uuid,
        provider: Provider,
        probe: bool,
        result: &Result<Suggestion, AiError>,
    ) -> Result<(), sqlx::Error> {
        let tokens = |value: Option<u64>| value.map(stored_tokens);
        let mut tx = self.pool.begin().await?;
        // Also matches a row another process already swept as abandoned.
        let update = "UPDATE ai_usage SET status = $2, outcome = $3, http_status = $4,
                provider_code = $5, input_tokens = $6, output_tokens = $7, total_tokens = $8,
                budget_tokens = COALESCE($9, budget_tokens), budget_basis = $10,
                finished_at = clock_timestamp(), lease_expires_at = NULL
             WHERE id = $1 AND (status = 'reserved' OR outcome = 'abandoned')";
        match result {
            Ok(suggestion) => {
                let usage = suggestion.usage;
                // The provider's own total when it reports one, otherwise its
                // input plus output. With neither, the estimate stays.
                let reported = usage
                    .total_tokens
                    .or_else(|| Some(usage.input_tokens?.saturating_add(usage.output_tokens?)));
                sqlx::query(update)
                    .bind(usage_id)
                    .bind("succeeded")
                    .bind(None::<&str>)
                    .bind(None::<i32>)
                    .bind(None::<&str>)
                    .bind(tokens(usage.input_tokens))
                    .bind(tokens(usage.output_tokens))
                    .bind(tokens(usage.total_tokens))
                    .bind(tokens(reported))
                    .bind(if reported.is_some() {
                        "reported"
                    } else {
                        "estimated"
                    })
                    .execute(&mut *tx)
                    .await?;
                // The provider works: close the circuit and forget past failures.
                sqlx::query("DELETE FROM ai_circuit WHERE provider = $1")
                    .bind(provider.as_str())
                    .execute(&mut *tx)
                    .await?;
            }
            Err(error) => {
                let billed = may_have_generated(error);
                sqlx::query(update)
                    .bind(usage_id)
                    .bind("failed")
                    .bind(error.kind.as_str())
                    // The column accepts real HTTP statuses only.
                    .bind(
                        error
                            .status
                            .filter(|status| (100..600).contains(status))
                            .map(i32::from),
                    )
                    .bind(error.provider_code.as_deref())
                    .bind(None::<i64>)
                    .bind(None::<i64>)
                    .bind(None::<i64>)
                    .bind((!billed).then_some(0i64))
                    .bind(if billed { "estimated" } else { "none" })
                    .execute(&mut *tx)
                    .await?;
                if trips_breaker(error.kind) {
                    count_failure(&mut tx, provider, probe, error.retry_after).await?;
                } else if probe && error.kind == AiErrorKind::Cancelled {
                    // Nothing was learned: let the next call probe.
                    sqlx::query(
                        "UPDATE ai_circuit SET probe_expires_at = NULL WHERE provider = $1",
                    )
                    .bind(provider.as_str())
                    .execute(&mut *tx)
                    .await?;
                } else if probe {
                    // The provider answered; only this request was unacceptable.
                    sqlx::query("DELETE FROM ai_circuit WHERE provider = $1")
                        .bind(provider.as_str())
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        tx.commit().await
    }
}

fn image_count(request: &SuggestionRequest) -> i32 {
    i32::try_from(request.images.len()).unwrap_or(i32::MAX)
}

/// The only log line shape for refusals and failures: identifiers and codes.
fn log_outcome(
    usage_id: Uuid,
    active: &ActiveAi,
    request: &SuggestionRequest,
    status: &'static str,
    outcome: Option<&'static str>,
) {
    tracing::info!(
        %usage_id,
        provider = active.provider.as_str(),
        model = %active.model,
        images = request.images.len(),
        status,
        outcome,
        "AI provider call finished"
    );
}

async fn insert_refusal(
    db: &mut PgConnection,
    account_id: Uuid,
    active: &ActiveAi,
    request: &SuggestionRequest,
    refusal: Refusal,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO ai_usage (account_id, provider, model, image_count, status, outcome,
                               budget_tokens, budget_basis, started_at, finished_at)
         VALUES ($1, $2, $3, $4, 'refused', $5, 0, 'none', clock_timestamp(), clock_timestamp())
         RETURNING id",
    )
    .bind(account_id)
    .bind(active.provider.as_str())
    .bind(&active.model)
    .bind(image_count(request))
    .bind(refusal.code())
    .fetch_one(db)
    .await
}

/// Ends reservations whose process never reported back. The call may have
/// reached the provider, so the row keeps counting with its estimate.
async fn expire_leases(db: &mut PgConnection) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "UPDATE ai_usage SET status = 'failed', outcome = 'abandoned',
                finished_at = clock_timestamp(), lease_expires_at = NULL
         WHERE status = 'reserved' AND lease_expires_at <= clock_timestamp()",
    )
    .execute(db)
    .await
    .map(|done| done.rows_affected())
}

/// Counts one provider failure. The circuit opens when the threshold is
/// reached, and again whenever the half-open probe fails. A provider's
/// `Retry-After` can keep it open longer than the configured cooldown.
async fn count_failure(
    db: &mut PgConnection,
    provider: Provider,
    probe: bool,
    retry_after: Option<Duration>,
) -> Result<(), sqlx::Error> {
    let (threshold, cooldown): (i32, i32) =
        sqlx::query_as("SELECT breaker_failure_threshold, breaker_cooldown_seconds FROM ai_limits")
            .fetch_one(&mut *db)
            .await?;
    let open_for = Duration::from_secs(cooldown.max(1).unsigned_abs().into())
        .max(retry_after.unwrap_or_default().min(MAX_RETRY_AFTER));
    sqlx::query(
        "INSERT INTO ai_circuit AS c (provider, consecutive_failures, open_until, opened_at)
         VALUES ($1, 1,
                 CASE WHEN $2 <= 1 THEN clock_timestamp() + make_interval(secs => $3) END,
                 CASE WHEN $2 <= 1 THEN clock_timestamp() END)
         ON CONFLICT (provider) DO UPDATE SET
            consecutive_failures = c.consecutive_failures + 1,
            open_until = CASE
                WHEN $4 OR (c.open_until IS NULL AND c.consecutive_failures + 1 >= $2)
                THEN clock_timestamp() + make_interval(secs => $3)
                ELSE c.open_until END,
            opened_at = CASE
                WHEN $4 OR (c.open_until IS NULL AND c.consecutive_failures + 1 >= $2)
                THEN clock_timestamp()
                ELSE c.opened_at END,
            probe_expires_at = CASE WHEN $4 THEN NULL ELSE c.probe_expires_at END",
    )
    .bind(provider.as_str())
    .bind(threshold)
    .bind(open_for.as_secs_f64())
    .bind(probe)
    .execute(db)
    .await
    .map(drop)
}

/// Closes `provider`'s circuit and forgets its failures. Called when an Admin
/// changes the key or the selection: old failures say nothing about the new one.
pub async fn reset_circuit(db: &mut PgConnection, provider: Provider) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM ai_circuit WHERE provider = $1")
        .bind(provider.as_str())
        .execute(db)
        .await
        .map(drop)
}

// --- Retention ------------------------------------------------------------------

/// Deletes finished usage records older than the Admin-set retention, at most
/// one batch per call, and returns how many it deleted. Each batch writes one
/// `ai_usage_purged` audit event (system actor) with the count. Reservations in
/// flight are never deleted.
pub async fn purge_expired(pool: &PgPool) -> Result<u64, ApiError> {
    let mut tx = pool.begin().await?;
    let retention_days: i32 = sqlx::query_scalar("SELECT retention_days FROM ai_limits FOR SHARE")
        .fetch_one(&mut *tx)
        .await?;
    let deleted = sqlx::query(
        "DELETE FROM ai_usage WHERE id IN (
            SELECT id FROM ai_usage
            WHERE status <> 'reserved'
              AND started_at < clock_timestamp() - make_interval(days => $1)
            ORDER BY started_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
    )
    .bind(retention_days)
    .bind(PURGE_BATCH)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted > 0 {
        audit(
            &mut *tx,
            "ai_usage_purged",
            None,
            None,
            serde_json::json!({ "deleted": deleted, "retention_days": retention_days }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(deleted)
}

/// Ends abandoned reservations (see [`AiGuard::suggest`]) without waiting for
/// the next admission. Returns how many it ended.
pub async fn release_abandoned(pool: &PgPool) -> Result<u64, ApiError> {
    Ok(expire_leases(&mut *pool.acquire().await?).await?)
}

/// A provider-reported token count as stored. Capped so that one absurd value
/// can neither overflow the window sums nor be taken at face value.
fn stored_tokens(reported: u64) -> i64 {
    const CAP: u64 = 1_000_000_000_000;
    reported.min(CAP) as i64
}

/// Background retention: abandoned reservations are released on every poll,
/// expired records are purged hourly (and at startup) until none are left.
pub async fn retention_loop(pool: PgPool) {
    let mut last_purge: Option<tokio::time::Instant> = None;
    poll_loop("ai_usage_retention", || {
        let pool = pool.clone();
        let due = last_purge.is_none_or(|at| at.elapsed() >= PURGE_EVERY);
        if due {
            last_purge = Some(tokio::time::Instant::now());
        }
        async move {
            release_abandoned(&pool).await?;
            if !due {
                return Ok(false);
            }
            while purge_expired(&pool).await? > 0 {}
            Ok(false)
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{ConditionalAnswer, ImageInput, ImageMediaType, Language};

    fn request(description: &str, images: usize) -> SuggestionRequest {
        SuggestionRequest {
            language: Language::En,
            category: "ab".into(),
            subject: String::new(),
            description: description.into(),
            answers: vec![ConditionalAnswer {
                label: "ab".into(),
                value: "abc".into(),
            }],
            images: vec![
                ImageInput {
                    media_type: ImageMediaType::Png,
                    bytes: vec![],
                };
                images
            ],
        }
    }

    #[test]
    fn the_estimate_covers_text_images_and_the_output_allowance() {
        let base = ESTIMATED_OVERHEAD_TOKENS + u64::from(MAX_OUTPUT_TOKENS);
        // 7 characters of fixed text + 3 of description = 10 characters.
        assert_eq!(estimate_tokens(&request("abc", 0)), base + 5);
        assert_eq!(estimate_tokens(&request("abcd", 0)), base + 6);
        assert_eq!(
            estimate_tokens(&request("abc", 3)),
            base + 5 + 3 * ESTIMATED_TOKENS_PER_IMAGE
        );
    }

    #[test]
    fn refusal_codes_are_stable_and_distinct() {
        use AiErrorKind::*;
        let all = [
            Refusal::BudgetExhausted(BudgetScope::UserRequests),
            Refusal::BudgetExhausted(BudgetScope::OrgRequests),
            Refusal::BudgetExhausted(BudgetScope::UserTokens),
            Refusal::BudgetExhausted(BudgetScope::OrgTokens),
            Refusal::CircuitOpen,
            Refusal::ConcurrencySaturated,
            Refusal::RateLimited { retry_after: None },
            Refusal::ProviderFailed(Timeout),
            Refusal::ProviderFailed(UpstreamUnavailable),
            Refusal::ProviderFailed(InvalidCredentials),
            Refusal::ProviderFailed(ModelError),
            Refusal::ProviderFailed(MalformedOutput),
            Refusal::ProviderFailed(OversizedOutput),
            Refusal::ProviderFailed(Vision),
            Refusal::ProviderFailed(InvalidInput),
            Refusal::ProviderFailed(DestinationBlocked),
            Refusal::Cancelled,
            Refusal::GuardUnavailable,
        ];
        let codes: std::collections::BTreeSet<_> = all.iter().map(|r| r.code()).collect();
        assert_eq!(codes.len(), all.len());
        assert!(codes.iter().all(|code| {
            !code.is_empty() && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        }));
        assert_eq!(Refusal::CircuitOpen.code(), "circuit_open");
        assert_eq!(
            Refusal::BudgetExhausted(BudgetScope::OrgTokens).code(),
            "budget_org_tokens"
        );
        assert!(!Refusal::CircuitOpen.provider_called());
        assert!(!Refusal::BudgetExhausted(BudgetScope::UserRequests).provider_called());
        assert!(Refusal::ProviderFailed(Timeout).provider_called());
    }

    #[test]
    fn only_provider_health_failures_trip_the_breaker() {
        use AiErrorKind::*;
        for kind in [
            Timeout,
            UpstreamUnavailable,
            RateLimited,
            InvalidCredentials,
            DestinationBlocked,
        ] {
            assert!(trips_breaker(kind), "{kind:?}");
        }
        for kind in [
            InvalidInput,
            ModelError,
            MalformedOutput,
            OversizedOutput,
            Vision,
            Cancelled,
        ] {
            assert!(!trips_breaker(kind), "{kind:?}");
        }
    }

    #[test]
    fn rejected_calls_are_not_charged_but_uncertain_ones_keep_the_estimate() {
        let error = |kind, status| AiError {
            kind,
            status,
            provider_code: None,
            retry_after: None,
        };
        assert!(!may_have_generated(&error(
            AiErrorKind::RateLimited,
            Some(429)
        )));
        assert!(!may_have_generated(&error(
            AiErrorKind::ModelError,
            Some(400)
        )));
        assert!(!may_have_generated(&error(
            AiErrorKind::UpstreamUnavailable,
            None
        )));
        assert!(!may_have_generated(&error(AiErrorKind::InvalidInput, None)));
        assert!(may_have_generated(&error(AiErrorKind::Timeout, None)));
        assert!(may_have_generated(&error(AiErrorKind::Cancelled, None)));
        assert!(may_have_generated(&error(
            AiErrorKind::MalformedOutput,
            Some(200)
        )));
        assert!(may_have_generated(&error(
            AiErrorKind::OversizedOutput,
            None
        )));
    }
}
