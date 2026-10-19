//! Operator work queues: paginated lists and counts of the tickets staff may
//! work on. Visibility is the one SQL predicate [`authz::staff_visible_sql`]
//! (the twin of [`authz::ticket_view`]), applied in the query itself so lists,
//! counts, and direct detail access always agree. See docs/queues.md.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    api::{
        ApiError, Cursor, Json, Page, Pagination, Query, Role, TicketAssignmentView, TicketStatus,
    },
    auth::{CurrentUser, ident, ident_str},
    authz::{self, Actor, Permission},
    tickets::TicketRequester,
};

/// Statuses of tickets that still need human work: the default of every queue.
const OPEN: [&str; 4] = [
    "queued",
    "in_progress",
    "waiting_for_requester",
    "awaiting_confirmation",
];
/// Statuses a queue can be filtered to: operator work plus finished tickets.
/// Drafts, AI stages, and AI-solved records never belong to a staff queue.
const FILTERABLE: [&str; 6] = [
    "queued",
    "in_progress",
    "waiting_for_requester",
    "awaiting_confirmation",
    "closed",
    "cancelled",
];

/// Which slice of the visible tickets a queue shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueView {
    /// Shared intake: tickets with no team yet. Every staff role.
    #[default]
    General,
    /// Dispatched tickets of the caller's team, claimed or not. An Admin sees
    /// every team, or one with `team_id`.
    Team,
    /// Tickets assigned to the caller.
    Mine,
    /// General intake plus every team. Admin only.
    All,
}

