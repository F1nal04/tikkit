//! Admin inspection of AI outcomes: statistics and the list of AI attempts with
//! what became of each. Operators never reach this; AI-solved records are not
//! operator work. See docs/ai-decisions.md.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    ai_attempts::AiAttemptState,
    api::{ApiError, Cursor, FieldErrors, Json, Page, Pagination, Query, TicketStatus},
    auth::{CurrentUser, ident, ident_str},
    authz::{Permission, require_permission},
    tickets::TicketRequester,
};

/// What became of a ticket's AI attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiRecordOutcome {
    /// The suggestion is still being prepared.
    Processing,
    /// A suggestion is stored; the requester has not decided.
    AwaitingDecision,
    /// The requester confirmed the suggestion solved it (`ai_solved`, final).
    Solved,
    /// The requester still needed help after the suggestion: escalated.
    NotSolved,
    /// The requester skipped AI while it was processing: escalated.
    Skipped,
    /// No suggestion (provider, limits, timeout, configuration): escalated.
    Failed,
    /// The requester cancelled the ticket before deciding.
    Cancelled,
}

/// `ai_attempts a`, `tickets t`, and the decision event `d` to one outcome.
const OUTCOME_SQL: &str = "CASE
    WHEN d.event_type = 'ai_solved' THEN 'solved'
    WHEN d.event_type = 'ai_not_solved' THEN 'not_solved'
    WHEN a.state = 'processing' THEN 'processing'
    WHEN a.outcome = 'requester_skip' THEN 'skipped'
    WHEN a.state = 'handed_off' THEN 'failed'
    WHEN a.state = 'cancelled' OR t.status = 'cancelled' THEN 'cancelled'
    ELSE 'awaiting_decision' END";

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AiRecordsQuery {
    /// Only records with this outcome.
    pub outcome: Option<AiRecordOutcome>,
    /// Page size, 1-100 (default 25).
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

/// One ticket that went through AI, for Admins.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiRecord {
    /// The attempt.
    pub id: Uuid,
    /// Open with `GET /api/tickets/{ticket_id}` for the intake and the suggestion.
    pub ticket_id: Uuid,
    pub reference: i64,
    pub subject: String,
    pub status: TicketStatus,
    pub requester: TicketRequester,
    pub outcome: AiRecordOutcome,
    pub attempt_state: AiAttemptState,
    /// Internal stable code of why there is no suggestion (docs/ai-attempts.md).
    #[schema(required = true)]
    pub outcome_code: Option<String>,
    pub provider: String,
    pub model: String,
    /// When the attempt was created (submission).
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    /// When the requester decided (`solved`: the confirmation time).
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub decided_at: Option<OffsetDateTime>,
}

#[derive(sqlx::FromRow)]
struct RecordRow {
    id: Uuid,
    ticket_id: Uuid,
    reference: i64,
    subject: String,
    status: String,
    requester_id: Uuid,
    requester_username: String,
    requester_display_name: String,
    outcome: String,
    attempt_state: String,
    outcome_code: Option<String>,
    provider: String,
    model: String,
    created_at: OffsetDateTime,
    decided_at: Option<OffsetDateTime>,
}

