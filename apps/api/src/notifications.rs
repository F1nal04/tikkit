//! Durable ticket notifications: email through #28's [`mail::deliver`] and an
//! in-app inbox. See docs/notifications.md.
//!
//! Outbox rows are written by a database trigger in the transaction of the
//! `ticket_events` insert (migration 0018), so a rollback sends nothing and
//! every event writer is covered. They name an event and an account only. The
//! worker resolves the recipient, their language, their preferences, and their
//! **current** access with the ticket policy at delivery time; text never
//! copies ticket subjects, replies, notes, or images.

use std::{future::Future, time::Duration};

use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, Cursor, Json, Page, Pagination, Path, Query, TicketStatus},
    auth::{CurrentUser, Settings, audit, ident, ident_str},
    authz::{self, Actor, Permission, TicketScope, TicketView, require_permission},
    mail::{self, DeliverOptions, MailError, OutboundMessage},
};

/// Tries per outbox row before it is `failed` (until an Admin retries it).
pub const MAX_ATTEMPTS: i32 = 8;
const POLL: Duration = Duration::from_secs(2);

/// Runs `step` until it reports no more work, then sleeps. Shared by background
/// processors (the notification outbox now, confirmation timeouts later);
/// `step` returns whether it did something.
pub async fn poll_loop<F, Fut>(name: &'static str, mut step: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, ApiError>>,
{
    loop {
        match step().await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(_) => tracing::error!(worker = name, "background step failed; retrying"),
        }
        tokio::time::sleep(POLL).await;
    }
}

pub async fn worker_loop(pool: PgPool, settings: Settings) {
    poll_loop("notifications", || process_one(&pool, &settings)).await
}

#[derive(sqlx::FromRow)]
struct Job {
    id: Uuid,
    event_id: i64,
    account_id: Uuid,
    audience: String,
    channel: String,
    attempts: i32,
}

#[derive(sqlx::FromRow)]
struct Event {
    ticket_id: Uuid,
    event_type: String,
    payload: serde_json::Value,
    requester_account_id: Uuid,
    team_id: Option<Uuid>,
    assignee_account_id: Option<Uuid>,
    status: String,
    reference: i64,
}

struct Text {
    title: String,
    body: String,
    href: String,
}

/// System text in the account's language (English fallback). Only the ticket
/// reference, the username, the proposal deadline, and the requester's
/// cancellation reason are included.
fn ticket_text(event: &Event, language: &str, username: &str, audience: &str) -> Text {
    let de = language == "de";
    let title = match (event.event_type.as_str(), de) {
        ("submit", false) => "Ticket submitted",
        ("submit", true) => "Ticket eingereicht",
        ("assignment_changed", false) => "Ticket assignment changed",
        ("assignment_changed", true) => "Ticketzuweisung geändert",
        ("public_reply_added", false) => "New public reply",
        ("public_reply_added", true) => "Neue öffentliche Antwort",
        ("request_information", false) => "More information requested",
        ("request_information", true) => "Weitere Informationen angefordert",
        ("propose_resolution", false) => "Resolution proposed: confirm or reject it in the portal",
        ("propose_resolution", true) => "Lösung vorgeschlagen: im Portal bestätigen oder ablehnen",
        ("accept_resolution", false) => "Ticket closed: you confirmed the resolution",
        ("accept_resolution", true) => "Ticket geschlossen: Sie haben die Lösung bestätigt",
        ("confirmation_timeout", false) => {
            "Ticket closed automatically: confirmation period expired, not acceptance"
        }
        ("confirmation_timeout", true) => {
            "Ticket automatisch geschlossen: Bestätigungsfrist abgelaufen, keine Zustimmung"
        }
        ("reject_resolution", false) => "Resolution rejected: the requester still needs help",
        ("reject_resolution", true) => "Lösung abgelehnt: der Anfragende benötigt weiter Hilfe",
        ("cancel", false) => "Ticket cancelled",
        ("cancel", true) => "Ticket storniert",
        ("ai_solved", false) => "Receipt: you confirmed the AI solution",
        ("ai_solved", true) => "Bestätigung: Sie haben die KI-Lösung angenommen",
        ("reopen", false) => "Ticket reopened",
        ("reopen", true) => "Ticket wieder geöffnet",
        // ai_failed, skip_ai, ai_not_solved: the ticket entered the shared queue.
        (_, false) => "New ticket in the shared queue",
        (_, true) => "Neues Ticket in der gemeinsamen Warteschlange",
    };
    let reference = event.reference;
    let mut body = if de {
        format!(
            "Für Konto {username}. Ticket #{reference}. Tickets werden ausschließlich im Portal bearbeitet und beantwortet; Antworten per E-Mail werden nicht verarbeitet."
        )
    } else {
        format!(
            "For account {username}. Ticket #{reference}. Tickets are handled and replies are posted in the portal only; replies by email are not processed."
        )
    };
    let payload = |key: &str| event.payload.get(key).and_then(|v| v.as_str());
    if event.event_type == "propose_resolution"
        && let Some(deadline) = payload("deadline")
    {
        body.push_str(&if de {
            format!(" Ohne Entscheidung wird das Ticket am {deadline} (UTC) automatisch geschlossen; Schweigen gilt nicht als Zustimmung.")
        } else {
            format!(" Without a decision the ticket closes automatically at {deadline} (UTC); silence is not acceptance.")
        });
    }
    if event.event_type == "cancel"
        && let Some(reason) = payload("reason")
    {
        body.push_str(&if de {
            format!(" Grund: {reason}")
        } else {
            format!(" Reason: {reason}")
        });
    }
    let area = if audience == "requester" {
        "tickets"
    } else {
        "queue"
    };
    Text {
        title: title.into(),
        body,
        href: format!("/{area}/{}", event.ticket_id),
    }
}

