//! One account's personal data (#43): what is stored, a scoped export, and
//! anonymization. Admin only. See docs/retention.md.
//!
//! Accounts are addressed by ID. The Admin additionally types the account's
//! username, which must match that ID's current username: a request aimed at
//! the wrong account fails instead of exporting or erasing it. Nothing here
//! looks at an email address.

use std::sync::Arc;

use axum::{
    Extension,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, PgConnection};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, Json, Path, Query, Role},
    audit::{self, Entry, Who},
    auth::CurrentUser,
    authz::{Permission, require_permission},
    retention::{self, RetentionCounts},
};

/// Rows per export section. A section that has more is cut and named in
/// `truncated`.
pub const SECTION_MAX: usize = 10_000;
/// Bytes of JSON rows one export may hold in total. A larger one is refused
/// with `413 export_too_large` before it is built.
pub const EXPORT_MAX_BYTES: i64 = 16 * 1024 * 1024;

fn invalid(field: &str, code: &'static str) -> ApiError {
    ApiError::Validation([(field.to_owned(), vec![code])].into())
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PersonalDataQuery {
    /// Exact username (case-insensitive). Resolves to one account ID.
    pub username: String,
}

/// What is stored about one account, as counts. Nothing is changed.
#[derive(Debug, Serialize, ToSchema, FromRow)]
pub struct PersonalDataSummary {
    pub account_id: Uuid,
    pub username: String,
    pub display_name: String,
    #[schema(value_type = Role)]
    pub role: String,
    pub is_active: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub anonymized_at: Option<OffsetDateTime>,
    /// Tickets the account submitted.
    pub tickets: i64,
    /// Of those, not in a final status. Anonymization leaves them open.
    pub open_tickets: i64,
    /// Of those, in a final status with their content still stored.
    pub final_tickets_with_content: i64,
    pub drafts: i64,
    pub pending_screenshots: i64,
    pub sessions: i64,
    /// Audit events the account acted in or is the subject of.
    pub audit_events: i64,
}

const SUMMARY_SQL: &str = "SELECT a.id AS account_id, a.username, a.display_name, a.role::text AS role,
        a.is_active, a.anonymized_at,
        (SELECT count(*) FROM tickets t WHERE t.requester_account_id = a.id AND t.status <> 'draft') AS tickets,
        (SELECT count(*) FROM tickets t WHERE t.requester_account_id = a.id
           AND t.status NOT IN ('draft', 'closed', 'cancelled', 'ai_solved')) AS open_tickets,
        (SELECT count(*) FROM tickets t WHERE t.requester_account_id = a.id
           AND t.status IN ('closed', 'cancelled', 'ai_solved')
           AND t.content_purged_at IS NULL) AS final_tickets_with_content,
        (SELECT count(*) FROM ticket_drafts d WHERE d.requester_account_id = a.id
           AND d.submitted_ticket_id IS NULL) AS drafts,
        (SELECT count(*) FROM screenshots s WHERE s.uploader_account_id = a.id
           AND s.ticket_id IS NULL) AS pending_screenshots,
        (SELECT count(*) FROM sessions s WHERE s.account_id = a.id) AS sessions,
        (SELECT count(*) FROM audit_events e
          WHERE e.actor_account_id = a.id OR e.subject_account_id = a.id) AS audit_events
    FROM accounts a";

/// Finds one account by username and counts what is stored about it (Admin
/// only). Read-only; the preview before an export or anonymization.
#[utoipa::path(get, path = "/api/admin/personal-data", tag = "admin", params(PersonalDataQuery),
    responses(
        (status = 200, body = PersonalDataSummary),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`: no account has that username", body = crate::api::ErrorEnvelope),
    ))]
pub async fn personal_data_summary(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<PersonalDataQuery>,
) -> Result<Json<PersonalDataSummary>, ApiError> {
    require_permission(&user, Permission::ManageAccounts)?;
    let mut tx = crate::submissions::read_only(&state).await?;
    let found: Option<PersonalDataSummary> = sqlx::query_as(&format!(
        "{SUMMARY_SQL} WHERE a.username_normalized = lower(normalize(btrim($1), NFKC))"
    ))
    .bind(&query.username)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    found.map(Json).ok_or(ApiError::NotFound)
}

/// The typed confirmation of a request about one account.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountConfirmation {
    /// The account's current username, typed by the Admin.
    pub username: String,
}

/// Locks the account and checks the typed username against it.
/// Returns `(is_active, anonymized)`.
async fn verified(
    db: &mut PgConnection,
    account: Uuid,
    username: &str,
) -> Result<(bool, bool), ApiError> {
    let row: Option<(bool, bool, bool)> = sqlx::query_as(
        "SELECT is_active, anonymized_at IS NOT NULL,
                username_normalized = lower(normalize(btrim($2), NFKC))
         FROM accounts WHERE id = $1 FOR UPDATE",
    )
    .bind(account)
    .bind(username)
    .fetch_optional(db)
    .await?;
    let (active, anonymized, matches) = row.ok_or(ApiError::NotFound)?;
    if !matches {
        return Err(invalid("username", "mismatch"));
    }
    Ok((active, anonymized))
}

/// One export section, without transferring it: how many bytes its rows are
/// as JSON, cut at [`SECTION_MAX`] rows. Measured in the database so an
/// oversized export is refused before any of it is loaded.
async fn section_bytes(
    db: &mut PgConnection,
    sql: &str,
    account: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT COALESCE(sum(octet_length(to_jsonb(x)::text)), 0)::bigint
         FROM (SELECT * FROM ({sql}) y LIMIT $2) x"
    ))
    .bind(account)
    .bind(SECTION_MAX as i64)
    .fetch_one(db)
    .await
}

/// One export section: the rows of `sql` (bound: the account ID), each as JSON
/// text, cut at [`SECTION_MAX`].
async fn section(
    db: &mut PgConnection,
    sql: &str,
    account: Uuid,
) -> Result<(Vec<String>, bool), sqlx::Error> {
    let mut rows: Vec<String> =
        sqlx::query_scalar(&format!("SELECT to_jsonb(x)::text FROM ({sql}) x LIMIT $2"))
            .bind(account)
            .bind(SECTION_MAX as i64 + 1)
            .fetch_all(db)
            .await?;
    let truncated = rows.len() > SECTION_MAX;
    rows.truncate(SECTION_MAX);
    Ok((rows, truncated))
}

/// The export's sections. Each query is scoped to `$1`, the account.
///
/// Included: what the account entered or was shown. Left out on purpose:
/// password and token hashes, every provider or SMTP credential, internal
/// notes (staff's private working notes, also those about this requester),
/// the identity of staff who replied, the text of messages this account wrote
/// as staff on other people's tickets, and image bytes.
const SECTIONS: [(&str, &str); 12] = [
    (
        "team_history",
        "SELECT h.team_id, t.name AS team_name, h.action, h.created_at
         FROM team_membership_history h JOIN teams t ON t.id = h.team_id
         WHERE h.account_id = $1 ORDER BY h.id",
    ),
    (
        "sessions",
        "SELECT created_at, last_seen_at, expires_at, revoked_at, user_agent, ip
         FROM sessions WHERE account_id = $1 ORDER BY created_at, id",
    ),
    (
        "tickets",
        "SELECT t.id, t.reference, t.status, t.subject, t.description,
                COALESCE(t.intake_snapshot->'catalog'->>'category_name', c.name) AS category,
                p.name AS priority, t.intake_snapshot->'catalog'->'answers' AS answers,
                t.created_at, t.resolution_source, t.closure_reason, t.content_purged_at
         FROM tickets t JOIN categories c ON c.id = t.category_id
              JOIN priorities p ON p.id = t.priority_id
         WHERE t.requester_account_id = $1 AND t.status <> 'draft'
         ORDER BY t.created_at, t.id",
    ),
    (
        "ticket_actions",
        "SELECT t.reference AS ticket_reference, e.event_type, e.from_status, e.to_status,
                CASE WHEN e.event_type = 'cancel' THEN e.payload->>'reason' END AS reason,
                e.created_at
         FROM ticket_events e JOIN tickets t ON t.id = e.ticket_id
         WHERE e.actor_account_id = $1 ORDER BY e.id",
    ),
    (
        "conversation",
        "SELECT t.reference AS ticket_reference, m.created_at,
                CASE WHEN m.author_account_id = $1 THEN 'account' ELSE 'staff' END AS author,
                m.body
         FROM ticket_messages m JOIN tickets t ON t.id = m.ticket_id
         WHERE t.requester_account_id = $1 AND m.kind = 'public_reply'
         ORDER BY m.ticket_id, m.sequence",
    ),
    (
        "staff_messages",
        "SELECT t.reference AS ticket_reference, m.kind, m.created_at
         FROM ticket_messages m JOIN tickets t ON t.id = m.ticket_id
         WHERE m.author_account_id = $1 AND t.requester_account_id <> $1
         ORDER BY m.ticket_id, m.sequence",
    ),
    (
        "ai_suggestions",
        "SELECT t.reference AS ticket_reference, a.state, a.language, a.explanation, a.steps,
                a.created_at, a.finished_at, a.content_purged_at
         FROM ai_attempts a JOIN tickets t ON t.id = a.ticket_id
         WHERE t.requester_account_id = $1 ORDER BY a.created_at, a.id",
    ),
    (
        "screenshots",
        "SELECT s.id, s.filename, s.content_type, s.byte_size, s.width, s.height, s.created_at,
                t.reference AS ticket_reference
         FROM screenshots s LEFT JOIN tickets t ON t.id = s.ticket_id
         WHERE s.uploader_account_id = $1
           AND (s.ticket_id IS NULL OR t.requester_account_id = $1)
           AND s.message_id IS NOT DISTINCT FROM (
               SELECT m.id FROM ticket_messages m
               WHERE m.id = s.message_id AND m.kind = 'public_reply')
         ORDER BY s.created_at, s.id",
    ),
    (
        "drafts",
        "SELECT id, intake, version, created_at, updated_at FROM ticket_drafts
         WHERE requester_account_id = $1 AND submitted_ticket_id IS NULL
         ORDER BY created_at, id",
    ),
    (
        "saved_views",
        "SELECT name, filters, created_at FROM saved_views
         WHERE owner_account_id = $1 ORDER BY created_at, id",
    ),
    (
        "ai_usage",
        "SELECT provider, model, status, outcome, image_count, total_tokens, started_at
         FROM ai_usage WHERE account_id = $1 ORDER BY started_at, id",
    ),
    (
        "audit_events",
        "SELECT created_at, event_type AS action, metadata->>'ip' AS ip
         FROM audit_events WHERE actor_account_id = $1 ORDER BY id DESC",
    ),
];

/// Exports the personal data of one account as a JSON document (Admin only).
/// The typed `username` must be the account's. Bounded: each section holds at
/// most 10,000 rows and `truncated` names the sections that were cut, and the
/// whole export at most 16 MiB (`413 export_too_large` otherwise). Never
/// contains credentials, hashes, internal notes, or other people's data; see
/// docs/retention.md for the exact contents. Writes `personal_data_exported`.
#[utoipa::path(post, path = "/api/admin/accounts/{id}/personal-data/export", tag = "admin",
    params(("id" = Uuid, Path)), request_body = AccountConfirmation,
    responses(
        (status = 200, description = "The export, as an attachment", body = Object, content_type = "application/json"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 413, description = "`export_too_large`: the account's data exceeds 16 MiB", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`username`: `mismatch`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn export_personal_data(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AccountConfirmation>,
) -> Result<Response, ApiError> {
    require_permission(&user, Permission::ManageAccounts)?;
    let mut tx = state.pool.begin().await?;
    verified(&mut tx, id, &body.username).await?;
    let account: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('id', a.id, 'username', a.username,
                'display_name', a.display_name, 'email', a.email, 'role', a.role,
                'language', a.language, 'is_active', a.is_active, 'created_at', a.created_at,
                'expertise', a.expertise, 'notify_email', a.notify_email,
                'notify_in_app', a.notify_in_app, 'anonymized_at', a.anonymized_at,
                'team', (SELECT jsonb_build_object('id', t.id, 'name', t.name)
                         FROM team_memberships m JOIN teams t ON t.id = m.team_id
                         WHERE m.account_id = a.id))
         FROM accounts a WHERE a.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let generated_at: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    // The byte budget, before anything is loaded: one account's messages alone
    // can be hundreds of megabytes.
    let mut bytes = 0;
    for (_, sql) in SECTIONS {
        bytes += section_bytes(&mut tx, sql, id).await?;
        if bytes > EXPORT_MAX_BYTES {
            return Err(ApiError::ExportTooLarge);
        }
    }
    // Rows stay the JSON text PostgreSQL produced; the document is written
    // once, without a parsed copy.
    let generated_at = generated_at
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| ApiError::Internal)?;
    let mut document = format!(
        "{{\"format\":\"tikkit.personal_data.v1\",\"generated_at\":{},\"account\":{account}",
        json!(generated_at)
    );
    let mut counts = Map::new();
    let mut truncated = Vec::new();
    for (name, sql) in SECTIONS {
        let (rows, cut) = section(&mut tx, sql, id).await?;
        counts.insert(name.into(), json!(rows.len()));
        if cut {
            truncated.push(name);
        }
        document.push_str(&format!(",\"{name}\":["));
        for (n, row) in rows.iter().enumerate() {
            if n > 0 {
                document.push(',');
            }
            document.push_str(row);
        }
        document.push(']');
    }
    document.push_str(&format!(",\"truncated\":{}}}", json!(truncated)));
    audit::record(
        &mut *tx,
        Entry {
            action: "personal_data_exported",
            who: Who::Account(user.account_id()),
            subject: Some(id),
            ticket: None,
            metadata: json!({ "rows": counts, "truncated": truncated }),
        },
    )
    .await?;
    tx.commit().await?;
    let body = document;
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"personal-data-{id}.json\""),
            ),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
        ],
        body,
    )
        .into_response())
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnonymizeAccount {
    /// The account's current username, typed by the Admin.
    pub username: String,
    /// Also remove the content of the account's own tickets that are in a
    /// final status now (as ticket content retention would later).
    #[serde(default)]
    pub purge_ticket_content: bool,
}

/// What an anonymization removed. Counts only.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct AnonymizationResult {
    pub account_id: Uuid,
    /// The placeholder that replaced the username and display name.
    pub username: String,
    pub sessions: i64,
    pub drafts: i64,
    pub pending_screenshots: i64,
    pub saved_views: i64,
    pub notifications: i64,
    /// Audit events whose username snapshot or IP address was removed.
    pub audit_events: i64,
    /// Content removed from the account's own final tickets, if requested.
    pub ticket_content: RetentionCounts,
}

async fn deleted(db: &mut PgConnection, sql: &str, account: Uuid) -> Result<i64, sqlx::Error> {
    Ok(sqlx::query(sql)
        .bind(account)
        .execute(db)
        .await?
        .rows_affected() as i64)
}

/// Removes the identity of a deactivated account for good (Admin only). The
/// account ID, role, and every ticket, event, and statistic that refers to it
/// stay; see docs/retention.md for the exact policy. The account must be
/// deactivated first, so the last active Admin and an active team's lead can
/// never be anonymized. Irreversible. Writes `account_anonymized`.
#[utoipa::path(post, path = "/api/admin/accounts/{id}/anonymize", tag = "admin",
    params(("id" = Uuid, Path)), request_body = AnonymizeAccount,
    responses(
        (status = 200, body = AnonymizationResult),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict`: a ticket of the account is being changed right now; try again; `restore_mode_blocked`: this copy is in restore mode", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`username`: `mismatch`; `account`: `active`, `already_anonymized`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn anonymize_account(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<crate::auth::Auth>>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AnonymizeAccount>,
) -> Result<Json<AnonymizationResult>, ApiError> {
    let mut tx = crate::management::transaction(&state, &user).await?;
    // Irreversible and destructive: not on a restored copy under verification.
    if auth.settings.restore_mode {
        return Err(ApiError::RestoreModeBlocked);
    }
    let (active, anonymized) = verified(&mut tx, id, &body.username).await?;
    if anonymized {
        return Err(invalid("account", "already_anonymized"));
    }
    if active {
        return Err(invalid("account", "active"));
    }
    let mut result = AnonymizationResult {
        account_id: id,
        ..Default::default()
    };
    result.sessions = deleted(&mut tx, "DELETE FROM sessions WHERE account_id = $1", id).await?;
    deleted(
        &mut tx,
        "DELETE FROM password_resets WHERE account_id = $1",
        id,
    )
    .await?;
    result.saved_views = deleted(
        &mut tx,
        "DELETE FROM saved_views WHERE owner_account_id = $1",
        id,
    )
    .await?;
    result.notifications = deleted(
        &mut tx,
        "DELETE FROM notifications WHERE account_id = $1",
        id,
    )
    .await?;
    deleted(
        &mut tx,
        "DELETE FROM notification_outbox WHERE account_id = $1",
        id,
    )
    .await?;
    result.drafts = deleted(
        &mut tx,
        "DELETE FROM ticket_drafts WHERE requester_account_id = $1 AND submitted_ticket_id IS NULL",
        id,
    )
    .await?;
    let mut files: Vec<Uuid> = sqlx::query_scalar(
        "DELETE FROM screenshots WHERE uploader_account_id = $1 AND ticket_id IS NULL RETURNING id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    result.pending_screenshots = files.len() as i64;
    // Where its invitation link was sent.
    deleted(
        &mut tx,
        "UPDATE invitations SET email = NULL WHERE account_id = $1",
        id,
    )
    .await?;
    // The account row before anything that is audited: new audit events
    // snapshot the username as it is then. The placeholder is in a namespace
    // no other account can register; if a name from before that rule is in the
    // way, the next free suffix is taken, so this never fails on a collision.
    result.username = sqlx::query_scalar(
        "WITH free AS (
             SELECT c.name FROM (
                 SELECT 'anonymized-' || replace($1::text, '-', '')
                        || CASE WHEN n = 0 THEN '' ELSE '-' || n END AS name, n
                 FROM generate_series(0, 10000) n) c
             WHERE NOT EXISTS (SELECT 1 FROM accounts o
                               WHERE o.username_normalized = c.name AND o.id <> $1)
             ORDER BY c.n LIMIT 1)
         UPDATE accounts a
         SET username = free.name, display_name = free.name,
             email = '', password_hash = '!', expertise = NULL,
             notify_email = false, notify_in_app = false, anonymized_at = now()
         FROM free WHERE a.id = $1 RETURNING a.username",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if body.purge_ticket_content {
        // The database verifies, per ticket, that this account is anonymized
        // and submitted it, and audits each removal.
        let tickets: Vec<Uuid> = sqlx::query_scalar(
            "SELECT t.id FROM tickets t
             WHERE t.requester_account_id = $1 AND t.status IN ('closed', 'cancelled', 'ai_solved')
             ORDER BY t.id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        let by = retention::Authority::Account(id);
        for ticket in tickets {
            let (removed, counts, _) = retention::purge_content(&mut tx, ticket, by).await?;
            files.extend(removed);
            result.ticket_content.screenshots += counts.screenshots;
            result.ticket_content.ai_content += counts.ai_content;
            result.ticket_content.ticket_content += counts.ticket_content;
        }
        // A ticket someone else holds right now was skipped: nothing is
        // committed and the Admin tries again.
        let left: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tickets t
                 WHERE t.requester_account_id = $1
                   AND t.status IN ('closed', 'cancelled', 'ai_solved')
                   AND (t.content_purged_at IS NULL
                        OR EXISTS (SELECT 1 FROM screenshots s WHERE s.ticket_id = t.id)))",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if left {
            return Err(ApiError::Conflict);
        }
    }
    result.audit_events =
        audit::anonymize_account(&mut tx, id, Who::Account(user.account_id())).await?;
    audit::record(
        &mut *tx,
        Entry {
            action: "account_anonymized",
            who: Who::Account(user.account_id()),
            subject: Some(id),
            ticket: None,
            metadata: json!({
                "sessions": result.sessions,
                "drafts": result.drafts,
                "pending_screenshots": result.pending_screenshots,
                "saved_views": result.saved_views,
                "notifications": result.notifications,
                "audit_events": result.audit_events,
                "purge_ticket_content": body.purge_ticket_content,
                "ticket_content": result.ticket_content,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    crate::screenshots::remove_files(&state.pool, &state.screenshot_dir, &files).await;
    Ok(Json(result))
}
