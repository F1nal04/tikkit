//! The audit log: one append-only `audit_events` table for privileged changes,
//! security events, and (mirrored by a database trigger) every ticket event.
//! See docs/audit.md.
//!
//! - [`record`] is the only writer. It adds the current request or job ID
//!   ([`telemetry::request_id`], the one source of correlation IDs), redacts
//!   secret-looking values, and leaves the actor snapshot (username, role,
//!   team) and the resource to the database trigger, so no caller can forget
//!   them. Write it on the transaction of the change it describes.
//! - [`ticket_denied`] records the denied attempts the spec wants kept, after
//!   the failed transaction is gone, without touching the ticket.
//! - [`purge_before`] is the one way audit rows are ever deleted (retention).
//! - [`search`], [`actions`], and [`export`] are the Admin read side.

use std::sync::Arc;

use axum::{
    Extension,
    extract::{Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgExecutor, PgPool, Postgres, QueryBuilder};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, Cursor, Json, Page, Pagination, Query, Role},
    auth::{Auth, Client, CurrentUser, ident},
    authz::{Permission, require_permission},
    csv, telemetry,
};

/// No account: a request without a session (failed login, invitation link).
pub const ANONYMOUS: &str = "anonymous";
/// The audit retention job (#43), when no Admin triggered it.
pub const RETENTION: &str = "retention";

/// Longest free-text filter value (`resource_id`, a username).
const TEXT_MAX: usize = 128;
const REDACTED: &str = "[redacted]";
/// Most rows one export returns; a longer result is cut and marked truncated.
pub const EXPORT_MAX: i64 = 10_000;

/// Who acted: an account (by immutable ID), or a named service identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Account(Uuid),
    Service(&'static str),
}

/// One audit event. `action` is a stable snake_case identifier. `metadata`
/// holds identifiers, codes, and before/after settings: never passwords, keys,
/// tokens, links, or ticket and message text.
#[derive(Debug)]
pub struct Entry<'a> {
    pub action: &'a str,
    pub who: Who,
    /// The account the event is about, if any.
    pub subject: Option<Uuid>,
    /// The ticket the event is about, if any (becomes the resource).
    pub ticket: Option<Uuid>,
    pub metadata: Value,
}

/// Appends one audit event. Pass the transaction of the change it describes,
/// so both commit or roll back together.
pub async fn record<'c>(db: impl PgExecutor<'c>, entry: Entry<'_>) -> Result<(), sqlx::Error> {
    let (account, service) = match entry.who {
        Who::Account(id) => (Some(id), None),
        Who::Service(name) => (None, Some(name)),
    };
    let mut metadata = entry.metadata;
    redact(&mut metadata);
    sqlx::query(
        "INSERT INTO audit_events (event_type, actor_account_id, actor_service, subject_account_id,
                                   metadata, request_id, resource_type, resource_id, ticket_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(entry.action)
    .bind(account)
    .bind(service)
    .bind(entry.subject)
    .bind(metadata)
    .bind(telemetry::request_id())
    .bind(entry.ticket.map(|_| "ticket"))
    .bind(entry.ticket.map(|id| id.to_string()))
    .bind(entry.ticket)
    .execute(db)
    .await
    .map(drop)
}

/// A background job's event, under its service name.
pub async fn system<'c>(
    db: impl PgExecutor<'c>,
    service: &'static str,
    action: &str,
    metadata: Value,
) -> Result<(), sqlx::Error> {
    let entry = Entry {
        action,
        who: Who::Service(service),
        subject: None,
        ticket: None,
        metadata,
    };
    record(db, entry).await
}

/// Key fragments that mark a value as secret, matched case-insensitively.
const SENSITIVE: [&str; 11] = [
    "password",
    "pass",
    "pwd",
    "token",
    "secret",
    "api_key",
    "private_key",
    "credential",
    "bearer",
    "authorization",
    "cookie",
];

/// Backstop, not the rule: whatever sits under a secret-looking key is
/// replaced, whether it is a string, a list, or an object. Booleans and
/// numbers stay: flags such as `password_changed: true` and numeric settings
/// such as `org_token_limit` are not secrets.
fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                let key = key.to_lowercase();
                if SENSITIVE.iter().any(|s| key.contains(s)) {
                    if !matches!(item, Value::Bool(_) | Value::Number(_) | Value::Null) {
                        *item = Value::String(REDACTED.into());
                    }
                } else {
                    redact(item);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        _ => {}
    }
}

