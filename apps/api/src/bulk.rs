//! Bounded bulk actions over queue tickets. See docs/queues.md "Bulk actions".
//!
//! There is no bulk write path: each item runs the single-ticket code
//! ([`assignment::apply`] or [`tickets::apply_staff_action`]) in its own
//! transaction, with its own locks, staleness check, and ticket policy. Items
//! succeed or fail independently and each reports a stable error code; a failed
//! item writes nothing.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, ErrorCode, FieldErrors, Json, Role},
    assignment::{self, AssignmentAction},
    auth::CurrentUser,
    authz::{self, Actor, Permission},
    tickets::{self, NamedId, StaffAction},
};

/// Most tickets one bulk request may change.
pub const BULK_MAX: usize = 50;

/// The change applied to every listed ticket; the same rules as the single-ticket
/// `assignment` and `staff-actions` endpoints.
#[derive(Clone, Copy, Debug, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BulkChange {
    Dispatch { team_id: Uuid },
    ReturnToGeneral,
    Claim,
    Assign { assignee_account_id: Uuid },
    Unassign,
    StartWork,
    RequestInformation,
    SetPriority { priority_id: Uuid },
}

/// A ticket and the `updated_at` the client listed it with (`QueueTicket.updated_at`).
#[derive(Clone, Copy, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkTicket {
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expected_updated_at: OffsetDateTime,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BulkRequest {
    pub change: BulkChange,
    /// 1-50 distinct tickets.
    pub tickets: Vec<BulkTicket>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BulkItemResult {
    pub id: Uuid,
    /// `null` when applied. Otherwise what the single-ticket endpoint would answer:
    /// `not_found` (not visible), `forbidden` (visible, not allowed), `conflict`
    /// (changed since listed, or nothing to change), `validation_failed` (not an
    /// eligible target for this ticket), or `internal`.
    #[schema(required = true)]
    pub error: Option<ErrorCode>,
}

/// One result per requested ticket, in request order.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkResponse {
    pub results: Vec<BulkItemResult>,
}

/// Applies one change to up to 50 tickets, each independently authorized, locked,
/// and checked for staleness. A retry never applies twice: an applied item moved
/// its `updated_at`, so repeating it reports `conflict` and writes nothing.
#[utoipa::path(post, path = "/api/tickets/bulk", tag = "tickets", request_body = BulkRequest,
    responses((status = 200, body = BulkResponse),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not staff)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`tickets` is empty (`required`), over 50 (`too_many`), or repeats a ticket (`duplicate`)", body = crate::api::ErrorEnvelope)))]
pub async fn bulk_change(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<BulkRequest>,
) -> Result<Json<BulkResponse>, ApiError> {
    authz::require_permission(&user, Permission::ViewGeneralQueue)?;
    let mut seen = HashSet::new();
    let problem = if body.tickets.is_empty() {
        Some("required")
    } else if body.tickets.len() > BULK_MAX {
        Some("too_many")
    } else if !body.tickets.iter().all(|t| seen.insert(t.id)) {
        Some("duplicate")
    } else {
        None
    };
    if let Some(code) = problem {
        return Err(ApiError::Validation(FieldErrors::from([(
            "tickets".into(),
            vec![code],
        )])));
    }
    let mut results = Vec::with_capacity(body.tickets.len());
    for t in body.tickets {
        let result = apply(&state, &user, body.change, t).await;
        results.push(BulkItemResult {
            id: t.id,
            error: result.err().map(|e| e.code()),
        });
    }
    Ok(Json(BulkResponse { results }))
}

async fn apply(
    state: &AppState,
    user: &CurrentUser,
    change: BulkChange,
    t: BulkTicket,
) -> Result<(), ApiError> {
    use AssignmentAction as A;
    use BulkChange as B;
    use StaffAction as S;
    let (pool, me, id, expected_updated_at) =
        (&state.pool, user.account_id(), t.id, t.expected_updated_at);
    let assign = |action| assignment::apply(pool, me, id, action);
    let staff = |action| tickets::apply_staff_action(pool, me, id, action);
    match change {
        B::Dispatch { team_id } => assign(A::Dispatch {
            expected_updated_at,
            team_id,
        })
        .await
        .map(drop),
        B::ReturnToGeneral => assign(A::ReturnToGeneral {
            expected_updated_at,
        })
        .await
        .map(drop),
        B::Claim => assign(A::Claim {
            expected_updated_at,
        })
        .await
        .map(drop),
        B::Assign {
            assignee_account_id,
        } => assign(A::Assign {
            expected_updated_at,
            assignee_account_id,
        })
        .await
        .map(drop),
        B::Unassign => assign(A::Unassign {
            expected_updated_at,
        })
        .await
        .map(drop),
        B::StartWork => staff(S::StartWork {
            expected_updated_at,
        })
        .await
        .map(drop),
        B::RequestInformation => staff(S::RequestInformation {
            expected_updated_at,
        })
        .await
        .map(drop),
        B::SetPriority { priority_id } => staff(S::SetPriority {
            expected_updated_at,
            priority_id,
        })
        .await
        .map(drop),
    }
}

/// An eligible assignee for bulk assignment.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct BulkAssignee {
    pub id: Uuid,
    pub display_name: String,
    pub team_id: Uuid,
    pub team_name: String,
}

/// Targets the bulk bar offers. Affordances only: each item is rechecked.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkOptions {
    /// Active teams (dispatch targets).
    pub teams: Vec<NamedId>,
    /// Active staff members a caller may assign: a Team Lead's own team, every
    /// active team for an Admin, nobody otherwise.
    pub assignees: Vec<BulkAssignee>,
}

#[utoipa::path(get, path = "/api/queue/bulk-options", tag = "queue",
    responses((status = 200, body = BulkOptions),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not staff)", body = crate::api::ErrorEnvelope)))]
pub async fn bulk_options(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<BulkOptions>, ApiError> {
    authz::require_permission(&user, Permission::ViewGeneralQueue)?;
    let actor = Actor::load(&state.pool, &user).await?;
    let teams =
        sqlx::query_as("SELECT id, name FROM teams WHERE archived_at IS NULL ORDER BY name, id")
            .fetch_all(&state.pool)
            .await?;
    // `NULL` team filter = every team (Admin); no rows for non-leads.
    let scope: Option<Option<Uuid>> = match (actor.role, actor.team) {
        (Role::Admin, _) => Some(None),
        (_, Some(team)) if team.is_lead => Some(Some(team.team_id)),
        _ => None,
    };
    let assignees = match scope {
        Some(team) => {
            sqlx::query_as(
                "SELECT a.id, a.display_name, t.id AS team_id, t.name AS team_name
                 FROM team_memberships m
                 JOIN accounts a ON a.id = m.account_id
                 JOIN teams t ON t.id = m.team_id
                 WHERE ($1::uuid IS NULL OR t.id = $1) AND t.archived_at IS NULL
                   AND a.is_active AND a.role NOT IN ('user', 'admin')
                 ORDER BY t.name, a.display_name, a.id",
            )
            .bind(team)
            .fetch_all(&state.pool)
            .await?
        }
        None => vec![],
    };
    Ok(Json(BulkOptions { teams, assignees }))
}
