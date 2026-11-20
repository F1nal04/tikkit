//! Admin submission view: every submitted ticket, whatever became of it (still
//! in AI, AI-solved, escalated, resolved by operators, cancelled), with its
//! intake, AI attempt, and full event history. Admin only; read-only. Both
//! handlers run in a `READ ONLY` transaction, so viewing can never assign,
//! reopen, notify, or otherwise change anything. See docs/admin-submissions.md.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    ai::AiErrorKind,
    ai_attempts::{self, AiAttemptState, AiDecision, AiSuggestion},
    ai_records::{AiRecordOutcome, OUTCOME_SQL},
    api::{ApiError, Cursor, Json, Language, Page, Pagination, Path, Query, TicketStatus},
    auth::{CurrentUser, ident, ident_str},
    authz::{Permission, require_permission},
    lifecycle::{ClosureReason, ResolutionSource},
    submission::SubmittedIntake,
    tickets::TicketRequester,
};

/// How a submission ended, from its status and closure provenance. Only
/// `ai_confirmed` is an AI resolution; an undecided or abandoned suggestion is
/// `open` or `cancelled`, never solved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionResolution {
    /// Not finished: in AI, waiting for a decision, or with the help desk.
    Open,
    /// The requester explicitly confirmed the AI suggestion (`ai_solved`, final).
    AiConfirmed,
    /// The requester confirmed an operator's proposed resolution.
    RequesterConfirmed,
    /// Closed by the confirmation timeout: the requester did not answer an
    /// operator's proposal. Not a confirmation.
    ConfirmationTimeout,
    /// Cancelled by the requester.
    Cancelled,
}

/// How the submission entered: the `route` of its `submit` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionRoute {
    /// Sent to AI first.
    Ai,
    /// AI was off or not configured: straight to the general queue.
    AiDisabled,
    /// The category skips AI: straight to the general queue, no provider call.
    CategoryExempt,
    /// The requester skipped AI at submission.
    RequesterSkip,
}

/// `resolution_source` filter: `none` is every submission without one (open
/// or cancelled).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSourceFilter {
    Ai,
    Operator,
    None,
}

/// `ai_outcome` filter: an [`AiRecordOutcome`], or `none` for submissions that
/// never had an AI attempt.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiOutcomeFilter {
    None,
    Processing,
    AwaitingDecision,
    Solved,
    NotSolved,
    Skipped,
    Failed,
    Cancelled,
}

/// Mutually exclusive AI reporting buckets, derived from the Admin attempt
/// outcome and the recorded submission route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiDisposition {
    Solved,
    NotSolved,
    ProviderFailed,
    Failed,
    MissingAttempt,
    PolicyWithdrawn,
    Processing,
    AwaitingDecision,
    Cancelled,
    CategoryExempt,
    AiDisabled,
    RequesterSkip,
    Unknown,
}

impl AiDisposition {
    pub(crate) fn completed_attempt(self) -> bool {
        matches!(
            self,
            Self::Solved | Self::NotSolved | Self::ProviderFailed | Self::Failed
        )
    }

    pub(crate) fn attempted(self) -> bool {
        self.completed_attempt()
            || matches!(
                self,
                Self::Processing | Self::AwaitingDecision | Self::Cancelled
            )
    }
}

// Queue participation is historical and can differ from the current status.
pub(crate) const ENTERED_QUEUE_SQL: &str =
    "EXISTS (SELECT 1 FROM ticket_events e WHERE e.ticket_id = x.id AND e.to_status = 'queued')";