/// Records a denied ticket action where the spec asks for it, and nothing else:
///
/// - a failed attempt to change an AI-solved record (`ai_solved_change_denied`);
/// - a reopen or requester cancellation the current policy or the caller's
///   access denied (`ticket_reopen_denied`, `ticket_cancel_denied`).
///
/// Only for a ticket the caller may see. For a ticket that is missing or
/// invisible to them nothing is written, exactly as for a ticket that does not
/// exist, so a denial never confirms that a ticket exists. Repeats of one
/// action by one account on one ticket are recorded a bounded number of times
/// per window ([`Auth::allow_denial_record`]).
///
/// Call it after the failed transaction is rolled back. It writes one audit
/// row and never a ticket event, so the ticket, its history, and every
/// statistic stay as they were. A failure to write is logged, not returned:
/// the caller still answers with the original error.
pub async fn ticket_denied(
    pool: &PgPool,
    auth: &Auth,
    account: Uuid,
    ticket: Uuid,
    attempted: &str,
    error: &ApiError,
) {
    let result = async {
        // One statement for a missing, an invisible, and a visible ticket alike.
        type Found = (
            Option<String>,
            Option<Uuid>,
            Option<Uuid>,
            Option<Uuid>,
            bool,
            String,
            Option<String>,
        );
        let row: Option<Found> = sqlx::query_as(
            "SELECT t.status::text, t.requester_account_id, t.team_id, t.assignee_account_id,
                    o.allow_reopening, o.requester_cancellation_policy,
                    (SELECT a.role::text FROM accounts a WHERE a.id = $2 AND a.is_active)
             FROM organization o LEFT JOIN tickets t ON t.id = $1",
        )
        .bind(ticket)
        .bind(account)
        .fetch_optional(pool)
        .await?;
        let team = crate::authz::team_of(pool, account).await?;
        // No such ticket: its columns are NULL.
        let Some((
            Some(status),
            Some(requester),
            team_id,
            assignee,
            allow_reopening,
            policy,
            Some(role),
        )) = row
        else {
            return Ok(());
        };
        let actor = crate::authz::Actor {
            id: account,
            role: ident(role).map_err(|_| sqlx::Error::RowNotFound)?,
            team,
        };
        let scope = crate::authz::TicketScope {
            requester_account_id: requester,
            team_id,
            assignee_account_id: assignee,
            status: ident(status.clone()).map_err(|_| sqlx::Error::RowNotFound)?,
        };
        if crate::authz::ticket_view(&actor, &scope).is_none() {
            return Ok(());
        }
        let denied = matches!(error, ApiError::Forbidden);
        let mut metadata = json!({
            "attempted": attempted,
            "status": status,
            "error": error.code(),
        });
        let action = if status == "ai_solved" {
            "ai_solved_change_denied"
        } else if attempted == "reopen" && denied {
            metadata["allow_reopening"] = json!(allow_reopening);
            "ticket_reopen_denied"
        } else if attempted == "cancel" && denied {
            metadata["cancellation_policy"] = json!(policy);
            "ticket_cancel_denied"
        } else {
            return Ok(());
        };
        if !auth.allow_denial_record(account, ticket, attempted) {
            return Ok(());
        }
        let entry = Entry {
            action,
            who: Who::Account(account),
            subject: None,
            ticket: Some(ticket),
            metadata,
        };
        record(pool, entry).await
    }
    .await;
    if let Err(error) = result {
        // The code only: database errors can carry statement text.
        let _ = error;
        tracing::error!(
            error_code = "audit_denial_not_recorded",
            "could not record a denied ticket action"
        );
    }
}

