//! Admin submission-cohort reporting and bounded metadata export.

use axum::{extract::State, http::header, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, PgConnection, Postgres, query::QueryAs};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, Json, Query, Role},
    audit::{self, Entry, Who},
    auth::{CurrentUser, ident},
    authz::{Permission, require_permission},
    csv,
    submissions::{self, AiDisposition, SubmissionResolution},
};

const MAX_REPORT_DAYS: i64 = 366;
const MAX_EXPORT_ROWS: i64 = 10_000;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReportQuery {
    /// Inclusive submission time, RFC 3339.
    #[param(required = true)]
    pub created_from: Option<String>,
    /// Exclusive submission time, RFC 3339.
    #[param(required = true)]
    pub created_before: Option<String>,
    /// Frozen category ID.
    pub category_id: Option<Uuid>,
    /// Current destination team ID.
    pub team_id: Option<Uuid>,
}

struct Scope {
    from: OffsetDateTime,
    before: OffsetDateTime,
    category_id: Option<Uuid>,
    team_id: Option<Uuid>,
}

impl TryFrom<ReportQuery> for Scope {
    type Error = ApiError;

    fn try_from(query: ReportQuery) -> Result<Self, ApiError> {
        let from = submissions::instant(query.created_from)?.ok_or(ApiError::InvalidQuery)?;
        let before = submissions::instant(query.created_before)?.ok_or(ApiError::InvalidQuery)?;
        if from >= before || before - from > Duration::days(MAX_REPORT_DAYS) {
            return Err(ApiError::InvalidQuery);
        }
        Ok(Self {
            from,
            before,
            category_id: query.category_id,
            team_id: query.team_id,
        })
    }
}

impl Scope {
    fn bind<'q, T>(
        &self,
        query: QueryAs<'q, Postgres, T, sqlx::postgres::PgArguments>,
    ) -> QueryAs<'q, Postgres, T, sqlx::postgres::PgArguments> {
        query
            .bind(self.from)
            .bind(self.before)
            .bind(self.category_id)
            .bind(self.team_id)
    }
}

#[derive(Debug, FromRow, Serialize, ToSchema)]
pub struct DailyVolume {
    /// Calendar date in UTC.
    #[schema(format = Date)]
    pub day: String,
    pub count: i64,
}