fn ai_disposition_sql() -> String {
    let provider_cases = AiErrorKind::ALL
        .iter()
        .filter_map(|kind| {
            let evidence = match kind {
                AiErrorKind::InvalidInput | AiErrorKind::Cancelled => return None,
                AiErrorKind::Timeout => "AND x.failed_usage",
                AiErrorKind::DestinationBlocked => {
                    "AND x.failed_usage AND x.usage_http_status BETWEEN 300 AND 399"
                }
                AiErrorKind::InvalidCredentials
                | AiErrorKind::RateLimited
                | AiErrorKind::ModelError
                | AiErrorKind::UpstreamUnavailable
                | AiErrorKind::MalformedOutput
                | AiErrorKind::OversizedOutput
                | AiErrorKind::Vision => "",
            };
            Some(format!(
                "WHEN x.attempt_outcome = '{}' {evidence} THEN 'provider_failed'",
                kind.as_str()
            ))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let withdrawals = [
        ai_attempts::AI_DISABLED,
        ai_attempts::CATEGORY_EXEMPT,
        ai_attempts::CONFIGURATION_CHANGED,
        ai_attempts::CREDENTIAL_CHANGED,
        ai_attempts::CREDENTIAL_MISSING,
        "credential_unreadable",
        "model_unsupported",
    ]
    .join("','");

    format!(
        "CASE
        WHEN x.ai_outcome = 'failed' THEN CASE
            WHEN x.attempt_outcome IN ('{withdrawals}') THEN 'policy_withdrawn'
            {provider_cases}
            ELSE 'failed' END
        WHEN x.ai_outcome = 'skipped' THEN 'requester_skip'
        WHEN x.ai_outcome IS NOT NULL THEN x.ai_outcome
        WHEN x.route = 'ai' AND EXISTS (
             SELECT 1 FROM ticket_events e WHERE e.ticket_id = x.id AND e.event_type = 'ai_failed'
        ) THEN 'missing_attempt'
        WHEN x.route IN ('category_exempt', 'ai_disabled', 'requester_skip') THEN x.route
        ELSE 'unknown' END"
    )
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SubmissionsQuery {
    /// The ticket's (frozen) category, archived ones included.
    pub category_id: Option<Uuid>,
    /// Current destination team, including archived teams.
    pub team_id: Option<Uuid>,
    /// Current assignee account.
    pub assignee_id: Option<Uuid>,
    /// Whether a recorded transition entered the shared queue.
    pub entered_queue: Option<bool>,
    /// Current effective priority.
    pub priority_id: Option<Uuid>,
    /// Current closure classification used in Admin reports.
    pub resolution: Option<SubmissionResolution>,
    /// AI reporting bucket, including bypass routes with no attempt.
    pub ai_disposition: Option<AiDisposition>,
    /// Current ticket status. `draft` is not a submission and is rejected.
    pub status: Option<TicketStatus>,
    pub resolution_source: Option<ResolutionSourceFilter>,
    pub ai_outcome: Option<AiOutcomeFilter>,
    /// Inclusive submission time, RFC 3339.
    pub created_from: Option<String>,
    /// Exclusive submission time, RFC 3339.
    pub created_before: Option<String>,
    /// Page size, 1-100 (default 25).
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

/// One submission in the Admin list. No ticket text beyond the subject.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubmissionSummary {
    pub id: Uuid,
    pub reference: i64,
    pub subject: String,
    pub status: TicketStatus,
    pub category_id: Uuid,
    /// The category name frozen at submission.
    pub category_name: String,
    /// Current effective priority.
    pub priority_name: String,
    pub requester: TicketRequester,
    /// `null`: the shared general queue, or never human work.
    #[schema(required = true)]
    pub team_name: Option<String>,
    #[schema(required = true)]
    pub assignee_display_name: Option<String>,
    /// Set on `closed` (`operator`) and `ai_solved` (`ai`) tickets.
    #[schema(required = true)]
    pub resolution_source: Option<ResolutionSource>,
    #[schema(required = true)]
    pub closure_reason: Option<ClosureReason>,
    pub resolution: SubmissionResolution,
    /// `null` for records predating the submission event.
    #[schema(required = true)]
    pub route: Option<SubmissionRoute>,
    /// What became of the AI attempt; `null` when there never was one.
    #[schema(required = true)]
    pub ai_outcome: Option<AiRecordOutcome>,
    /// Submission time.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

/// The ticket's AI attempt as Admins see it: the stored suggestion, the
/// internal outcome code, and the requester's decision.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubmissionAi {
    pub attempt_id: Uuid,
    pub outcome: AiRecordOutcome,
    pub state: AiAttemptState,
    /// Internal stable code of why there is no suggestion (docs/ai-attempts.md).
    #[schema(required = true)]
    pub outcome_code: Option<String>,
    pub provider: String,
    pub model: String,
    pub language: Language,
    /// The one stored suggestion, untrusted model text: render as plain text.
    #[schema(required = true)]
    pub suggestion: Option<AiSuggestion>,
    #[schema(required = true)]
    pub decision: Option<AiDecision>,
    /// The decision event in `events` (for `solved`: the confirmation event).
    #[schema(required = true)]
    pub decision_event_id: Option<i64>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub decided_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub finished_at: Option<OffsetDateTime>,
    /// Retention removed the stored suggestion; `suggestion` is then `null`.
    pub content_removed: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventActor {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// One immutable ticket event: transitions, assignment and priority changes,
/// and message events (which carry the message ID only, never its text).
#[derive(Debug, Serialize, ToSchema)]
pub struct SubmissionEvent {
    pub id: i64,
    pub event_type: String,
    /// Set on status transitions only.
    #[schema(required = true)]
    pub from_status: Option<TicketStatus>,
    #[schema(required = true)]
    pub to_status: Option<TicketStatus>,
    /// `null` for background jobs.
    #[schema(required = true)]
    pub actor: Option<EventActor>,
    #[schema(value_type = Object)]
    pub payload: Value,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

/// One submission in full, read-only.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubmissionDetail {
    pub summary: SubmissionSummary,
    pub description: String,
    /// Frozen original intake; null only for legacy records predating submission.
    /// `screenshot_ids` lists only screenshots that still exist.
    #[schema(required = true)]
    pub intake_snapshot: Option<SubmittedIntake>,
    /// Intake screenshots the retention policy has removed.
    pub removed_screenshots: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub confirmation_deadline: Option<OffsetDateTime>,
    #[schema(required = true)]
    pub ai: Option<SubmissionAi>,
    /// Set when retention removed the ticket's text; the text fields then hold `—`.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub content_removed_at: Option<OffsetDateTime>,
    /// Every event of the ticket, oldest first.
    pub events: Vec<SubmissionEvent>,
}

/// `tickets t` (with `ai_attempts a` and the decision event `d` for
/// [`OUTCOME_SQL`]) to one [`SubmissionResolution`].
const RESOLUTION_SQL: &str = "CASE
    WHEN t.status = 'ai_solved' THEN 'ai_confirmed'
    WHEN t.status = 'closed' AND t.closure_reason = 'confirmation_timeout' THEN 'confirmation_timeout'
    WHEN t.status = 'closed' THEN 'requester_confirmed'
    WHEN t.status = 'cancelled' THEN 'cancelled'
    ELSE 'open' END";

/// Every submitted ticket as one row `x` of summary columns; `filter` is the
/// rest of the query (conditions on `x`, order, limit).
pub(crate) fn summaries(filter: &str) -> String {
    format!(
        "SELECT * FROM (SELECT x.*, {ai_disposition} AS ai_disposition,
                           {ENTERED_QUEUE_SQL} AS entered_queue FROM (
             SELECT t.id, t.reference, t.subject, t.status::text AS status, t.category_id,
                    COALESCE(t.intake_snapshot->'catalog'->>'category_name', c.name) AS category_name,
                    p.id AS priority_id, p.name AS priority_name, r.id AS requester_id,
                    r.username AS requester_username, r.display_name AS requester_display_name,
                    tm.id AS team_id, tm.name AS team_name, asg.id AS assignee_id,
                    asg.display_name AS assignee_display_name, asg.role::text AS assignee_role,
                    t.resolution_source, t.closure_reason, {RESOLUTION_SQL} AS resolution,
                    s.route, CASE WHEN a.id IS NOT NULL THEN {OUTCOME_SQL} END AS ai_outcome,
                    a.outcome AS attempt_outcome,
                    (u.status = 'failed' AND u.outcome = a.outcome) AS failed_usage,
                    u.http_status AS usage_http_status,
                    t.created_at, t.updated_at
             FROM tickets t
             JOIN categories c ON c.id = t.category_id
             JOIN priorities p ON p.id = t.priority_id
             JOIN accounts r ON r.id = t.requester_account_id
             LEFT JOIN teams tm ON tm.id = t.team_id
             LEFT JOIN accounts asg ON asg.id = t.assignee_account_id
             LEFT JOIN ai_attempts a ON a.ticket_id = t.id
             LEFT JOIN ai_usage u ON u.id = a.usage_id
             LEFT JOIN ticket_events d ON d.ticket_id = t.id
                  AND d.event_type IN ('ai_solved', 'ai_not_solved')
             LEFT JOIN LATERAL (
                 SELECT e.payload->>'route' AS route FROM ticket_events e
                 WHERE e.ticket_id = t.id AND e.event_type = 'submit' ORDER BY e.id LIMIT 1
             ) s ON true
             WHERE t.status <> 'draft'
         ) x) x {filter}",
        ai_disposition = ai_disposition_sql(),
    )
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    reference: i64,
    subject: String,
    status: String,
    category_id: Uuid,
    category_name: String,
    priority_name: String,
    requester_id: Uuid,
    requester_username: String,
    requester_display_name: String,
    team_name: Option<String>,
    assignee_display_name: Option<String>,
    resolution_source: Option<String>,
    closure_reason: Option<String>,
    resolution: String,
    route: Option<String>,
    ai_outcome: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<SummaryRow> for SubmissionSummary {
    type Error = ApiError;

    fn try_from(row: SummaryRow) -> Result<Self, ApiError> {
        Ok(Self {
            id: row.id,
            reference: row.reference,
            subject: row.subject,
            status: ident(row.status)?,
            category_id: row.category_id,
            category_name: row.category_name,
            priority_name: row.priority_name,
            requester: TicketRequester {
                id: row.requester_id,
                username: row.requester_username,
                display_name: row.requester_display_name,
            },
            team_name: row.team_name,
            assignee_display_name: row.assignee_display_name,
            resolution_source: row.resolution_source.map(ident).transpose()?,
            closure_reason: row.closure_reason.map(ident).transpose()?,
            resolution: ident(row.resolution)?,
            route: row.route.map(ident).transpose()?,
            ai_outcome: row.ai_outcome.map(ident).transpose()?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

pub(crate) fn instant(value: Option<String>) -> Result<Option<OffsetDateTime>, ApiError> {
    value
        .map(|s| {
            OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
                .map_err(|_| ApiError::InvalidQuery)
        })
        .transpose()
}

/// Read-only snapshot: nothing a handler here runs can write.
pub(crate) async fn read_only(
    state: &AppState,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Every submitted ticket, newest first (Admin only): AI-stage and AI-solved
/// records, escalated tickets, operator resolutions, and cancellations. Drafts
/// are not submissions and never listed. Filters apply before the page limit.
#[utoipa::path(get, path = "/api/admin/submissions", tag = "admin", params(SubmissionsQuery),
    responses(
        (status = 200, body = Page<SubmissionSummary>),
        (status = 400, description = "`invalid_query` (unknown filter value, `status=draft`, bad or empty date range)", body = crate::api::ErrorEnvelope),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`cursor`: `invalid_cursor`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn list_submissions(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<SubmissionsQuery>,
) -> Result<Json<Page<SubmissionSummary>>, ApiError> {
    require_permission(&user, Permission::ViewAllSubmissions)?;
    if query.status == Some(TicketStatus::Draft) {
        return Err(ApiError::InvalidQuery);
    }
    let from = instant(query.created_from)?;
    let before = instant(query.created_before)?;
    if from.zip(before).is_some_and(|(a, b)| a >= b) {
        return Err(ApiError::InvalidQuery);
    }
    let page = Pagination {
        limit: query.limit,
        cursor: query.cursor,
    };
    let cursor = page.cursor()?;
    let mut tx = read_only(&state).await?;
    let rows: Vec<SummaryRow> = sqlx::query_as(&summaries(
        "WHERE ($1::uuid IS NULL OR x.category_id = $1)
           AND ($2::text IS NULL OR x.status = $2)
           AND ($3::text IS NULL OR COALESCE(x.resolution_source, 'none') = $3)
           AND ($4::text IS NULL OR COALESCE(x.ai_outcome, 'none') = $4)
           AND ($5::timestamptz IS NULL OR x.created_at >= $5)
           AND ($6::timestamptz IS NULL OR x.created_at < $6)
           AND ($7::timestamptz IS NULL OR (x.created_at, x.id) < ($7, $8))
           AND ($10::uuid IS NULL OR x.team_id = $10)
           AND ($11::uuid IS NULL OR x.priority_id = $11)
           AND ($12::text IS NULL OR x.resolution = $12)
           AND ($13::text IS NULL OR x.ai_disposition = $13)
           AND ($14::uuid IS NULL OR x.assignee_id = $14)
           AND ($15::boolean IS NULL OR x.entered_queue = $15)
         ORDER BY x.created_at DESC, x.id DESC LIMIT $9",
    ))
    .bind(query.category_id)
    .bind(query.status.map(ident_str))
    .bind(query.resolution_source.map(ident_str))
    .bind(query.ai_outcome.map(ident_str))
    .bind(from)
    .bind(before)
    .bind(cursor.map(|c| c.created_at))
    .bind(cursor.map(|c| c.id))
    .bind(page.limit() + 1)
    .bind(query.team_id)
    .bind(query.priority_id)
    .bind(query.resolution.map(ident_str))
    .bind(query.ai_disposition.map(ident_str))
    .bind(query.assignee_id)
    .bind(query.entered_queue)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let items = rows
        .into_iter()
        .map(SubmissionSummary::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(Page::from_rows(items, page.limit(), |s| Cursor {
        created_at: s.created_at,
        id: s.id,
    })))
}

type AttemptRow = (
    Uuid,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<sqlx::types::Json<Vec<String>>>,
    OffsetDateTime,
    Option<OffsetDateTime>,
    Option<String>,
    Option<i64>,
    Option<OffsetDateTime>,
    bool,
);

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    event_type: String,
    from_status: Option<String>,
    to_status: Option<String>,
    actor_id: Option<Uuid>,
    actor_username: Option<String>,
    actor_display_name: Option<String>,
    payload: Value,
    created_at: OffsetDateTime,
}

/// One submission with its frozen intake, AI attempt (suggestion, internal
/// outcome code, decision and confirmation event), and every event (Admin
/// only). Read-only: there is no action here, and AI-solved records stay final.
#[utoipa::path(get, path = "/api/admin/submissions/{id}", tag = "admin",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = SubmissionDetail),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (missing, or a draft)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn submission_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SubmissionDetail>, ApiError> {
    require_permission(&user, Permission::ViewAllSubmissions)?;
    let mut tx = read_only(&state).await?;
    let summary: SummaryRow = sqlx::query_as(&summaries("WHERE x.id = $1"))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (description, intake_snapshot, confirmation_deadline, content_removed_at): (
        String,
        Option<sqlx::types::Json<SubmittedIntake>>,
        Option<OffsetDateTime>,
        Option<OffsetDateTime>,
    ) = sqlx::query_as(
        "SELECT description, intake_snapshot, confirmation_deadline, content_purged_at
         FROM tickets WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let mut intake_snapshot = intake_snapshot.map(|s| s.0);
    let removed_screenshots = match &mut intake_snapshot {
        Some(intake) => {
            crate::screenshots::existing_intake(&mut tx, id, &mut intake.screenshot_ids).await?
        }
        None => 0,
    };
    let attempt: Option<AttemptRow> = sqlx::query_as(&format!(
        "SELECT a.id, {OUTCOME_SQL}, a.state, a.outcome, a.provider, a.model, a.language, a.explanation,
                a.steps, a.created_at, a.finished_at, d.event_type, d.id, d.created_at,
                a.content_purged_at IS NOT NULL
         FROM ai_attempts a
         JOIN tickets t ON t.id = a.ticket_id
         LEFT JOIN ticket_events d ON d.ticket_id = a.ticket_id
              AND d.event_type IN ('ai_solved', 'ai_not_solved')
         WHERE a.ticket_id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let events: Vec<EventRow> = sqlx::query_as(
        "SELECT e.id, e.event_type, e.from_status::text AS from_status,
                e.to_status::text AS to_status, a.id AS actor_id, a.username AS actor_username,
                a.display_name AS actor_display_name, e.payload, e.created_at
         FROM ticket_events e LEFT JOIN accounts a ON a.id = e.actor_account_id
         WHERE e.ticket_id = $1 ORDER BY e.id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let ai = match attempt {
        Some((
            attempt_id,
            outcome,
            state,
            outcome_code,
            provider,
            model,
            language,
            explanation,
            steps,
            created_at,
            finished_at,
            decision,
            decision_event_id,
            decided_at,
            content_removed,
        )) => Some(SubmissionAi {
            attempt_id,
            outcome: ident(outcome)?,
            state: ident(state)?,
            outcome_code,
            provider,
            model,
            language: ident(language)?,
            suggestion: explanation.zip(steps).filter(|_| !content_removed).map(
                |(explanation, steps)| AiSuggestion {
                    explanation,
                    steps: steps.0,
                },
            ),
            decision: decision.map(|event| match event.as_str() {
                "ai_solved" => AiDecision::Solved,
                _ => AiDecision::NotSolved,
            }),
            decision_event_id,
            decided_at,
            created_at,
            finished_at,
            content_removed,
        }),
        None => None,
    };
    let events = events
        .into_iter()
        .map(|e| {
            Ok(SubmissionEvent {
                id: e.id,
                event_type: e.event_type,
                from_status: e.from_status.map(ident).transpose()?,
                to_status: e.to_status.map(ident).transpose()?,
                actor: match (e.actor_id, e.actor_username, e.actor_display_name) {
                    (Some(id), Some(username), Some(display_name)) => Some(EventActor {
                        id,
                        username,
                        display_name,
                    }),
                    _ => None,
                },
                payload: e.payload,
                created_at: e.created_at,
            })
        })
        .collect::<Result<_, ApiError>>()?;
    Ok(Json(SubmissionDetail {
        summary: summary.try_into()?,
        description,
        intake_snapshot,
        removed_screenshots,
        confirmation_deadline,
        ai,
        content_removed_at,
        events,
    }))
}