/// Middleware for `POST /api/invitations/accept`: a body with a field the
/// contract does not have (an invitee trying to set `role`, for example) is
/// rejected by the extractor before the handler runs; this records the attempt.
/// The token is not read and never stored.
pub async fn invitation_tampering(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    request: Request,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    if let Some(ApiError::UnknownField(field)) = response.extensions().get::<ApiError>()
        // The extractor rejects these before the handler's own limiter runs:
        // bound the rows one client can write. The response stays the same.
        && auth.limit_ip("invitation_rejected", &client).is_ok()
    {
        // The name is the caller's text: keep it only when it looks like a field name.
        let plain = field.len() <= 64
            && field
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        let entry = Entry {
            action: "invitation_rejected",
            who: Who::Service(ANONYMOUS),
            subject: None,
            ticket: None,
            metadata: json!({
                "reason": "unexpected_field",
                "field": if plain { field.as_str() } else { "other" },
                "ip": client.ip,
            }),
        };
        if let Err(error) = record(&state.pool, entry).await {
            tracing::error!(%error, "could not record a rejected invitation");
        }
    }
    response
}

/// Shortest retention the database accepts: [`purge_before`] refuses a cutoff
/// less than this many days in the past (`audit_retention_floor()` in SQL).
pub const RETENTION_FLOOR_DAYS: i64 = 30;

/// Retention (#43): deletes audit events older than `cutoff` through the one
/// database path that may (`audit_retention_purge`) and appends
/// `audit_retention_purged` with the cutoff, the count, and who ran it. Returns
/// the number of deleted events. `who` must be an active Admin account or
/// [`RETENTION`]; `cutoff` must be at least [`RETENTION_FLOOR_DAYS`] in the
/// past; `audit_retention_purged` events are never deleted. Violations are
/// database errors (`42501`, `23514`).
pub async fn purge_before(
    tx: &mut sqlx::PgConnection,
    cutoff: OffsetDateTime,
    who: Who,
) -> Result<i64, sqlx::Error> {
    let admin = match who {
        Who::Account(id) => Some(id),
        Who::Service(_) => None,
    };
    sqlx::query_scalar("SELECT audit_retention_purge($1, $2)")
        .bind(cutoff)
        .bind(admin)
        .fetch_one(tx)
        .await
}

/// Anonymization (#43): blanks the username snapshots of `account` in the log
/// and removes the `ip` key from the events it acted in or is the subject of,
/// through the one database path that may update audit rows
/// (`audit_anonymize_account`). Account IDs, actions, timestamps, and all other
/// metadata stay. Appends `audit_account_anonymized` and returns the number of
/// events changed. `who` must be an active Admin account or [`RETENTION`].
///
/// New events snapshot `accounts.username` as it is then, so anonymize the
/// account row first.
pub async fn anonymize_account(
    tx: &mut sqlx::PgConnection,
    account: Uuid,
    who: Who,
) -> Result<i64, sqlx::Error> {
    let admin = match who {
        Who::Account(id) => Some(id),
        Who::Service(_) => None,
    };
    sqlx::query_scalar("SELECT audit_anonymize_account($1, $2)")
        .bind(account)
        .bind(admin)
        .fetch_one(tx)
        .await
}

/// [`purge_before`] for at most `limit` events, oldest first, so a long
/// history is deleted in bounded transactions. Call it until it returns 0.
/// Each call that deletes something appends its own `audit_retention_purged`.
pub async fn purge_batch(
    tx: &mut sqlx::PgConnection,
    cutoff: OffsetDateTime,
    who: Who,
    limit: i32,
) -> Result<i64, sqlx::Error> {
    let admin = match who {
        Who::Account(id) => Some(id),
        Who::Service(_) => None,
    };
    sqlx::query_scalar("SELECT audit_retention_purge($1, $2, $3)")
        .bind(cutoff)
        .bind(admin)
        .bind(limit)
        .fetch_one(tx)
        .await
}

// ------------------------------------------------------------------ read side

/// Filters of the audit search and export. All are optional and combine with AND.
#[derive(Debug, Default, Deserialize, Serialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    /// Acting account ID.
    pub actor: Option<Uuid>,
    /// Acting account's username (case-insensitive, exact).
    pub actor_username: Option<String>,
    /// Action identifier, e.g. `login_failed` or `ticket_ai_solved`.
    pub action: Option<String>,
    /// Resource type, e.g. `ticket`, `account`, `team`, `invitation`.
    pub resource_type: Option<String>,
    /// Resource identifier within its type.
    pub resource_id: Option<String>,
    /// Events about this ticket.
    pub ticket_id: Option<Uuid>,
    /// Events about the ticket with this reference number.
    pub ticket_reference: Option<i64>,
    /// Events written by one request, or by the jobs it caused (`x-request-id`, a UUID).
    pub request_id: Option<Uuid>,
    /// Inclusive event time, RFC 3339.
    pub created_from: Option<String>,
    /// Exclusive event time, RFC 3339.
    pub created_before: Option<String>,
    /// Page size, 1-100 (default 25). Ignored by the export.
    #[serde(skip_serializing)]
    pub limit: Option<i64>,
    /// `next_cursor` from the previous page. Ignored by the export.
    #[serde(skip_serializing)]
    pub cursor: Option<String>,
}

