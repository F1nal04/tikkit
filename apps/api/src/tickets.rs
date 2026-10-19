//! Ticket read API. Status changes go through [`crate::lifecycle`]; visibility
//! is [`authz::ticket_view`]. See docs/lifecycle.md.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    api::{
        ApiError, Cursor, FieldErrors, Json, Page, Pagination, Path, Query, TicketAssignmentView,
        TicketStatus,
    },
    auth::{CurrentUser, ident},
    authz::{self, Actor, Permission, TicketScope, TicketView},
    lifecycle::{self, By, ClosureReason, ResolutionSource, Transition},
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
    /// Effective-priority changes, oldest first. Actors are staff-view only.
    pub priority_changes: Vec<PriorityChange>,
    /// What the caller may do as staff; `null` in the requester view.
    #[schema(required = true)]
    pub staff_access: Option<StaffAccess>,
}

/// Affordances for the staff view, from the same rules the write endpoints
/// enforce. Clients hide controls with them; the server rechecks every write.
#[derive(Debug, Serialize, ToSchema)]
pub struct StaffAccess {
    /// Staff reply, internal note, status work, resolution proposal, and priority
    /// changes: the ticket is open, in a team, and the caller is in charge of it.
    pub can_handle: bool,
    /// The ticket is closed with operator resolution, the Admin toggle is on, and
    /// the caller is in charge of it.
    pub can_reopen: bool,
}

/// One immutable change of the effective priority (`priority_changed` event).
#[derive(Debug, Serialize, ToSchema)]
pub struct PriorityChange {
    pub id: i64,
    pub previous_priority_name: String,
    pub priority_name: String,
    /// Staff view only; `null` in the requester view.
    #[schema(required = true)]
    pub actor_account_id: Option<Uuid>,
    #[schema(required = true)]
    pub actor_display_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
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
    /// Current display name of the actor; staff view only.
    #[schema(required = true)]
    pub actor_display_name: Option<String>,
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
    Option<String>,
    Value,
    OffsetDateTime,
);

type ChangeRow = (i64, Option<Uuid>, Option<String>, Value, OffsetDateTime);

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
        "SELECT e.id, e.event_type, e.from_status::text, e.to_status::text, e.actor_account_id,
                a.display_name, e.payload, e.created_at
         FROM ticket_events e LEFT JOIN accounts a ON a.id = e.actor_account_id
         WHERE e.ticket_id = $1 AND e.to_status IS NOT NULL ORDER BY e.id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let transitions = rows
        .into_iter()
        .map(
            |(id, event_type, from, to, actor, actor_name, payload, created_at)| {
                Ok(TicketTransition {
                    id,
                    event_type,
                    from_status: ident(from)?,
                    to_status: ident(to)?,
                    actor_account_id: actor.filter(|_| staff),
                    actor_display_name: actor_name.filter(|_| staff),
                    payload,
                    created_at,
                })
            },
        )
        .collect::<Result<_, ApiError>>()?;
    let changes: Vec<ChangeRow> = sqlx::query_as(
        "SELECT e.id, e.actor_account_id, a.display_name, e.payload, e.created_at
         FROM ticket_events e LEFT JOIN accounts a ON a.id = e.actor_account_id
         WHERE e.ticket_id = $1 AND e.event_type = 'priority_changed' ORDER BY e.id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let name = |payload: &Value, key: &str| payload[key].as_str().unwrap_or_default().to_owned();
    let priority_changes = changes
        .into_iter()
        .map(
            |(id, actor, actor_name, payload, created_at)| PriorityChange {
                id,
                previous_priority_name: name(&payload, "previous_priority_name"),
                priority_name: name(&payload, "priority_name"),
                actor_account_id: actor.filter(|_| staff),
                actor_display_name: actor_name.filter(|_| staff),
                created_at,
            },
        )
        .collect();
    let staff_access = if staff {
        let allow_reopen: bool =
            sqlx::query_scalar("SELECT allow_reopening FROM organization WHERE id")
                .fetch_one(&mut *tx)
                .await?;
        Some(StaffAccess {
            can_handle: authz::authorize_ticket(&actor, &scope, Permission::ReplyAsStaff).is_ok(),
            can_reopen: allow_reopen
                && status == TicketStatus::Closed
                && authz::is_staff_writer(&actor, &scope),
        })
    } else {
        None
    };
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
        priority_changes,
        staff_access,
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
#[utoipa::path(get, path = "/api/tickets", tag = "tickets", params(OwnTicketsQuery),
    responses((status = 200, body = Page<TicketSummary>),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope)))]
