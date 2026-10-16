//! Minimal writer for `audit_events` (append-only). Metadata must never
//! contain secrets: no passwords, tokens or token hashes.
//!
//! Generic enough for other modules; may move to a crate-level `audit`
//! module once more features record events.

use serde_json::Value;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

use crate::request_id;

/// Who performed the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditActor {
    Account(Uuid),
    System,
    Anonymous,
}

/// Stable action identifiers recorded by authentication.
pub mod actions {
    pub const SETUP_COMPLETED: &str = "setup.completed";
    pub const LOGIN_SUCCEEDED: &str = "auth.login_succeeded";
    pub const LOGIN_FAILED: &str = "auth.login_failed";
    pub const LOGOUT: &str = "auth.logout";
    pub const SESSION_REVOKED: &str = "auth.session_revoked";
}

#[derive(Clone, Debug)]
pub struct AuditEvent<'a> {
    pub actor: AuditActor,
    pub action: &'a str,
    pub target: Option<(&'a str, String)>,
    pub metadata: Value,
}

impl<'a> AuditEvent<'a> {
    pub fn new(actor: AuditActor, action: &'a str) -> Self {
        Self {
            actor,
            action,
            target: None,
            metadata: Value::Object(Default::default()),
        }
    }

    pub fn target(mut self, kind: &'a str, id: impl ToString) -> Self {
        self.target = Some((kind, id.to_string()));
        self
    }

    pub fn metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Inserts `event`, attaching the current request id. Run it on the same
/// transaction as the change it describes.
pub async fn record<'e, E>(executor: E, event: AuditEvent<'_>) -> Result<(), sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let (kind, account) = match event.actor {
        AuditActor::Account(id) => ("account", Some(id)),
        AuditActor::System => ("system", None),
        AuditActor::Anonymous => ("anonymous", None),
    };
    let (target_type, target_id) = match event.target {
        Some((kind, id)) => (Some(kind), Some(id)),
        None => (None, None),
    };
    sqlx::query(
        "INSERT INTO audit_events (id, actor_account_id, actor_kind, action, target_type, \
                                   target_id, metadata, request_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .bind(kind)
    .bind(event.action)
    .bind(target_type)
    .bind(target_id)
    .bind(event.metadata)
    .bind(request_id::current())
    .execute(executor)
    .await?;
    Ok(())
}