/// Who acted, as it was when the event was written.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditActor {
    /// `null` when a service acted.
    #[schema(required = true)]
    pub account_id: Option<Uuid>,
    #[schema(required = true)]
    pub username: Option<String>,
    /// The account's role at the time. `null` for services and for events
    /// older than this field.
    #[schema(required = true)]
    pub role: Option<Role>,
    /// The account's team at the time, if it had one.
    #[schema(required = true)]
    pub team_id: Option<Uuid>,
    #[schema(required = true)]
    pub team_name: Option<String>,
    /// Set exactly when no account acted: `anonymous` or the background service.
    #[schema(required = true)]
    pub service: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditAccount {
    pub account_id: Uuid,
    #[schema(required = true)]
    pub username: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEvent {
    pub id: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    /// Stable machine identifier.
    pub action: String,
    pub actor: AuditActor,
    /// The account the event is about, if any.
    #[schema(required = true)]
    pub subject: Option<AuditAccount>,
    #[schema(required = true)]
    pub resource_type: Option<String>,
    #[schema(required = true)]
    pub resource_id: Option<String>,
    #[schema(required = true)]
    pub ticket_id: Option<Uuid>,
    /// `null` when the event is not about a ticket.
    #[schema(required = true)]
    pub ticket_reference: Option<i64>,
    /// The request that wrote the event, or that caused the job that wrote it.
    #[schema(required = true)]
    pub request_id: Option<Uuid>,
    /// Identifiers, codes, and before/after settings. Never secrets or content.
    #[schema(value_type = Object)]
    pub metadata: Value,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    created_at: OffsetDateTime,
    event_type: String,
    actor_account_id: Option<Uuid>,
    actor_username: Option<String>,
    actor_role: Option<String>,
    actor_team_id: Option<Uuid>,
    actor_team_name: Option<String>,
    actor_service: Option<String>,
    subject_account_id: Option<Uuid>,
    subject_username: Option<String>,
    resource_type: Option<String>,
    resource_id: Option<String>,
    ticket_id: Option<Uuid>,
    ticket_reference: Option<i64>,
    request_id: Option<Uuid>,
    metadata: Value,
}

impl TryFrom<Row> for AuditEvent {
    type Error = ApiError;

    fn try_from(row: Row) -> Result<Self, ApiError> {
        Ok(Self {
            id: row.id,
            created_at: row.created_at,
            action: row.event_type,
            actor: AuditActor {
                account_id: row.actor_account_id,
                username: row.actor_username,
                role: row.actor_role.map(ident).transpose()?,
                team_id: row.actor_team_id,
                team_name: row.actor_team_name,
                service: row.actor_service,
            },
            subject: row.subject_account_id.map(|account_id| AuditAccount {
                account_id,
                username: row.subject_username,
            }),
            resource_type: row.resource_type,
            resource_id: row.resource_id,
            ticket_id: row.ticket_id,
            ticket_reference: row.ticket_reference,
            request_id: row.request_id,
            metadata: row.metadata,
        })
    }
}

/// Validated filters with usernames and references resolved to IDs.
struct Filters {
    actor: Option<Uuid>,
    action: Option<String>,
    resource_type: Option<String>,
    resource_id: Option<String>,
    ticket: Option<Uuid>,
    request_id: Option<Uuid>,
    from: Option<OffsetDateTime>,
    before: Option<OffsetDateTime>,
    /// A filter names an account or ticket that does not exist: nothing matches.
    nothing: bool,
}

fn instant(value: &Option<String>) -> Result<Option<OffsetDateTime>, ApiError> {
    value
        .as_deref()
        .map(|s| OffsetDateTime::parse(s, &Rfc3339).map_err(|_| ApiError::InvalidQuery))
        .transpose()
}

/// A stable identifier: 1-64 lowercase letters, digits, or underscores.
fn identifier(value: &Option<String>) -> Result<Option<String>, ApiError> {
    match value.as_deref() {
        None => Ok(None),
        Some(v)
            if (1..=64).contains(&v.len())
                && v.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') =>
        {
            Ok(Some(v.to_owned()))
        }
        Some(_) => Err(ApiError::InvalidQuery),
    }
}

fn bounded(value: &Option<String>) -> Result<Option<String>, ApiError> {
    match value.as_deref() {
        None => Ok(None),
        Some(v) if (1..=TEXT_MAX).contains(&v.chars().count()) => Ok(Some(v.to_owned())),
        Some(_) => Err(ApiError::InvalidQuery),
    }
}

async fn resolve(db: &mut sqlx::PgConnection, q: &AuditQuery) -> Result<Filters, ApiError> {
    let (from, before) = (instant(&q.created_from)?, instant(&q.created_before)?);
    if let (Some(from), Some(before)) = (from, before)
        && from >= before
    {
        return Err(ApiError::InvalidQuery);
    }
    let mut filters = Filters {
        actor: q.actor,
        action: identifier(&q.action)?,
        resource_type: identifier(&q.resource_type)?,
        resource_id: bounded(&q.resource_id)?,
        ticket: q.ticket_id,
        request_id: q.request_id,
        from,
        before,
        nothing: false,
    };
    if let Some(username) = bounded(&q.actor_username)? {
        // Usernames are unique, emails are not: this names exactly one account.
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM accounts WHERE username_normalized = lower(normalize(btrim($1), NFKC))",
        )
        .bind(username)
        .fetch_optional(&mut *db)
        .await?;
        filters.nothing |= id.is_none() || filters.actor.is_some_and(|a| Some(a) != id);
        filters.actor = id;
    }
    if let Some(reference) = q.ticket_reference {
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM tickets WHERE reference = $1")
            .bind(reference)
            .fetch_optional(&mut *db)
            .await?;
        filters.nothing |= id.is_none() || filters.ticket.is_some_and(|t| Some(t) != id);
        filters.ticket = id;
    }
    Ok(filters)
}