pub async fn list_own_tickets(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(filters): Query<OwnTicketsQuery>,
) -> Result<Json<Page<TicketSummary>>, ApiError> {
    let page = Pagination {
        limit: filters.limit,
        cursor: filters.cursor,
    };
    let cursor = page.cursor()?;
    let parse = |value: Option<String>| {
        value
            .map(|s| {
                OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
                    .map_err(|_| ApiError::InvalidQuery)
            })
            .transpose()
    };
    let from = parse(filters.created_from)?;
    let before = parse(filters.created_before)?;
    if from.zip(before).is_some_and(|(a, b)| a >= b) {
        return Err(ApiError::InvalidQuery);
    }
    let status = filters.status.map(crate::auth::ident_str);
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
           AND ($5::text IS NULL OR t.status::text = $5)
           AND ($6::timestamptz IS NULL OR t.created_at >= $6)
           AND ($7::timestamptz IS NULL OR t.created_at < $7)
         ORDER BY t.created_at DESC, t.id DESC LIMIT $4")
        .bind(user.account_id()).bind(cursor.map(|c| c.created_at))
        .bind(cursor.map(|c| c.id)).bind(page.limit() + 1)
        .bind(status).bind(from).bind(before).fetch_all(&state.pool).await?;
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

/// Own history filters, applied before the page limit. Dates are UTC RFC 3339 instants.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OwnTicketsQuery {
    pub status: Option<TicketStatus>,
    /// Inclusive creation timestamp.
    pub created_from: Option<String>,
    /// Exclusive creation timestamp.
    pub created_before: Option<String>,
    /// Page size, 1–100 (default 25).
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

/// Explicit decisions only: replies and views never implicitly accept a proposal.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequesterAction {
    AcceptResolution { proposal_version: i32 },
    RejectResolution { proposal_version: i32 },
    Cancel { reason: String },
    Reopen,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ActionResult {
    pub status: TicketStatus,
}

/// Apply one lifecycle decision atomically. Stale/duplicate proposal decisions
/// return 409; policy is read again by the lifecycle inside the transaction.
#[utoipa::path(post, path = "/api/tickets/{id}/actions", tag = "tickets",
    params(("id" = Uuid, Path)), request_body = RequesterAction,
    responses((status = 200, body = ActionResult),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "Current policy or actor denies the action", body = crate::api::ErrorEnvelope),
        (status = 404, body = crate::api::ErrorEnvelope),
        (status = 409, description = "Stale state or proposal version", body = crate::api::ErrorEnvelope),
        (status = 422, body = crate::api::ErrorEnvelope)))]
pub async fn requester_action(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(action): Json<RequesterAction>,
) -> Result<Json<ActionResult>, ApiError> {
    let transition = match action {
        RequesterAction::AcceptResolution { proposal_version } => {
            Transition::AcceptResolution { proposal_version }
        }
        RequesterAction::RejectResolution { proposal_version } => {
            Transition::RejectResolution { proposal_version }
        }
        RequesterAction::Cancel { reason } => Transition::Cancel { reason },
        RequesterAction::Reopen => Transition::Reopen,
    };
    let mut tx = state.pool.begin().await?;
    let status =
        lifecycle::transition(&mut tx, id, By::Account(user.account_id()), &transition).await?;
    tx.commit().await?;
    Ok(Json(ActionResult { status }))
}

/// Staff work on a ticket. Every action carries the `updated_at` the client last
/// loaded; a newer change (reply, status, priority) is `409 conflict`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum StaffAction {
    StartWork {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
    },
    RequestInformation {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
    },
    /// `summary` is requester-visible, 1–5000 characters. Supersedes a pending proposal.
    ProposeResolution {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
        summary: String,
    },
    /// Only while the Admin reopening toggle is on.
    Reopen {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
    },
    SetPriority {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
        priority_id: Uuid,
    },
}

