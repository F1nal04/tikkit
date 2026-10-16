//! Admin directory services (issue #11): accounts/roles and teams/leadership.
//!
//! Database-backed services; [`http`] exposes them under `/api/v1/admin`
//! (Admin only, via the `Actor` extractor).
//! Each mutation:
//!
//! - runs in one transaction and re-checks inside it that `actor_id` is an
//!   active Admin (handlers additionally call [`crate::authz::can_manage_org`]);
//! - takes row locks so concurrent changes serialize (all active Admin rows for
//!   role/status changes, the team row for leadership/membership changes);
//! - validates the target state first and returns stable error codes
//!   (`last_admin`, `team_membership_conflict`, `team_lead_required`,
//!   `team_has_open_tickets`, `validation_failed`, `not_found`, `conflict`);
//!   constraint/trigger violations from the schema are mapped to the same codes
//!   by [`map_db_error`] instead of surfacing as `internal_error`;
//! - writes an `audit_events` row in the same transaction.
//!
//! A failed change rolls back completely and leaves the original valid state.

pub mod accounts;
pub mod audit;
pub mod http;
pub mod teams;

use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::{ApiError, ErrorCode, FieldErrorCode};

/// Maps database errors raised by the schema's constraints and triggers
/// (SQLSTATE class 23, named constraints from `0001_initial_schema.sql`) and
/// lock conflicts to stable API errors. Anything else is `internal_error`.
pub fn map_db_error(error: sqlx::Error) -> ApiError {
    let Some(db) = error.as_database_error() else {
        return ApiError::internal(error);
    };
    let code = db.code().map(|c| c.into_owned()).unwrap_or_default();
    // 40001 serialization_failure, 40P01 deadlock_detected, 55P03 lock_not_available.
    if matches!(code.as_str(), "40001" | "40P01" | "55P03") {
        return ApiError::conflict();
    }
    if !code.starts_with("23") {
        return ApiError::internal(error);
    }
    match db.constraint() {
        Some("teams_active_name_key") => ApiError::field("name", FieldErrorCode::AlreadyTaken),
        Some("team_memberships_one_current_per_account" | "team_memberships_no_admin") => {
            ApiError::new(ErrorCode::TeamMembershipConflict)
        }
        Some(
            "teams_exactly_one_lead"
            | "team_memberships_lead_role"
            | "team_memberships_lead_active",
        ) => ApiError::new(ErrorCode::TeamLeadRequired),
        Some("team_memberships_active_account") => {
            ApiError::field("accountId", FieldErrorCode::InvalidValue)
        }
        // A concurrent appointment won, or the team was archived meanwhile.
        Some(
            "team_memberships_one_current_lead_per_team"
            | "team_memberships_active_team"
            | "teams_archived_without_members"
            | "tickets_assignee_team_member",
        ) => ApiError::conflict(),
        _ => ApiError::internal(error),
    }
}

/// Fails with 403 unless `actor_id` is an active Admin. Share-locks the
/// actor row so a concurrent demotion/deactivation of the actor serializes.
pub(crate) async fn require_admin_actor(
    conn: &mut PgConnection,
    actor_id: Uuid,
) -> Result<(), ApiError> {
    let ok: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM accounts WHERE id = $1 AND role = 'admin' AND status = 'active' FOR SHARE",
    )
    .bind(actor_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    ok.map(|_| ()).ok_or_else(ApiError::forbidden)
}

/// Locks every active Admin row (in id order, so concurrent callers cannot
/// deadlock) and returns their ids. Role and status changes take this lock
/// first, which serializes last-active-Admin checks.
pub(crate) async fn lock_active_admins(conn: &mut PgConnection) -> Result<Vec<Uuid>, ApiError> {
    sqlx::query_scalar(
        "SELECT id FROM accounts WHERE role = 'admin' AND status = 'active' \
         ORDER BY id FOR NO KEY UPDATE",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Trims a bounded free-text value; `None` for blank input.
pub(crate) fn trimmed(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