/// Whether `actor` may be told about `event` now. The ticket policy decides;
/// AI-solved records only ever reach their requester's own confirmation receipt.
fn permitted(event: &Event, actor: &Actor, audience: &str) -> Result<bool, ApiError> {
    let status: TicketStatus = ident(event.status.clone())?;
    // Notes are staff-only content: no notification exists for them, whoever asks.
    if event.event_type == "internal_note_added" {
        return Ok(false);
    }
    if status == TicketStatus::AiSolved {
        return Ok(audience == "requester"
            && event.event_type == "ai_solved"
            && actor.id == event.requester_account_id);
    }
    let scope = TicketScope {
        requester_account_id: event.requester_account_id,
        team_id: event.team_id,
        assignee_account_id: event.assignee_account_id,
        status,
    };
    Ok(matches!(
        (audience, authz::ticket_view(actor, &scope)),
        ("requester", Some(TicketView::Requester)) | ("staff", Some(TicketView::Staff))
    ))
}

/// What the worker decided for one job.
enum Outcome {
    Sent,
    Suppressed,
    /// Retry later with this stable code.
    Failed(&'static str),
}

/// One bounded unit of work: claim a due row, deliver it, record the result.
/// Returns whether a row was handled. The row lock is held until the result is
/// committed, so concurrent workers skip it and a row is never sent twice; if
/// the process dies after SMTP accepted a message, the idempotency key makes
/// the retry a no-op that is then recorded as sent.
pub async fn process_one(pool: &PgPool, settings: &Settings) -> Result<bool, ApiError> {
    let mut tx = pool.begin().await?;
    let job: Option<Job> = sqlx::query_as(
        "SELECT id, event_id, account_id, audience, channel, attempts FROM notification_outbox
         WHERE status = 'pending' AND available_at <= clock_timestamp()
         ORDER BY available_at, id LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(job) = job else { return Ok(false) };
    let outcome = match deliver(pool, &mut tx, &job, settings).await {
        Ok(outcome) => outcome,
        Err(error) => {
            // The transaction may be aborted; record the attempt outside it so a
            // poison row cannot be retried forever.
            let _ = tx.rollback().await;
            tracing::error!(notification_id = %job.id, "notification processing failed");
            record(pool, &job, &Outcome::Failed("internal")).await?;
            return Err(error);
        }
    };
    if let Outcome::Failed(code) = outcome {
        tracing::warn!(notification_id = %job.id, error = code, "notification delivery failed");
    }
    record(&mut *tx, &job, &outcome).await?;
    tx.commit().await?;
    Ok(true)
}

const UPDATE_OUTCOME: &str = "UPDATE notification_outbox SET status = $2, attempts = attempts + 1,
    last_error = $3, available_at = clock_timestamp() + make_interval(secs => $4),
    completed_at = CASE WHEN $2 = 'pending' THEN NULL ELSE clock_timestamp() END WHERE id = $1";

fn status_of(job: &Job, outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Sent => "sent",
        Outcome::Suppressed => "suppressed",
        Outcome::Failed(_) if job.attempts + 1 >= MAX_ATTEMPTS => "failed",
        Outcome::Failed(_) => "pending",
    }
}

/// 30 s, doubling per attempt, at most one hour.
fn backoff(attempts: i32) -> f64 {
    f64::from((30_i32 << attempts.clamp(0, 7)).min(3600))
}

async fn record<'c>(db: impl PgExecutor<'c>, job: &Job, outcome: &Outcome) -> Result<(), ApiError> {
    sqlx::query(UPDATE_OUTCOME)
        .bind(job.id)
        .bind(status_of(job, outcome))
        .bind(match outcome {
            Outcome::Failed(code) => Some(*code),
            _ => None,
        })
        .bind(backoff(job.attempts))
        .execute(db)
        .await?;
    Ok(())
}

