//! Manual dispatch and individual assignment. See docs/queues.md "Dispatch and assignment".
//!
//! Team and assignee changes are not status transitions: each one locks the ticket
//! row, checks the ticket policy ([`authz::authorize_ticket`]), updates
//! `team_id`/`assignee_account_id`, and appends one immutable `assignment_changed`
//! event with the actor and the previous and new team and assignee. Nothing routes
//! automatically: every change here is an explicit staff action.

use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, Json, Path},
    auth::{CurrentUser, ident},
    authz::{self, Permission, TicketScope},
    lifecycle,
    management::invalid,
};

/// One explicit team or assignee change, with the `updated_at` the client last
/// loaded (`TicketDetail.updated_at`); a newer change is `409 conflict`.
#[derive(Clone, Copy, Debug, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssignmentAction {
    /// Send the ticket to an active team (from the general queue or another team).
    /// Leaves the individual assignee empty.
    Dispatch {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
        team_id: Uuid,
    },
    /// Put a team ticket back into shared intake: no team, no assignee.
    ReturnToGeneral {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
    },
    /// Take an unclaimed ticket of the caller's team.
    Claim {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
    },
    /// Team Lead (own team) or Admin: (re)assign to an active member of the ticket's team.
    Assign {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
        assignee_account_id: Uuid,
    },
    /// Team Lead (own team) or Admin: clear the individual assignee.
    Unassign {
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String, format = DateTime)]
        expected_updated_at: OffsetDateTime,
    },
}

/// What changed, safe to show even when the caller lost access by the change.
#[derive(Debug, Serialize, ToSchema)]
pub struct AssignmentReceipt {
    pub reference: i64,
    #[schema(required = true)]
    pub team_name: Option<String>,
    #[schema(required = true)]
    pub assignee_display_name: Option<String>,
    /// The caller can still open the ticket as staff. False after a transfer to a
    /// team the caller is not in: further reads are `404`.
    pub visible: bool,
}

/// Dispatch, transfer, return to general intake, claim, assign, or unassign.
/// Checks: visibility (`404`), self-handling (`403`), stale `expected_updated_at`
/// (`409`), the rest of the ticket policy (`403`), then the target (`422`).
/// Concurrent changes serialize on the ticket row; the later one is stale (`409`).
#[utoipa::path(post, path = "/api/tickets/{id}/assignment", tag = "tickets",
    params(("id" = Uuid, Path)), request_body = AssignmentAction,
    responses((status = 200, body = AssignmentReceipt),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "Not allowed on this ticket (role, team, claim, self-handling, status)", body = crate::api::ErrorEnvelope),
        (status = 404, body = crate::api::ErrorEnvelope),
        (status = 409, description = "Stale `expected_updated_at`, or nothing to return/unassign", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`team_id` or `assignee_account_id` is not an eligible target", body = crate::api::ErrorEnvelope)))]
pub async fn change_assignment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(action): Json<AssignmentAction>,
) -> Result<Json<AssignmentReceipt>, ApiError> {
    apply(&state.pool, user.account_id(), id, action)
        .await
        .map(Json)
}