/// Matching events, newest first, at most `limit`, starting after `cursor`.
async fn fetch(
    db: &mut sqlx::PgConnection,
    f: &Filters,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Vec<AuditEvent>, ApiError> {
    if f.nothing {
        return Ok(Vec::new());
    }
    let mut sql: QueryBuilder<'_, Postgres> = QueryBuilder::new(
        "SELECT e.id, e.created_at, e.event_type, e.actor_account_id, e.actor_username,
                e.actor_role::text AS actor_role, e.actor_team_id, tm.name AS actor_team_name,
                e.actor_service, e.subject_account_id, e.subject_username, e.resource_type,
                e.resource_id, e.ticket_id, t.reference AS ticket_reference, e.request_id,
                e.metadata
         FROM audit_events e
         LEFT JOIN teams tm ON tm.id = e.actor_team_id
         LEFT JOIN tickets t ON t.id = e.ticket_id
         WHERE true",
    );
    if let Some(actor) = f.actor {
        sql.push(" AND e.actor_account_id = ").push_bind(actor);
    }
    if let Some(action) = &f.action {
        sql.push(" AND e.event_type = ").push_bind(action);
    }
    if let Some(resource_type) = &f.resource_type {
        sql.push(" AND e.resource_type = ").push_bind(resource_type);
    }
    if let Some(resource_id) = &f.resource_id {
        sql.push(" AND e.resource_id = ").push_bind(resource_id);
    }
    if let Some(ticket) = f.ticket {
        sql.push(" AND e.ticket_id = ").push_bind(ticket);
    }
    if let Some(request_id) = f.request_id {
        sql.push(" AND e.request_id = ").push_bind(request_id);
    }
    if let Some(from) = f.from {
        sql.push(" AND e.created_at >= ").push_bind(from);
    }
    if let Some(before) = f.before {
        sql.push(" AND e.created_at < ").push_bind(before);
    }
    if let Some(cursor) = cursor {
        sql.push(" AND (e.created_at, e.id) < (")
            .push_bind(cursor.created_at)
            .push(", ")
            .push_bind(cursor_id(cursor.id))
            .push(")");
    }
    sql.push(" ORDER BY e.created_at DESC, e.id DESC LIMIT ")
        .push_bind(limit);
    let rows: Vec<Row> = sql.build_query_as().fetch_all(db).await?;
    rows.into_iter().map(AuditEvent::try_from).collect()
}