async fn deliver(
    pool: &PgPool,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job: &Job,
    settings: &Settings,
) -> Result<Outcome, ApiError> {
    // Plain reads, no row locks: this transaction stays open while SMTP talks
    // (up to `mail::SMTP_TIMEOUT`), and a lock would stall portal writes on the
    // ticket, the account, or its team. Like a detail read, this is the access
    // as of now.
    let account: Option<(String, String, String, String, bool)> = sqlx::query_as(
        "SELECT role::text, email, username, language,
                CASE WHEN $2 = 'email' THEN notify_email ELSE notify_in_app END
         FROM accounts WHERE id = $1 AND is_active",
    )
    .bind(job.account_id)
    .bind(&job.channel)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((role, email, username, language, wanted)) = account else {
        return Ok(Outcome::Suppressed); // deactivated
    };
    let actor = Actor {
        id: job.account_id,
        role: ident(role)?,
        team: authz::team_of(&mut **tx, job.account_id).await?,
    };
    let event: Event = sqlx::query_as(
        "SELECT e.ticket_id, e.event_type, e.payload, t.requester_account_id, t.team_id,
                t.assignee_account_id, t.status::text AS status, t.reference
         FROM ticket_events e JOIN tickets t ON t.id = e.ticket_id
         WHERE e.id = $1",
    )
    .bind(job.event_id)
    .fetch_one(&mut **tx)
    .await?;
    if !permitted(&event, &actor, &job.audience)? || !wanted {
        return Ok(Outcome::Suppressed);
    }
    if job.channel == "in_app" {
        sqlx::query(
            "INSERT INTO notifications (id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(job.id)
        .bind(job.account_id)
        .execute(&mut **tx)
        .await?;
        return Ok(Outcome::Sent);
    }
    let text = ticket_text(&event, &language, &username, &job.audience);
    let message = OutboundMessage {
        to: email,
        subject: text.title,
        body: format!("{}\n\n{}{}", text.body, settings.public_origin, text.href),
        idempotency_key: Some(format!("notification:{}", job.id)),
    };
    // One SMTP try per row; the outbox owns the retry schedule.
    let options = DeliverOptions {
        max_attempts: 1,
        ..DeliverOptions::default()
    };
    let sent = mail::deliver(
        pool,
        &settings.encryption_key,
        settings.smtp_trust_root_pem.as_deref(),
        &message,
        options,
    )
    .await;
    Ok(match sent {
        Ok(delivered) => {
            if delivered == mail::Delivered::Sent {
                let _ = mail::clear_delivery_error(pool).await;
            }
            Outcome::Sent
        }
        Err(error) => {
            if !matches!(
                error,
                MailError::NotConfigured | MailError::Disabled | MailError::Store
            ) {
                let _ = mail::note_delivery_error(pool, error.code()).await;
            }
            Outcome::Failed(error.code())
        }
    })
}

// ---- Preferences -----------------------------------------------------------

#[derive(Serialize, Deserialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct NotificationPreferences {
    pub email: bool,
    pub in_app: bool,
}