#[derive(Debug, FromRow, Serialize, ToSchema)]
pub struct NamedVolume {
    pub id: Uuid,
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AiVolume {
    pub outcome: AiDisposition,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ClosureVolume {
    pub resolution: SubmissionResolution,
    pub count: i64,
}

#[derive(Debug, FromRow, Serialize, ToSchema)]
pub struct OperatorTotals {
    pub queued: i64,
    pub first_response_samples: i64,
    #[schema(required = true)]
    pub mean_first_response_seconds: Option<f64>,
    pub initial_closures: i64,
    pub initial_requester_confirmed: i64,
    pub initial_confirmation_timeout: i64,
    pub subsequent_closures: i64,
    pub reopens: i64,
    pub rejected_proposals: i64,
    pub time_to_proposal_samples: i64,
    #[schema(required = true)]
    pub mean_queue_to_proposal_seconds: Option<f64>,
    pub confirmation_wait_samples: i64,
    #[schema(required = true)]
    pub mean_proposal_to_closure_seconds: Option<f64>,
    pub resolution_samples: i64,
    #[schema(required = true)]
    pub mean_queue_to_closure_seconds: Option<f64>,
}

#[derive(Debug, FromRow, Serialize, ToSchema)]
pub struct Workload {
    #[schema(required = true)]
    pub team_id: Option<Uuid>,
    #[schema(required = true)]
    pub team_name: Option<String>,
    #[schema(required = true)]
    pub assignee_id: Option<Uuid>,
    #[schema(required = true)]
    pub assignee_name: Option<String>,
    #[schema(value_type = Option<Role>, required = true)]
    pub assignee_role: Option<String>,
    pub queued: i64,
    pub active: i64,
    pub awaiting_confirmation: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Report {
    pub submissions: i64,
    pub daily: Vec<DailyVolume>,
    pub categories: Vec<NamedVolume>,
    pub priorities: Vec<NamedVolume>,
    pub ai_outcomes: Vec<AiVolume>,
    pub ai_attempts: i64,
    /// Explicit AI confirmations. Null with a team filter.
    #[schema(required = true)]
    pub ai_rate_numerator: Option<i64>,
    /// Solved, not-solved, and recorded failures. Null with a team filter.
    #[schema(required = true)]
    pub ai_rate_denominator: Option<i64>,
    /// Explicit AI confirmations. Null with a team filter.
    #[schema(required = true)]
    pub deflection_rate_numerator: Option<i64>,
    /// Submissions excluding cancellations and policy withdrawals. Null with a team filter.
    #[schema(required = true)]
    pub deflection_rate_denominator: Option<i64>,
    pub closures: Vec<ClosureVolume>,
    pub awaiting_confirmation: i64,
    pub operator: OperatorTotals,
    pub workload: Vec<Workload>,
}

fn scoped(alias: &str) -> String {
    format!(
        "WHERE {alias}.created_at >= $1 AND {alias}.created_at < $2
           AND ($3::uuid IS NULL OR {alias}.category_id = $3)
           AND ($4::uuid IS NULL OR {alias}.team_id = $4)"
    )
}

fn cohort(limit: &str) -> String {
    submissions::summaries(&format!("{} {limit}", scoped("x")))
}

// The first queue entry and first human closure come from immutable transition
// history, so reassignments, rejected proposals and later reopens cannot reset
// the clocks. The proposal is matched to that closure by version.
fn facts(cohort: &str) -> String {
    format!(
        "WITH cohort AS ({cohort}), facts AS (
            SELECT c.*, e.*, first_close.closed_at AS first_closed_at,
                   first_close.reason AS first_closure_reason,
                   p.created_at AS first_closure_proposed_at,
                   CASE WHEN c.status = 'closed' THEN e.latest_closed_at END AS closed_at,
                   CASE WHEN c.resolution = 'requester_confirmed' THEN e.latest_confirmed_at END AS requester_confirmed_at,
                   LEAST(e.first_staff_transition_at, msg.first_reply_at) AS first_response_at
            FROM cohort c
            CROSS JOIN LATERAL (
                SELECT min(created_at) FILTER (WHERE to_status = 'queued') AS queued_at,
                       max(created_at) FILTER (WHERE event_type = 'propose_resolution') AS latest_proposed_at,
                       max(created_at) FILTER (WHERE event_type = 'ai_solved') AS ai_confirmed_at,
                       max(created_at) FILTER (WHERE event_type = 'cancel') AS cancelled_at,
                       max(created_at) FILTER (WHERE event_type = 'accept_resolution') AS latest_confirmed_at,
                       max(created_at) FILTER (WHERE to_status = 'closed') AS latest_closed_at,
                       count(*) FILTER (WHERE from_status = 'closed' AND event_type = 'reopen') AS reopens,
                       count(*) FILTER (WHERE to_status = 'closed') AS closure_events,
                       count(*) FILTER (WHERE event_type = 'reject_resolution') AS rejected_proposals,
                       min(created_at) FILTER (WHERE event_type IN ('request_information', 'propose_resolution')) AS first_staff_transition_at
                FROM ticket_events WHERE ticket_id = c.id
            ) e
            LEFT JOIN LATERAL (
                SELECT created_at AS closed_at, payload->>'proposal_version' AS version,
                       payload->>'closure_reason' AS reason
                FROM ticket_events WHERE ticket_id = c.id AND to_status = 'closed'
                ORDER BY id LIMIT 1
            ) first_close ON e.queued_at IS NOT NULL
            LEFT JOIN LATERAL (
                SELECT created_at FROM ticket_events
                WHERE ticket_id = c.id AND event_type = 'propose_resolution'
                  AND payload->>'proposal_version' = first_close.version
                ORDER BY id LIMIT 1
            ) p ON true
            LEFT JOIN LATERAL (
                SELECT min(created_at) AS first_reply_at FROM ticket_messages
                WHERE ticket_id = c.id AND kind = 'public_reply' AND author_kind = 'staff'
                  AND created_at >= e.queued_at
            ) msg ON e.queued_at IS NOT NULL
        )"
    )
}

async fn report(db: &mut PgConnection, scope: &Scope) -> Result<Report, ApiError> {
    let cohort = cohort("");
    let grouped: Vec<(String, String, String, i64)> = scope
        .bind(sqlx::query_as(&format!(
            "SELECT ai_disposition, resolution, status, count(*)
         FROM ({cohort}) c GROUP BY 1, 2, 3"
        )))
        .fetch_all(&mut *db)
        .await?;

    let mut ai_counts = std::collections::BTreeMap::new();
    let mut closure_counts = std::collections::BTreeMap::new();
    let (
        mut submissions,
        mut ai_attempts,
        mut ai_rate_denominator,
        mut deflection_rate_denominator,
        mut awaiting_confirmation,
    ) = (0, 0, 0, 0, 0);
    for (ai, resolution, status, count) in grouped {
        let ai: AiDisposition = ident(ai)?;
        submissions += count;
        if ai.attempted() {
            ai_attempts += count;
        }
        if ai.completed_attempt() {
            ai_rate_denominator += count;
        }
        if resolution != "cancelled" && ai != AiDisposition::PolicyWithdrawn {
            deflection_rate_denominator += count;
        }
        if status == "awaiting_confirmation" {
            awaiting_confirmation += count;
        }
        *ai_counts.entry(ai).or_insert(0) += count;
        *closure_counts.entry(resolution).or_insert(0) += count;
    }
    let solved = ai_counts.get(&AiDisposition::Solved).copied().unwrap_or(0);
    let ai_outcomes = ai_counts
        .into_iter()
        .map(|(outcome, count)| AiVolume { outcome, count })
        .collect();
    let closures = closure_counts
        .into_iter()
        .map(|(key, count)| {
            Ok(ClosureVolume {
                resolution: ident(key)?,
                count,
            })
        })
        .collect::<Result<_, ApiError>>()?;

    // Volume aggregates only need ticket scope and catalog names. Avoid
    // repeating attempt, event and account lookups from the detail cohort.
    let volume = format!(
        "SELECT t.created_at, t.category_id, t.priority_id
         FROM tickets t {} AND t.status <> 'draft'",
        scoped("t")
    );
    let daily = scope
        .bind(sqlx::query_as(&format!(
            "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS day, count(*)
         FROM ({volume}) c GROUP BY 1 ORDER BY 1"
        )))
        .fetch_all(&mut *db)
        .await?;
    let categories = scope.bind(sqlx::query_as(&format!(
        "SELECT cat.id, cat.name, count(*) FROM ({volume}) c JOIN categories cat ON cat.id = c.category_id
         GROUP BY cat.id, cat.name ORDER BY cat.name, cat.id"
    ))).fetch_all(&mut *db).await?;
    let priorities = scope
        .bind(sqlx::query_as(&format!(
            "SELECT p.id, p.name, count(*) FROM ({volume}) c JOIN priorities p ON p.id = c.priority_id
         GROUP BY 1, 2 ORDER BY 2, 1"
        )))
        .fetch_all(&mut *db)
        .await?;

    let operator = scope.bind(sqlx::query_as(&format!(
        "{} SELECT count(*) AS queued,
            count(first_response_at) FILTER (WHERE first_response_at >= queued_at) AS first_response_samples,
            avg(extract(epoch FROM first_response_at - queued_at)) FILTER (WHERE first_response_at >= queued_at)::float8 AS mean_first_response_seconds,
            count(first_closed_at) AS initial_closures,
            count(*) FILTER (WHERE first_closure_reason = 'requester_confirmed') AS initial_requester_confirmed,
            count(*) FILTER (WHERE first_closure_reason = 'confirmation_timeout') AS initial_confirmation_timeout,
            coalesce(sum(greatest(closure_events - 1, 0)), 0)::bigint AS subsequent_closures,
            coalesce(sum(reopens), 0)::bigint AS reopens,
            coalesce(sum(rejected_proposals), 0)::bigint AS rejected_proposals,
            count(first_closure_proposed_at) FILTER (WHERE first_closure_proposed_at BETWEEN queued_at AND first_closed_at) AS time_to_proposal_samples,
            avg(extract(epoch FROM first_closure_proposed_at - queued_at))
                FILTER (WHERE first_closure_proposed_at BETWEEN queued_at AND first_closed_at)::float8 AS mean_queue_to_proposal_seconds,
            count(first_closure_proposed_at) FILTER (WHERE first_closure_proposed_at BETWEEN queued_at AND first_closed_at) AS confirmation_wait_samples,
            avg(extract(epoch FROM first_closed_at - first_closure_proposed_at))
                FILTER (WHERE first_closure_proposed_at BETWEEN queued_at AND first_closed_at)::float8 AS mean_proposal_to_closure_seconds,
            count(first_closed_at) FILTER (WHERE first_closed_at >= queued_at) AS resolution_samples,
            avg(extract(epoch FROM first_closed_at - queued_at)) FILTER (WHERE first_closed_at >= queued_at)::float8 AS mean_queue_to_closure_seconds
         FROM facts WHERE queued_at IS NOT NULL",
        facts(&cohort)
    ))).fetch_one(&mut *db).await?;

    let workload = scope.bind(sqlx::query_as(&format!(
        "SELECT team_id, team_name, assignee_id, assignee_display_name AS assignee_name,
                assignee_role,
                count(*) AS queued,
                count(*) FILTER (WHERE resolution = 'open') AS active,
                count(*) FILTER (WHERE status = 'awaiting_confirmation') AS awaiting_confirmation
         FROM ({cohort}) c
         WHERE entered_queue
         GROUP BY 1, 2, 3, 4, 5 ORDER BY team_name NULLS FIRST, assignee_name NULLS FIRST, team_id, assignee_id"
    ))).fetch_all(db).await?;

    let rates_available = scope.team_id.is_none();
    Ok(Report {
        submissions,
        daily,
        categories,
        priorities,
        ai_outcomes,
        ai_attempts,
        ai_rate_numerator: rates_available.then_some(solved),
        ai_rate_denominator: rates_available.then_some(ai_rate_denominator),
        deflection_rate_numerator: rates_available.then_some(solved),
        deflection_rate_denominator: rates_available.then_some(deflection_rate_denominator),
        closures,
        awaiting_confirmation,
        operator,
        workload,
    })
}

#[utoipa::path(get, path = "/api/admin/reports", tag = "admin", params(ReportQuery),
    responses((status = 200, body = Report),
        (status = 400, description = "Missing, reversed, or over-366-day range", body = crate::api::ErrorEnvelope),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, body = crate::api::ErrorEnvelope)))]