/// One assignment change in its own transaction: the single-ticket endpoint and
/// each item of a bulk action (`bulk.rs`) run exactly this.
pub(crate) async fn apply(
    pool: &PgPool,
    account_id: Uuid,
    id: Uuid,
    action: AssignmentAction,
) -> Result<AssignmentReceipt, ApiError> {
    use AssignmentAction as A;
    let mut tx = pool.begin().await?;
    // Account, membership, and team before the ticket. `lock_actor`, deactivation,
    // and membership edits use that order; locking a target after the ticket
    // deadlocks with a member who is already waiting on this row.
    let actor = lifecycle::lock_actor(&mut tx, account_id).await?;
    match &action {
        A::Dispatch {
            team_id: target, ..
        } => {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM teams WHERE id = $1 FOR SHARE")
                .bind(target)
                .fetch_optional(&mut *tx)
                .await?;
        }
        A::Assign {
            assignee_account_id: target,
            ..
        } => {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM accounts WHERE id = $1 FOR SHARE")
                .bind(target)
                .fetch_optional(&mut *tx)
                .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT m.account_id FROM team_memberships m
                 JOIN teams t ON t.id = m.team_id
                 WHERE m.account_id = $1
                 FOR SHARE OF m, t",
            )
            .bind(target)
            .fetch_optional(&mut *tx)
            .await?;
        }
        _ => {}
    }
    let (reference, requester, team_id, assignee, status, updated_at): (
        i64,
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        String,
        OffsetDateTime,
    ) = sqlx::query_as(
        "SELECT reference, requester_account_id, team_id, assignee_account_id, status::text, updated_at
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
    // Invisible tickets are `404` before any stale or policy result. The requester
    // is `403` before the stale check, so a wrong timestamp never reveals that a
    // ticket they must not handle has moved.
    match authz::ticket_view(&actor, &scope) {
        None => return Err(ApiError::NotFound),
        Some(authz::TicketView::Requester) => return Err(ApiError::Forbidden),
        Some(authz::TicketView::Staff) => {}
    }
    let (A::Dispatch {
        expected_updated_at,
        ..
    }
    | A::ReturnToGeneral {
        expected_updated_at,
    }
    | A::Claim {
        expected_updated_at,
    }
    | A::Assign {
        expected_updated_at,
        ..
    }
    | A::Unassign {
        expected_updated_at,
    }) = action;
    // Before the policy check, so the loser of a concurrent claim or transfer gets a
    // recoverable conflict rather than "already claimed".
    if expected_updated_at != updated_at {
        return Err(ApiError::Conflict);
    }
    let permission = match action {
        A::Dispatch { .. } | A::ReturnToGeneral { .. } => Permission::DispatchTicket,
        A::Claim { .. } => Permission::ClaimTicket,
        A::Assign { .. } | A::Unassign { .. } => Permission::AssignTicket,
    };
    authz::authorize_ticket(&actor, &scope, permission)?;
    let (name, next_team, next_assignee) = match action {
        A::Dispatch {
            team_id: target, ..
        } => {
            if Some(target) == team_id {
                return Err(invalid("team_id"));
            }
            // The team row is already share-locked, so archiving it waits for us.
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM teams WHERE id = $1 AND archived_at IS NULL",
            )
            .bind(target)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| invalid("team_id"))?;
            // Nobody is a member of two teams, so any assignee is ineligible now.
            ("dispatch", Some(target), None)
        }
        A::ReturnToGeneral { .. } => {
            if team_id.is_none() {
                return Err(ApiError::Conflict);
            }
            ("return_to_general", None, None)
        }
        A::Claim { .. } => ("claim", team_id, Some(actor.id)),
        A::Assign {
            assignee_account_id: target,
            ..
        } => {
            // Account, membership, and team are already share-locked, so deactivation
            // or a membership change waits until this assignment commits.
            let eligible: Option<Uuid> = sqlx::query_scalar(
                "SELECT a.id FROM team_memberships m
                 JOIN accounts a ON a.id = m.account_id
                 JOIN teams t ON t.id = m.team_id
                 WHERE m.account_id = $1 AND m.team_id = $2 AND a.is_active
                   AND a.role NOT IN ('user', 'admin') AND t.archived_at IS NULL",
            )
            .bind(target)
            .bind(team_id)
            .fetch_optional(&mut *tx)
            .await?;
            if eligible.is_none() || target == requester || Some(target) == assignee {
                return Err(invalid("assignee_account_id"));
            }
            ("assign", team_id, Some(target))
        }
        A::Unassign { .. } => {
            if assignee.is_none() {
                return Err(ApiError::Conflict);
            }
            ("unassign", team_id, None)
        }
    };
    let (team_name, assignee_name): (Option<String>, Option<String>) = sqlx::query_as(
        "WITH changed AS (
             UPDATE tickets SET team_id = $2, assignee_account_id = $3, updated_at = clock_timestamp()
             WHERE id = $1 RETURNING updated_at
         ), event AS (
             INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, payload, created_at)
             SELECT $1, $4, 'assignment_changed', $5, updated_at FROM changed
         )
         SELECT (SELECT name FROM teams WHERE id = $2), (SELECT display_name FROM accounts WHERE id = $3)",
    )
    .bind(id)
    .bind(next_team)
    .bind(next_assignee)
    .bind(actor.id)
    .bind(json!({
        "action": name,
        "previous_team_id": team_id, "team_id": next_team,
        "previous_assignee_account_id": assignee, "assignee_account_id": next_assignee,
    }))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let after = TicketScope {
        team_id: next_team,
        assignee_account_id: next_assignee,
        ..scope
    };
    Ok(AssignmentReceipt {
        reference,
        team_name,
        assignee_display_name: assignee_name,
        visible: authz::ticket_view(&actor, &after).is_some(),
    })
}