#[utoipa::path(get, path = "/api/notification-preferences", tag = "notifications",
    responses((status = 200, body = NotificationPreferences),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope)))]
pub async fn notification_preferences(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<NotificationPreferences>, ApiError> {
    Ok(Json(
        sqlx::query_as(
            "SELECT notify_email AS email, notify_in_app AS in_app FROM accounts WHERE id = $1",
        )
        .bind(user.account_id())
        .fetch_one(&state.pool)
        .await?,
    ))
}

#[utoipa::path(put, path = "/api/notification-preferences", tag = "notifications", request_body = NotificationPreferences,
    responses((status = 200, body = NotificationPreferences),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope)))]
pub async fn update_notification_preferences(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NotificationPreferences>,
) -> Result<Json<NotificationPreferences>, ApiError> {
    sqlx::query("UPDATE accounts SET notify_email = $2, notify_in_app = $3 WHERE id = $1")
        .bind(user.account_id())
        .bind(body.email)
        .bind(body.in_app)
        .execute(&state.pool)
        .await?;
    Ok(Json(body))
}

// ---- In-app inbox ----------------------------------------------------------

#[derive(Serialize, ToSchema)]
pub struct Notification {
    pub id: Uuid,
    pub ticket_id: Uuid,
    /// System text in the caller's language.
    pub title: String,
    pub body: String,
    /// Portal path of the ticket; opening it still requires current access.
    pub href: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub read_at: Option<OffsetDateTime>,
}

#[derive(Serialize, ToSchema)]
pub struct UnreadCount {
    pub count: i64,
}

#[derive(sqlx::FromRow)]
struct InboxRow {
    id: Uuid,
    created_at: OffsetDateTime,
    read_at: Option<OffsetDateTime>,
    audience: String,
    #[sqlx(flatten)]
    event: Event,
}

/// The delivery policy again, in SQL over `notifications n`, its outbox row `o`,
/// event `e`, and ticket `t`; `$1..$3` are [`Actor::visibility_binds`]. A
/// notification stops being listed the moment the caller loses access.
fn visible_sql() -> String {
    format!(
        "n.account_id = $1 AND e.event_type <> 'internal_note_added' AND (
           (o.audience = 'requester' AND t.requester_account_id = $1
            AND (t.status <> 'ai_solved' OR e.event_type = 'ai_solved'))
        OR (o.audience = 'staff' AND t.status <> 'ai_solved' AND {}))",
        authz::staff_visible_sql(1)
    )
}

const FROM: &str = "FROM notifications n JOIN notification_outbox o ON o.id = n.id
    JOIN ticket_events e ON e.id = o.event_id JOIN tickets t ON t.id = e.ticket_id";

#[utoipa::path(get, path = "/api/notifications", tag = "notifications", params(Pagination),
    responses((status = 200, body = Page<Notification>),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope)))]
pub async fn list_notifications(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(page): Query<Pagination>,
) -> Result<Json<Page<Notification>>, ApiError> {
    let cursor = page.cursor()?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let (id, role, team) = Actor::load(&mut *tx, &user).await?.visibility_binds();
    let rows: Vec<InboxRow> = sqlx::query_as(&format!(
        "SELECT n.id, n.created_at, n.read_at, o.audience, e.ticket_id, e.event_type, e.payload,
                t.requester_account_id, t.team_id, t.assignee_account_id, t.status::text AS status, t.reference
         {FROM} WHERE {} AND ($4::timestamptz IS NULL OR (n.created_at, n.id) < ($4, $5))
         ORDER BY n.created_at DESC, n.id DESC LIMIT $6",
        visible_sql()
    ))
    .bind(id)
    .bind(role)
    .bind(team)
    .bind(cursor.map(|c| c.created_at))
    .bind(cursor.map(|c| c.id))
    .bind(page.limit() + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let language = ident_str(user.language());
    let items = rows
        .into_iter()
        .map(|row| {
            let text = ticket_text(&row.event, &language, &user.account.username, &row.audience);
            Notification {
                id: row.id,
                ticket_id: row.event.ticket_id,
                title: text.title,
                body: text.body,
                href: text.href,
                created_at: row.created_at,
                read_at: row.read_at,
            }
        })
        .collect();
    Ok(Json(Page::from_rows(items, page.limit(), |n| Cursor {
        created_at: n.created_at,
        id: n.id,
    })))
}

#[utoipa::path(get, path = "/api/notifications/unread", tag = "notifications",
    responses((status = 200, body = UnreadCount),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope)))]