/// Audit rows have numeric IDs; the shared page cursor carries a UUID.
fn cursor_key(event: &AuditEvent) -> Cursor {
    Cursor {
        created_at: event.created_at,
        id: Uuid::from_u128(event.id as u128),
    }
}

fn cursor_id(id: Uuid) -> i64 {
    i64::try_from(id.as_u128()).unwrap_or(i64::MAX)
}

/// Read-only snapshot: a search can never write, not even an audit row.
async fn read_only(pool: &PgPool) -> Result<sqlx::Transaction<'_, Postgres>, ApiError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// The audit log, newest first (Admin only). Searching is a pure read: it
/// writes nothing, so reviewing history leaves no trace in it.
#[utoipa::path(get, path = "/api/admin/audit", tag = "audit", params(AuditQuery),
    responses(
        (status = 200, body = Page<AuditEvent>),
        (status = 400, description = "`invalid_query` (unknown parameter, malformed value, or empty time range)", body = crate::api::ErrorEnvelope),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`cursor: invalid_cursor`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Page<AuditEvent>>, ApiError> {
    require_permission(&user, Permission::ViewReports)?;
    let page = Pagination {
        limit: query.limit,
        cursor: query.cursor.clone(),
    };
    let cursor = page.cursor()?;
    let mut tx = read_only(&state.pool).await?;
    let filters = resolve(&mut tx, &query).await?;
    let events = fetch(&mut tx, &filters, cursor, page.limit() + 1).await?;
    Ok(Json(Page::from_rows(events, page.limit(), cursor_key)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditActions {
    /// Every action identifier present in the log, sorted.
    pub actions: Vec<String>,
}

/// The action identifiers that occur in the log, for the search filter (Admin only).
#[utoipa::path(get, path = "/api/admin/audit/actions", tag = "audit",
    responses(
        (status = 200, body = AuditActions),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn actions(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<AuditActions>, ApiError> {
    require_permission(&user, Permission::ViewReports)?;
    // A skip scan over the (event_type, ...) index: one probe per distinct
    // action instead of reading every row.
    let actions = sqlx::query_scalar(
        "WITH RECURSIVE seen AS (
             (SELECT event_type FROM audit_events ORDER BY event_type LIMIT 1)
             UNION ALL
             SELECT (SELECT e.event_type FROM audit_events e
                     WHERE e.event_type > seen.event_type ORDER BY e.event_type LIMIT 1)
             FROM seen WHERE seen.event_type IS NOT NULL
         )
         SELECT event_type FROM seen WHERE event_type IS NOT NULL ORDER BY event_type",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(AuditActions { actions }))
}

const EXPORT_COLUMNS: [&str; 17] = [
    "id",
    "time_utc",
    "action",
    "actor_account_id",
    "actor_username",
    "actor_role",
    "actor_team_id",
    "actor_team_name",
    "actor_service",
    "subject_account_id",
    "subject_username",
    "resource_type",
    "resource_id",
    "ticket_reference",
    "request_id",
    "metadata",
    "ticket_id",
];

/// Exports the matching events as CSV, newest first (Admin only). Bounded: at
/// most 10,000 rows; `x-export-truncated: true` says a longer result was cut
/// (narrow the filters or the time range). Cells that a spreadsheet would run
/// as a formula are neutralized. The export itself is audited
/// (`audit_exported`: who, the filters, the row count).
#[utoipa::path(post, path = "/api/admin/audit/export", tag = "audit", params(AuditQuery),
    responses(
        (status = 200, description = "CSV with a header row. Headers `x-export-rows` and `x-export-truncated`.", content_type = "text/csv", body = String),
        (status = 400, description = "`invalid_query`", body = crate::api::ErrorEnvelope),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin) or `csrf_rejected`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn export(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<AuditQuery>,
) -> Result<Response, ApiError> {
    require_permission(&user, Permission::ViewReports)?;
    let mut tx = state.pool.begin().await?;
    // One snapshot for the rows and the count that is recorded about them.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let filters = resolve(&mut tx, &query).await?;
    let mut events = fetch(&mut tx, &filters, None, EXPORT_MAX + 1).await?;
    let truncated = events.len() as i64 > EXPORT_MAX;
    events.truncate(EXPORT_MAX as usize);

    let mut out = csv::line(EXPORT_COLUMNS);
    for e in &events {
        let text = |v: &Option<String>| v.clone().unwrap_or_default();
        let id = |v: Option<Uuid>| v.map(|v| v.to_string()).unwrap_or_default();
        let subject = e.subject.as_ref();
        out.push_str(&csv::line([
            e.id.to_string(),
            e.created_at
                .format(&Rfc3339)
                .map_err(|_| ApiError::Internal)?,
            e.action.clone(),
            id(e.actor.account_id),
            text(&e.actor.username),
            e.actor.role.map(crate::auth::ident_str).unwrap_or_default(),
            id(e.actor.team_id),
            text(&e.actor.team_name),
            text(&e.actor.service),
            id(subject.map(|s| s.account_id)),
            subject.and_then(|s| s.username.clone()).unwrap_or_default(),
            text(&e.resource_type),
            text(&e.resource_id),
            e.ticket_reference
                .map(|r| r.to_string())
                .unwrap_or_default(),
            id(e.request_id),
            e.metadata.to_string(),
            id(e.ticket_id),
        ]));
    }
    let entry = Entry {
        action: "audit_exported",
        who: Who::Account(user.account_id()),
        subject: None,
        ticket: None,
        metadata: json!({ "filters": query, "rows": events.len(), "truncated": truncated }),
    };
    record(&mut *tx, entry).await?;
    tx.commit().await?;

    let date = OffsetDateTime::now_utc().date();
    let disposition = format!("attachment; filename=\"tikkit-audit-{date}.csv\"");
    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/csv; charset=utf-8"),
        ),
        (
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&disposition).map_err(|_| ApiError::Internal)?,
        ),
        (
            header::HeaderName::from_static("x-export-rows"),
            HeaderValue::from(events.len() as u64),
        ),
        (
            header::HeaderName::from_static("x-export-truncated"),
            HeaderValue::from_static(if truncated { "true" } else { "false" }),
        ),
    ];
    Ok((headers, out).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_under_a_secret_looking_key_is_redacted_and_flags_kept() {
        // Assembled at runtime: nothing here is a real credential.
        let secret = ["not", "a", "real", "value"].join("-");
        let mut value = json!({
            "password_changed": true,
            "credential_replaced": false,
            "org_token_limit": 5000,
            "password": secret,
            "nested": [{ "reset_token": secret, "count": 3 }],
            "API_KEY": secret,
            "Api_Keys": [secret, secret],
            "smtp_password": { "value": secret },
            "SMTP_PWD": secret,
            "passphrase": secret,
            "private_key": [{ "pem": secret }],
            "provider_credential": { "key": secret, "hint": "abcd" },
            "Bearer": secret,
            "session_cookie": null,
            "host": "mail.example",
            "to": { "enabled": true, "Authorization": [secret] },
        });
        redact(&mut value);
        assert_eq!(
            value,
            json!({
                "password_changed": true,
                "credential_replaced": false,
                "org_token_limit": 5000,
                "password": REDACTED,
                "nested": [{ "reset_token": REDACTED, "count": 3 }],
                "API_KEY": REDACTED,
                "Api_Keys": REDACTED,
                "smtp_password": REDACTED,
                "SMTP_PWD": REDACTED,
                "passphrase": REDACTED,
                "private_key": REDACTED,
                "provider_credential": REDACTED,
                "Bearer": REDACTED,
                "session_cookie": null,
                "host": "mail.example",
                "to": { "enabled": true, "Authorization": REDACTED },
            })
        );
        assert!(!value.to_string().contains(&secret));
    }

    #[test]
    fn numeric_ids_round_trip_through_the_page_cursor() {
        for id in [1_i64, 42, i64::MAX] {
            assert_eq!(cursor_id(Uuid::from_u128(id as u128)), id);
        }
    }
}