/// AI attempts and their outcomes, newest first (Admin only). This is the only
/// list that contains AI-solved records and tickets still in an AI stage.
#[utoipa::path(get, path = "/api/admin/ai/records", tag = "settings", params(AiRecordsQuery),
    responses(
        (status = 200, body = Page<AiRecord>),
        (status = 400, description = "`invalid_query`", body = crate::api::ErrorEnvelope),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn records(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AiRecordsQuery>,
) -> Result<Json<Page<AiRecord>>, ApiError> {
    require_permission(&user, Permission::ViewAllSubmissions)?;
    let page = Pagination {
        limit: query.limit,
        cursor: query.cursor,
    };
    let cursor = page.cursor()?;
    let rows: Vec<RecordRow> = sqlx::query_as(&format!(
        "SELECT * FROM (
             SELECT a.id, a.ticket_id, t.reference, t.subject, t.status::text AS status,
                    r.id AS requester_id, r.username AS requester_username,
                    r.display_name AS requester_display_name,
                    {OUTCOME_SQL} AS outcome, a.state AS attempt_state,
                    a.outcome AS outcome_code, a.provider, a.model, a.created_at,
                    d.created_at AS decided_at
             FROM ai_attempts a
             JOIN tickets t ON t.id = a.ticket_id
             JOIN accounts r ON r.id = t.requester_account_id
             LEFT JOIN ticket_events d ON d.ticket_id = a.ticket_id
                  AND d.event_type IN ('ai_solved', 'ai_not_solved')
         ) x
         WHERE ($1::text IS NULL OR x.outcome = $1)
           AND ($2::timestamptz IS NULL OR (x.created_at, x.id) < ($2, $3))
         ORDER BY x.created_at DESC, x.id DESC LIMIT $4"
    ))
    .bind(query.outcome.map(ident_str))
    .bind(cursor.map(|c| c.created_at))
    .bind(cursor.map(|c| c.id))
    .bind(page.limit() + 1)
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(AiRecord {
                id: row.id,
                ticket_id: row.ticket_id,
                reference: row.reference,
                subject: row.subject,
                status: ident(row.status)?,
                requester: TicketRequester {
                    id: row.requester_id,
                    username: row.requester_username,
                    display_name: row.requester_display_name,
                },
                outcome: ident(row.outcome)?,
                attempt_state: ident(row.attempt_state)?,
                outcome_code: row.outcome_code,
                provider: row.provider,
                model: row.model,
                created_at: row.created_at,
                decided_at: row.decided_at,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(Page::from_rows(items, page.limit(), |r| Cursor {
        created_at: r.created_at,
        id: r.id,
    })))
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AiOutcomesQuery {
    /// Reporting period in days, 1-3650 (default 30).
    pub days: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AiFailureCount {
    /// Stable attempt outcome code (docs/ai-attempts.md).
    pub outcome: String,
    pub count: i64,
}

/// AI outcome counts. Period counts come from the immutable ticket events, so a
/// denied or repeated request never moves them.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiOutcomeStats {
    pub days: i64,
    /// Requester-confirmed AI resolutions (`ai_solved` events).
    pub solved: i64,
    /// Escalated after a suggestion (`ai_not_solved` events).
    pub not_solved: i64,
    /// Requester skips: while processing (`skip_ai`) or at submission.
    pub skipped: i64,
    /// Handed off without a suggestion (`ai_failed` events).
    pub failed: i64,
    /// Submissions that bypassed AI because their category is exempt.
    pub category_exempt: i64,
    /// Cancelled by the requester during an AI stage.
    pub cancelled: i64,
    /// Tickets waiting for a suggestion right now (not bound to the period).
    pub processing: i64,
    /// Tickets with an undecided suggestion right now (not bound to the period).
    pub awaiting_decision: i64,
    /// `failed` by attempt outcome code, most frequent first.
    pub failures: Vec<AiFailureCount>,
}

/// AI outcome statistics (Admin only): solved, escalated after a suggestion,
/// skipped, and failed. No ticket content.
#[utoipa::path(get, path = "/api/admin/ai/outcomes", tag = "settings", params(AiOutcomesQuery),
    responses(
        (status = 200, body = AiOutcomeStats),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`days`: `out_of_range`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn outcomes(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AiOutcomesQuery>,
) -> Result<Json<AiOutcomeStats>, ApiError> {
    require_permission(&user, Permission::ViewReports)?;
    let days = query.days.unwrap_or(30);
    if !(1..=3650).contains(&days) {
        return Err(ApiError::Validation(FieldErrors::from([(
            "days".into(),
            vec!["out_of_range"],
        )])));
    }
    let period = i32::try_from(days).unwrap_or(3650);
    // One snapshot, so the counts agree with each other.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let (solved, not_solved, skipped, failed, category_exempt, cancelled): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE event_type = 'ai_solved'),
                count(*) FILTER (WHERE event_type = 'ai_not_solved'),
                count(*) FILTER (WHERE event_type = 'skip_ai'
                    OR (event_type = 'submit' AND payload->>'route' = 'requester_skip')),
                count(*) FILTER (WHERE event_type = 'ai_failed'),
                count(*) FILTER (WHERE event_type = 'submit'
                    AND payload->>'route' = 'category_exempt'),
                count(*) FILTER (WHERE event_type = 'cancel'
                    AND from_status IN ('ai_processing', 'awaiting_ai_decision'))
         FROM ticket_events
         WHERE created_at > now() - make_interval(days => $1)
           AND event_type IN ('ai_solved', 'ai_not_solved', 'skip_ai', 'ai_failed', 'submit', 'cancel')",
    )
    .bind(period)
    .fetch_one(&mut *tx)
    .await?;
    let (processing, awaiting_decision): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'ai_processing'),
                count(*) FILTER (WHERE status = 'awaiting_ai_decision')
         FROM tickets WHERE status IN ('ai_processing', 'awaiting_ai_decision')",
    )
    .fetch_one(&mut *tx)
    .await?;
    let failures: Vec<(String, i64)> = sqlx::query_as(
        "SELECT outcome, count(*) FROM ai_attempts
         WHERE state = 'handed_off' AND outcome <> 'requester_skip'
           AND finished_at > now() - make_interval(days => $1)
         GROUP BY outcome ORDER BY 2 DESC, 1",
    )
    .bind(period)
    .fetch_all(&mut *tx)
    .await?;
    Ok(Json(AiOutcomeStats {
        days,
        solved,
        not_solved,
        skipped,
        failed,
        category_exempt,
        cancelled,
        processing,
        awaiting_decision,
        failures: failures
            .into_iter()
            .map(|(outcome, count)| AiFailureCount { outcome, count })
            .collect(),
    }))
}