pub async fn unread_count(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<UnreadCount>, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let (id, role, team) = Actor::load(&mut *tx, &user).await?.visibility_binds();
    let count = sqlx::query_scalar(&format!(
        "SELECT count(*) {FROM} WHERE n.read_at IS NULL AND {}",
        visible_sql()
    ))
    .bind(id)
    .bind(role)
    .bind(team)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(UnreadCount { count }))
}

/// Marks one of the caller's visible notifications read. Idempotent; anything
/// else (another account's, or one whose ticket is no longer visible) is `404`.
#[utoipa::path(post, path = "/api/notifications/{id}/read", tag = "notifications", params(("id" = Uuid, Path)),
    responses((status = 204),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope)))]
pub async fn mark_notification_read(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let (account, role, team) = Actor::load(&mut *tx, &user).await?.visibility_binds();
    let visible: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 {FROM} WHERE n.id = $4 AND {})",
        visible_sql()
    ))
    .bind(account)
    .bind(role)
    .bind(team)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !visible {
        return Err(ApiError::NotFound);
    }
    sqlx::query(
        "UPDATE notifications SET read_at = COALESCE(read_at, clock_timestamp()) WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- Admin observability ---------------------------------------------------

#[derive(Serialize, ToSchema)]
pub struct DeliveryFailure {
    /// Stable code, e.g. `smtp_auth_failed`.
    pub error: String,
    pub count: i64,
}

#[derive(Serialize, ToSchema)]
pub struct DeliveryStats {
    pub pending: i64,
    pub sent: i64,
    pub suppressed: i64,
    pub failed: i64,
    pub failures: Vec<DeliveryFailure>,
}

#[utoipa::path(get, path = "/api/admin/notifications", tag = "notifications",
    responses((status = 200, body = DeliveryStats),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope)))]
pub async fn delivery_stats(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<DeliveryStats>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let counts: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*) FROM notification_outbox GROUP BY status")
            .fetch_all(&state.pool)
            .await?;
    let count = |status: &str| {
        counts
            .iter()
            .find(|(s, _)| s == status)
            .map_or(0, |(_, n)| *n)
    };
    let failures: Vec<(String, i64)> = sqlx::query_as(
        "SELECT COALESCE(last_error, 'unknown'), count(*) FROM notification_outbox
         WHERE status = 'failed' GROUP BY 1 ORDER BY 2 DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(DeliveryStats {
        pending: count("pending"),
        sent: count("sent"),
        suppressed: count("suppressed"),
        failed: count("failed"),
        failures: failures
            .into_iter()
            .map(|(error, count)| DeliveryFailure { error, count })
            .collect(),
    }))
}

#[derive(Serialize, ToSchema)]
pub struct RetryResult {
    pub retried: u64,
}

/// Re-queues every `failed` row. Rows keep their idempotency key, so a message
/// the relay already accepted is never sent twice.
#[utoipa::path(post, path = "/api/admin/notifications/retry", tag = "notifications",
    responses((status = 200, body = RetryResult),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope)))]
pub async fn retry_failed(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<RetryResult>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let mut tx = state.pool.begin().await?;
    let retried = sqlx::query(
        "UPDATE notification_outbox SET status = 'pending', attempts = 0, last_error = NULL,
         completed_at = NULL, available_at = clock_timestamp() WHERE status = 'failed'",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    audit(
        &mut *tx,
        "notifications_retried",
        Some(user.account_id()),
        None,
        serde_json::json!({ "count": retried }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(RetryResult { retried }))
}
