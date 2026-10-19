//! Ticket read API. Status changes go through [`crate::lifecycle`]; visibility
//! is [`authz::ticket_view`]. See docs/lifecycle.md.

use axum::extract::State;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{
        ApiError, Cursor, Json, Page, Pagination, Path, Query, TicketAssignmentView, TicketStatus,
    },
    auth::{CurrentUser, ident},
    authz::{self, Actor, TicketScope, TicketView},
    lifecycle::{ClosureReason, ResolutionSource},
    submission::SubmittedIntake,
};

/// One ticket as the caller may see it.
#[derive(Debug, Serialize, ToSchema)]
pub struct TicketDetail {
    pub id: Uuid,
    /// Stable human-facing number.
    pub reference: i64,
    pub status: TicketStatus,
    /// `requester` when the caller submitted it (public data only, whatever their
    /// role); `staff` for authorized staff.
    pub view: TicketView,
    pub subject: String,
    pub description: String,
    pub category_id: Uuid,
    pub category_name: String,
    pub priority_id: Uuid,
    pub priority_name: String,
    pub assignment: TicketAssignmentView,
    /// Frozen original intake; null only for legacy records predating submission.
    #[schema(required = true)]
    pub intake_snapshot: Option<SubmittedIntake>,
    /// Set on `closed` (`operator`) and `ai_solved` (`ai`) tickets.
    #[schema(required = true)]
    pub resolution_source: Option<ResolutionSource>,
    /// Set on `closed` and `ai_solved` tickets.
    #[schema(required = true)]
    pub closure_reason: Option<ClosureReason>,
    /// Version of the latest staff resolution proposal (0 = none). Requester
    /// decisions send it back.
    pub proposal_version: i32,
    /// Auto-close time of the pending proposal; set only while `awaiting_confirmation`.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub confirmation_deadline: Option<OffsetDateTime>,
    /// Staff view only; `null` in the requester view.
    #[schema(required = true)]
    pub requester: Option<TicketRequester>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
    /// Status transitions, oldest first.
    pub transitions: Vec<TicketTransition>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TicketRequester {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// One immutable status transition (`ticket_events` row with a `to_status`).
#[derive(Debug, Serialize, ToSchema)]
pub struct TicketTransition {
    pub id: i64,
    /// The transition, e.g. `submit`, `propose_resolution`, `confirmation_timeout`.
    pub event_type: String,
    pub from_status: TicketStatus,
    pub to_status: TicketStatus,
    /// Staff view only; `null` for background jobs and in the requester view.
    #[schema(required = true)]
    pub actor_account_id: Option<Uuid>,
    /// Public details: `route` (queue entry reason), `resolution_source`,
    /// `closure_reason`, `proposal_version`, `summary`, `timeout_hours`,
    /// `deadline`, or the cancellation `reason`.
    #[schema(value_type = Object)]
    pub payload: Value,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct DetailRow {
    id: Uuid,
    reference: i64,
    status: String,
    subject: String,
    description: String,
    category_id: Uuid,
    category_name: String,
    intake_snapshot: Option<sqlx::types::Json<SubmittedIntake>>,
    priority_id: Uuid,
    priority_name: String,
    requester_account_id: Uuid,
    requester_username: String,
    requester_display_name: String,
    team_id: Option<Uuid>,
    team_name: Option<String>,
    assignee_account_id: Option<Uuid>,
    assignee_display_name: Option<String>,
    resolution_source: Option<String>,
    closure_reason: Option<String>,
    proposal_version: i32,
    confirmation_deadline: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

type TransitionRow = (
    i64,
    String,
    String,
    String,
    Option<Uuid>,
    Value,
    OffsetDateTime,
);

/// A ticket the caller may see: their own (requester view, any status, any
/// role), or as staff under the ticket policy. Drafts, AI-stage and AI-solved
/// records are hidden from operators; dispatched tickets from other teams.
#[utoipa::path(get, path = "/api/tickets/{id}", tag = "tickets",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = TicketDetail),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (missing or not visible to the caller)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn ticket_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<TicketDetail>, ApiError> {
    // One snapshot, so the status and its transitions agree.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let actor = Actor::load(&mut *tx, &user).await?;
    let row: DetailRow = sqlx::query_as(
        "SELECT t.id, t.reference, t.status::text AS status, t.subject, t.description,
                t.category_id, COALESCE(t.intake_snapshot->'catalog'->>'category_name', c.name) AS category_name,
                t.intake_snapshot, t.priority_id, p.name AS priority_name,
                t.requester_account_id, r.username AS requester_username,
                r.display_name AS requester_display_name, t.team_id, tm.name AS team_name,
                t.assignee_account_id, a.display_name AS assignee_display_name,
                t.resolution_source, t.closure_reason, t.proposal_version,
                t.confirmation_deadline, t.created_at, t.updated_at
         FROM tickets t
         JOIN categories c ON c.id = t.category_id
         JOIN priorities p ON p.id = t.priority_id
         JOIN accounts r ON r.id = t.requester_account_id
         LEFT JOIN teams tm ON tm.id = t.team_id
         LEFT JOIN accounts a ON a.id = t.assignee_account_id
         WHERE t.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    let status: TicketStatus = ident(row.status)?;
    let scope = TicketScope {
        requester_account_id: row.requester_account_id,
        team_id: row.team_id,
        assignee_account_id: row.assignee_account_id,
        status,
    };
    let view = authz::ticket_view(&actor, &scope).ok_or(ApiError::NotFound)?;
    let staff = view == TicketView::Staff;
    // ponytail: every transition in one response; page it if tickets ever collect hundreds.
    let rows: Vec<TransitionRow> = sqlx::query_as(
        "SELECT id, event_type, from_status::text, to_status::text, actor_account_id, payload, created_at
         FROM ticket_events WHERE ticket_id = $1 AND to_status IS NOT NULL ORDER BY id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let transitions = rows
        .into_iter()
        .map(|(id, event_type, from, to, actor, payload, created_at)| {
            Ok(TicketTransition {
                id,
                event_type,
                from_status: ident(from)?,
                to_status: ident(to)?,
                actor_account_id: actor.filter(|_| staff),
                payload,
                created_at,
            })
        })
        .collect::<Result<_, ApiError>>()?;
    Ok(Json(TicketDetail {
        id: row.id,
        reference: row.reference,
        status,
        view,
        subject: row.subject,
        description: row.description,
        category_id: row.category_id,
        category_name: row.category_name,
        intake_snapshot: row.intake_snapshot.map(|s| s.0),
        priority_id: row.priority_id,
        priority_name: row.priority_name,
        assignment: TicketAssignmentView {
            team_name: row.team_name,
            assignee_display_name: row.assignee_display_name,
        },
        resolution_source: row.resolution_source.map(ident).transpose()?,
        closure_reason: row.closure_reason.map(ident).transpose()?,
        proposal_version: row.proposal_version,
        confirmation_deadline: row.confirmation_deadline,
        requester: staff.then_some(TicketRequester {
            id: row.requester_account_id,
            username: row.requester_username,
            display_name: row.requester_display_name,
        }),
        created_at: row.created_at,
        updated_at: row.updated_at,
        transitions,
    }))
}

/// Minimal requester history. No staff-only metadata; all roles see only their own.
#[derive(Debug, Serialize, ToSchema)]
pub struct TicketSummary {
    pub id: Uuid,
    pub reference: i64,
    pub status: TicketStatus,
    pub subject: String,
    pub category_name: String,
    pub priority_name: String,
    pub assignment: TicketAssignmentView,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    reference: i64,
    status: String,
    subject: String,
    category_name: String,
    priority_name: String,
    team_name: Option<String>,
    assignee_display_name: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

/// Own submitted tickets, newest first, including final AI-solved history.
/// This is a requester list for every role, never a staff queue or Admin report.
#[utoipa::path(get, path = "/api/tickets", tag = "tickets", params(Pagination),
    responses((status = 200, body = Page<TicketSummary>),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope)))]
pub async fn list_own_tickets(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(page): Query<Pagination>,
) -> Result<Json<Page<TicketSummary>>, ApiError> {
    let cursor = page.cursor()?;
    let rows: Vec<SummaryRow> = sqlx::query_as(
        "SELECT t.id, t.reference, t.status::text AS status, t.subject,
                COALESCE(t.intake_snapshot->'catalog'->>'category_name', c.name) AS category_name,
                p.name AS priority_name, tm.name AS team_name, a.display_name AS assignee_display_name,
                t.created_at, t.updated_at
         FROM tickets t JOIN categories c ON c.id = t.category_id
         JOIN priorities p ON p.id = t.priority_id
         LEFT JOIN teams tm ON tm.id = t.team_id
         LEFT JOIN accounts a ON a.id = t.assignee_account_id
         WHERE t.requester_account_id = $1 AND t.status <> 'draft'
           AND ($2::timestamptz IS NULL OR (t.created_at, t.id) < ($2, $3))
         ORDER BY t.created_at DESC, t.id DESC LIMIT $4")
        .bind(user.account_id()).bind(cursor.map(|c| c.created_at))
        .bind(cursor.map(|c| c.id)).bind(page.limit() + 1).fetch_all(&state.pool).await?;
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(TicketSummary {
                id: row.id,
                reference: row.reference,
                status: ident(row.status)?,
                subject: row.subject,
                category_name: row.category_name,
                priority_name: row.priority_name,
                assignment: TicketAssignmentView {
                    team_name: row.team_name,
                    assignee_display_name: row.assignee_display_name,
                },
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(Page::from_rows(items, page.limit(), |t| Cursor {
        created_at: t.created_at,
        id: t.id,
    })))
}
