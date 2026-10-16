//! Audit action identifiers for admin directory changes and a thin adapter
//! over the shared audit writer.
//!
//! Rows go to the append-only `audit_events` table in the caller's
//! transaction, so an action and its audit record commit or roll back
//! together. Metadata must be safe: ids, role/status identifiers, team names,
//! never secrets or credentials.

use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::map_db_error;
use crate::{
    auth::audit::{self as shared, AuditActor, AuditEvent},
    error::ApiError,
};

/// Stable audit action identifiers written by this module.
pub mod action {
    pub const ACCOUNT_ROLE_CHANGED: &str = "account.role_changed";
    pub const ACCOUNT_DEACTIVATED: &str = "account.deactivated";
    pub const ACCOUNT_REACTIVATED: &str = "account.reactivated";
    pub const TEAM_CREATED: &str = "team.created";
    pub const TEAM_RENAMED: &str = "team.renamed";
    pub const TEAM_LEAD_REPLACED: &str = "team.lead_replaced";
    pub const TEAM_MEMBER_ADDED: &str = "team.member_added";
    pub const TEAM_MEMBER_REMOVED: &str = "team.member_removed";
    pub const TEAM_ARCHIVED: &str = "team.archived";
}

/// Target types used in `audit_events.target_type`.
pub mod target {
    pub const ACCOUNT: &str = "account";
    pub const TEAM: &str = "team";
}

/// Appends one attributable audit event by an Admin account through the
/// shared writer [`crate::auth::audit::record`] (which adds the request id).
pub async fn record(
    conn: &mut PgConnection,
    actor_id: Uuid,
    action: &str,
    target_type: &str,
    target_id: Uuid,
    metadata: Value,
) -> Result<(), ApiError> {
    let event = AuditEvent::new(AuditActor::Account(actor_id), action)
        .target(target_type, target_id)
        .metadata(metadata);
    shared::record(&mut *conn, event)
        .await
        .map_err(map_db_error)
}