/// Staff status work and priority changes. Thin over [`lifecycle::transition`]
/// (visibility, self-handling, claimed-ticket and policy rules live there and in
/// [`authz::authorize_ticket`]); stale clients get `409` and nothing is written.
#[utoipa::path(post, path = "/api/tickets/{id}/staff-actions", tag = "tickets",
    params(("id" = Uuid, Path)), request_body = StaffAction,
    responses((status = 200, body = ActionResult),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "Not in charge of the ticket, self-handling, or policy denies", body = crate::api::ErrorEnvelope),
        (status = 404, body = crate::api::ErrorEnvelope),
        (status = 409, description = "Stale `expected_updated_at` or status", body = crate::api::ErrorEnvelope),
        (status = 422, body = crate::api::ErrorEnvelope)))]
pub async fn staff_action(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(action): Json<StaffAction>,
) -> Result<Json<ActionResult>, ApiError> {
    let mut tx = state.pool.begin().await?;
    // Same lock order as the lifecycle: actor, then ticket.
    let actor = lifecycle::lock_actor(&mut tx, user.account_id()).await?;
    let (requester, team_id, assignee, status, updated_at, priority_id): (
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        String,
        OffsetDateTime,
        Uuid,
    ) = sqlx::query_as(
        "SELECT requester_account_id, team_id, assignee_account_id, status::text, updated_at, priority_id
         FROM tickets WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    let scope = TicketScope {
        requester_account_id: requester,
        team_id,
        assignee_account_id: assignee,
        status: ident(status)?,
    };
    authz::ticket_view(&actor, &scope).ok_or(ApiError::NotFound)?;
    let stale = |expected: OffsetDateTime| {
        if expected == updated_at {
            Ok(())
        } else {
            Err(ApiError::Conflict)
        }
    };
    let by = By::Account(actor.id);
    let status = match action {
        StaffAction::SetPriority {
            expected_updated_at,
            priority_id: next,
        } => {
            authz::authorize_ticket(&actor, &scope, Permission::ChangePriority)?;
            stale(expected_updated_at)?;
            let names: Vec<(Uuid, String)> = sqlx::query_as(
                "SELECT id, name FROM priorities
                 WHERE id = ANY($1) AND (archived_at IS NULL OR id = $2)",
            )
            .bind([priority_id, next].as_slice())
            .bind(priority_id)
            .fetch_all(&mut *tx)
            .await?;
            let name_of = |wanted: Uuid| {
                names
                    .iter()
                    .find(|(id, _)| *id == wanted)
                    .map(|n| n.1.clone())
            };
            let (Some(previous_name), Some(next_name)) = (name_of(priority_id), name_of(next))
            else {
                return Err(ApiError::Validation(FieldErrors::from([(
                    "priority_id".into(),
                    vec!["invalid"],
                )])));
            };
            if next == priority_id {
                return Err(ApiError::Validation(FieldErrors::from([(
                    "priority_id".into(),
                    vec!["invalid"],
                )])));
            }
            sqlx::query(
                "UPDATE tickets SET priority_id = $2, updated_at = clock_timestamp() WHERE id = $1",
            )
            .bind(id)
            .bind(next)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, payload)
                 VALUES ($1, $2, 'priority_changed', $3)",
            )
            .bind(id)
            .bind(actor.id)
            .bind(json!({
                "previous_priority_id": priority_id, "previous_priority_name": previous_name,
                "priority_id": next, "priority_name": next_name,
            }))
            .execute(&mut *tx)
            .await?;
            scope.status
        }
        StaffAction::StartWork {
            expected_updated_at,
        } => {
            let to = lifecycle::transition(&mut tx, id, by, &Transition::StartWork).await?;
            stale(expected_updated_at)?;
            to
        }
        StaffAction::RequestInformation {
            expected_updated_at,
        } => {
            let to =
                lifecycle::transition(&mut tx, id, by, &Transition::RequestInformation).await?;
            stale(expected_updated_at)?;
            to
        }
        StaffAction::ProposeResolution {
            expected_updated_at,
            summary,
        } => {
            let to =
                lifecycle::transition(&mut tx, id, by, &Transition::ProposeResolution { summary })
                    .await?;
            stale(expected_updated_at)?;
            to
        }
        StaffAction::Reopen {
            expected_updated_at,
        } => {
            let to = lifecycle::transition(&mut tx, id, by, &Transition::Reopen).await?;
            stale(expected_updated_at)?;
            to
        }
    };
    tx.commit().await?;
    Ok(Json(ActionResult { status }))
}