/// Fixed orders; ties break on `(created_at, id)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueSort {
    /// Newest first.
    #[default]
    Newest,
    /// Oldest first (longest waiting).
    Oldest,
    /// Admin-defined priority order (catalog order), then oldest first.
    Priority,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueueQuery {
    /// Default `general`.
    pub view: Option<QueueView>,
    /// `team` view, Admin only: restrict to one team.
    pub team_id: Option<Uuid>,
    /// One of the open statuses, `closed`, or `cancelled`; default: all open statuses.
    pub status: Option<TicketStatus>,
    pub category_id: Option<Uuid>,
    pub priority_id: Option<Uuid>,
    /// Only tickets without an individual assignee.
    pub unassigned: Option<bool>,
    pub sort: Option<QueueSort>,
    /// Page size, 1-100 (default 25).
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

/// One queue row.
#[derive(Debug, Serialize, ToSchema)]
pub struct QueueTicket {
    pub id: Uuid,
    /// Stable human-facing number.
    pub reference: i64,
    pub status: TicketStatus,
    pub subject: String,
    pub category_name: String,
    /// The current effective priority; the requester's original choice stays in
    /// the ticket's intake snapshot.
    pub priority_id: Uuid,
    pub priority_name: String,
    pub requester: TicketRequester,
    pub assignment: TicketAssignmentView,
    /// Age is derived from this.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    reference: i64,
    status: String,
    subject: String,
    category_name: String,
    priority_id: Uuid,
    priority_name: String,
    requester_id: Uuid,
    requester_username: String,
    requester_display_name: String,
    team_name: Option<String>,
    assignee_display_name: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

/// Open work counts per view, under the same visibility as the lists.
#[derive(Debug, Serialize, ToSchema)]
pub struct QueueCounts {
    pub general: i64,
    /// The caller's team; for an Admin, every dispatched ticket.
    pub team: i64,
    pub mine: i64,
    /// Admin only.
    #[schema(required = true)]
    pub all: Option<i64>,
}

/// Staff queue page, filtered and ordered as requested. `view=team` for a
/// non-Admin is always their own team (another `team_id` is `404`).
#[utoipa::path(get, path = "/api/queue", tag = "queue", params(QueueQuery),
    responses(
        (status = 200, body = Page<QueueTicket>),
        (status = 400, description = "`invalid_query` (unknown or non-queue filter value)", body = crate::api::ErrorEnvelope),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not staff, or `view=all` without Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (another team's `team_id`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn list_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<QueueQuery>,
) -> Result<Json<Page<QueueTicket>>, ApiError> {
    authz::require_permission(&user, Permission::ViewGeneralQueue)?;
    let page = Pagination {
        limit: q.limit,
        cursor: q.cursor.clone(),
    };
    let cursor = page.cursor()?;
    let statuses: Vec<String> = match q.status {
        None => OPEN.map(String::from).to_vec(),
        Some(s) => {
            let s = ident_str(s);
            if !FILTERABLE.contains(&s.as_str()) {
                return Err(ApiError::InvalidQuery);
            }
            vec![s]
        }
    };
    let actor = Actor::load(&state.pool, &user).await?;
    let view = q.view.unwrap_or_default();
    let Some((view_sql, team)) = scope(&actor, view, q.team_id)? else {
        return Ok(Json(Page {
            items: vec![],
            next_cursor: None,
        }));
    };
    let (order, after) = match q.sort.unwrap_or_default() {
        QueueSort::Newest => (
            "t.created_at DESC, t.id DESC",
            "(t.created_at, t.id) < ($9, $10)",
        ),
        QueueSort::Oldest => ("t.created_at, t.id", "(t.created_at, t.id) > ($9, $10)"),
        QueueSort::Priority => (
            "p.sort_order, t.created_at, t.id",
            "(p.sort_order, t.created_at, t.id) > (SELECT p2.sort_order, t2.created_at, t2.id
                 FROM tickets t2 JOIN priorities p2 ON p2.id = t2.priority_id WHERE t2.id = $10)",
        ),
    };
    let sql = format!(
        "SELECT t.id, t.reference, t.status::text AS status, t.subject,
                COALESCE(t.intake_snapshot->'catalog'->>'category_name', c.name) AS category_name,
                t.priority_id, p.name AS priority_name,
                r.id AS requester_id, r.username AS requester_username,
                r.display_name AS requester_display_name,
                tm.name AS team_name, a.display_name AS assignee_display_name,
                t.created_at, t.updated_at
         FROM tickets t
         JOIN categories c ON c.id = t.category_id
         JOIN priorities p ON p.id = t.priority_id
         JOIN accounts r ON r.id = t.requester_account_id
         LEFT JOIN teams tm ON tm.id = t.team_id
         LEFT JOIN accounts a ON a.id = t.assignee_account_id
         WHERE {visible} AND {view_sql}
           AND t.status = ANY($4::ticket_status[])
           AND ($5::uuid IS NULL OR t.category_id = $5)
           AND ($6::uuid IS NULL OR t.priority_id = $6)
           AND ($7::bool IS NOT TRUE OR t.assignee_account_id IS NULL)
           AND ($10::uuid IS NULL OR {after})
         ORDER BY {order} LIMIT $11",
        visible = authz::staff_visible_sql(1),
    );
    let (id, role, own_team) = actor.visibility_binds();
    let rows: Vec<Row> = sqlx::query_as(&sql)
        .bind(id)
        .bind(role)
        .bind(own_team)
        .bind(statuses)
        .bind(q.category_id)
        .bind(q.priority_id)
        .bind(q.unassigned)
        .bind(team)
        .bind(cursor.map(|c| c.created_at))
        .bind(cursor.map(|c| c.id))
        .bind(page.limit() + 1)
        .fetch_all(&state.pool)
        .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(QueueTicket {
                id: row.id,
                reference: row.reference,
                status: ident(row.status)?,
                subject: row.subject,
                category_name: row.category_name,
                priority_id: row.priority_id,
                priority_name: row.priority_name,
                requester: TicketRequester {
                    id: row.requester_id,
                    username: row.requester_username,
                    display_name: row.requester_display_name,
                },
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

/// Intake and the Admin overview leave out cancelled tickets that never reached a
/// team: they may have ended during AI, which is Admin reporting, not operator work.
///
/// The SQL narrowing a view (binds `$8` as the team filter) and the team
/// it filters to; `None` when the caller can see nothing there (no team).
fn scope(
    actor: &Actor,
    view: QueueView,
    team_id: Option<Uuid>,
) -> Result<Option<(&'static str, Option<Uuid>)>, ApiError> {
    let admin = actor.role == Role::Admin;
    Ok(match view {
        QueueView::General if team_id.is_none() => {
            Some(("t.team_id IS NULL AND t.status <> 'cancelled'", None))
        }
        QueueView::Mine if team_id.is_none() => Some(("t.assignee_account_id = $1", None)),
        QueueView::All if admin && team_id.is_none() => {
            Some(("(t.team_id IS NOT NULL OR t.status <> 'cancelled')", None))
        }
        QueueView::All if !admin => return Err(ApiError::Forbidden),
        QueueView::Team => match (admin, team_id, actor.team) {
            (true, None, _) => Some(("t.team_id IS NOT NULL", None)),
            (true, Some(team), _) => Some(("t.team_id = $8", Some(team))),
            (false, Some(team), own) if own.map(|t| t.team_id) != Some(team) => {
                return Err(ApiError::NotFound);
            }
            (false, _, Some(own)) => Some(("t.team_id = $8", Some(own.team_id))),
            (false, _, None) => None,
        },
        _ => return Err(ApiError::InvalidQuery), // `team_id` only narrows the team view
    })
}

/// Open work counts for the caller's queue tabs.
#[utoipa::path(get, path = "/api/queue/counts", tag = "queue",
    responses(
        (status = 200, body = QueueCounts),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not staff)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn queue_counts(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<QueueCounts>, ApiError> {
    authz::require_permission(&user, Permission::ViewGeneralQueue)?;
    let actor = Actor::load(&state.pool, &user).await?;
    let admin = actor.role == Role::Admin;
    let (id, role, team) = actor.visibility_binds();
    let (general, team_count, mine, all): (i64, i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE t.team_id IS NULL),
                count(*) FILTER (WHERE t.team_id IS NOT NULL),
                count(*) FILTER (WHERE t.assignee_account_id = $1),
                count(*)
         FROM tickets t
         WHERE {visible} AND t.status = ANY($4::ticket_status[])",
        visible = authz::staff_visible_sql(1),
    ))
    .bind(id)
    .bind(role)
    .bind(team)
    .bind(OPEN.map(String::from).to_vec())
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(QueueCounts {
        general,
        team: team_count,
        mine,
        all: admin.then_some(all),
    }))
}