pub async fn get_report(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Report>, ApiError> {
    require_permission(&user, Permission::ViewReports)?;
    let scope = Scope::try_from(query)?;
    let mut tx = submissions::read_only(&state).await?;
    // JIT added hundreds of milliseconds to the reference report request.
    // Keep this setting local to the report transaction.
    sqlx::query("SET LOCAL jit = off").execute(&mut *tx).await?;
    let result = report(&mut tx, &scope).await?;
    tx.commit().await?;
    Ok(Json(result))
}

#[derive(FromRow)]
struct ExportRow {
    reference: i64,
    created_at: OffsetDateTime,
    category_name: String,
    priority_name: String,
    team_name: Option<String>,
    status: String,
    ai_disposition: String,
    resolution: String,
    closure_reason: Option<String>,
    ai_confirmed_at: Option<OffsetDateTime>,
    queued_at: Option<OffsetDateTime>,
    first_response_at: Option<OffsetDateTime>,
    first_closure_proposed_at: Option<OffsetDateTime>,
    first_closed_at: Option<OffsetDateTime>,
    first_closure_reason: Option<String>,
    latest_proposed_at: Option<OffsetDateTime>,
    closed_at: Option<OffsetDateTime>,
    requester_confirmed_at: Option<OffsetDateTime>,
    cancelled_at: Option<OffsetDateTime>,
    rejected_proposals: i64,
    reopens: i64,
    closure_events: i64,
}

impl ExportRow {
    fn csv_line(&self) -> Result<String, ApiError> {
        Ok(csv::line([
            &self.reference.to_string(),
            &timestamp(Some(self.created_at))?,
            &self.category_name,
            &self.priority_name,
            self.team_name.as_deref().unwrap_or_default(),
            &self.status,
            &self.ai_disposition,
            &self.resolution,
            self.closure_reason.as_deref().unwrap_or_default(),
            &timestamp(self.ai_confirmed_at)?,
            &timestamp(self.queued_at)?,
            &timestamp(self.first_response_at)?,
            &timestamp(self.first_closure_proposed_at)?,
            &timestamp(self.first_closed_at)?,
            self.first_closure_reason.as_deref().unwrap_or_default(),
            &timestamp(self.latest_proposed_at)?,
            &timestamp(self.closed_at)?,
            &timestamp(self.requester_confirmed_at)?,
            &timestamp(self.cancelled_at)?,
            &self.rejected_proposals.to_string(),
            &self.reopens.to_string(),
            &self.closure_events.to_string(),
        ]))
    }
}

fn timestamp(value: Option<OffsetDateTime>) -> Result<String, ApiError> {
    value
        .map(|instant| {
            instant
                .to_offset(time::UtcOffset::UTC)
                .format(&Rfc3339)
                .map_err(|_| ApiError::Internal)
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

/// The audit integration point for the report export.
async fn audit_export(
    db: &mut PgConnection,
    actor: Uuid,
    scope: &Scope,
    rows: usize,
) -> Result<(), ApiError> {
    audit::record(
        db,
        Entry {
            action: "report_exported",
            who: Who::Account(actor),
            subject: None,
            ticket: None,
            metadata: json!({
                "filters": {
                    "created_from": timestamp(Some(scope.from))?,
                    "created_before": timestamp(Some(scope.before))?,
                    "category_id": scope.category_id,
                    "team_id": scope.team_id,
                },
                "rows": rows,
            }),
        },
    )
    .await?;
    Ok(())
}

#[utoipa::path(post, path = "/api/admin/reports/export", tag = "admin", params(ReportQuery),
    responses((status = 200, body = String, content_type = "text/csv"),
        (status = 400, description = "Missing, reversed, or over-366-day range", body = crate::api::ErrorEnvelope),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, body = crate::api::ErrorEnvelope),
        (status = 413, description = "More than 10,000 matching rows", body = crate::api::ErrorEnvelope)))]
pub async fn export_report(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ReportQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_permission(&user, Permission::ViewReports)?;
    let scope = Scope::try_from(query)?;

    let mut tx = state.pool.begin().await?;
    let cohort = cohort("ORDER BY x.created_at, x.id LIMIT $5");
    let sql = format!(
        "{} SELECT * FROM facts ORDER BY created_at, id",
        facts(&cohort)
    );
    let rows: Vec<ExportRow> = scope
        .bind(sqlx::query_as(&sql))
        .bind(MAX_EXPORT_ROWS + 1)
        .fetch_all(&mut *tx)
        .await?;
    if rows.len() > MAX_EXPORT_ROWS as usize {
        return Err(ApiError::PayloadTooLarge);
    }

    let mut out = csv::line([
        "reference",
        "submitted_at",
        "category",
        "priority",
        "team",
        "status",
        "ai_outcome",
        "resolution",
        "closure_reason",
        "ai_confirmed_at",
        "queued_at",
        "first_response_at",
        "first_closure_proposed_at",
        "first_closed_at",
        "first_closure_reason",
        "latest_proposed_at",
        "closed_at",
        "requester_confirmed_at",
        "cancelled_at",
        "rejected_proposals",
        "reopens",
        "closure_events",
    ]);
    for row in &rows {
        out.push_str(&row.csv_line()?);
    }
    audit_export(&mut tx, user.account_id(), &scope, rows.len()).await?;
    tx.commit().await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"ticket-report.csv\"",
            ),
        ],
        out,
    ))
}
