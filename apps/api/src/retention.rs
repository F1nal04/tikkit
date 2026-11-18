//! Data retention (#43): the Admin policy, its preview, and the restart-safe
//! runs that apply it. See docs/retention.md.
//!
//! - Every period is `None` ("keep") until an Admin sets it; nothing is deleted
//!   before that.
//! - [`preview`] and [`process`] share the predicates below, so the counts an
//!   Admin sees are the counts a run started at the same instant removes.
//! - A run snapshots the policy and its clock (`as_of`). [`process`] does one
//!   bounded batch per transaction while holding the run's row lock and adds to
//!   the run's counts in that transaction: a crash loses a batch and its counts
//!   together, and a second worker skips the locked run.
//! - Ticket content goes only through the SQL functions of migration `0028`
//!   (`retention_delete_screenshots`, `retention_purge_ai`,
//!   `retention_purge_ticket`); audit events only through
//!   [`audit::purge_before`]. Screenshot files are removed after the commit; a
//!   crash in between leaves a file without a row, which
//!   [`crate::screenshots::cleanup`] removes.

use std::path::Path as FsPath;

use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, PgConnection, PgPool};
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json},
    audit::{self, Entry, Who},
    auth::CurrentUser,
    authz::{Permission, require_permission},
    metrics::{Metrics, Worker},
    notifications::poll_loop,
    telemetry,
};

/// Shortest and longest period, in days. Audit metadata has the higher floor
/// [`audit::RETENTION_FLOOR_DAYS`].
pub const MIN_DAYS: i64 = 1;
pub const MAX_DAYS: i64 = 3650;
/// What an Admin types to confirm an irreversible deletion. Not translated.
pub const CONFIRM: &str = "DELETE";
/// What replaces removed text (`retention_marker()` in SQL).
pub const REMOVED: &str = "—";
/// Tickets or drafts per transaction.
pub const BATCH: i64 = 100;
/// Runs listed on the Admin page.
const RUNS_SHOWN: i64 = 10;

/// `tickets t` in a final status: the only tickets retention touches.
const FINAL: &str = "t.status IN ('closed', 'cancelled', 'ai_solved')";

fn nullable_days<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Option::deserialize(d)
}

/// Retention periods in days. `null` keeps that category for good, which is
/// the default for all five.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromRow)]
#[serde(deny_unknown_fields)]
pub struct RetentionPolicy {
    /// Unsubmitted drafts, from their last save.
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub draft_days: Option<i64>,
    /// Screenshots of tickets in a final status, from the final status.
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub screenshot_days: Option<i64>,
    /// AI input and suggestion of tickets in a final status.
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub ai_content_days: Option<i64>,
    /// Subject, description, answers, messages, and cancellation reason of
    /// tickets in a final status. Implies their screenshots and AI content.
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub ticket_content_days: Option<i64>,
    /// Audit events. At least 30 days.
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub audit_days: Option<i64>,
}

impl RetentionPolicy {
    fn fields(&self) -> [(&'static str, Option<i64>, i64); 5] {
        [
            ("draft_days", self.draft_days, MIN_DAYS),
            ("screenshot_days", self.screenshot_days, MIN_DAYS),
            ("ai_content_days", self.ai_content_days, MIN_DAYS),
            ("ticket_content_days", self.ticket_content_days, MIN_DAYS),
            ("audit_days", self.audit_days, audit::RETENTION_FLOOR_DAYS),
        ]
    }

    /// The bounds of the `retention_settings` constraints.
    pub fn validate(&self) -> FieldErrors {
        let mut errors = FieldErrors::new();
        for (field, value, min) in self.fields() {
            if value.is_some_and(|v| !(min..=MAX_DAYS).contains(&v)) {
                errors.insert(field.into(), vec!["out_of_range"]);
            }
        }
        errors
    }

    pub fn is_empty(&self) -> bool {
        self.fields().iter().all(|(_, value, _)| value.is_none())
    }

    /// Everything strictly older than these instants has expired. Removing a
    /// ticket's content removes its screenshots and AI content too, so those
    /// two use the shorter of their own period and the ticket content period.
    fn cutoffs(&self, as_of: OffsetDateTime) -> Cutoffs {
        let at = |days: Option<i64>| days.map(|d| as_of - Duration::days(d));
        let shorter = |own: Option<i64>| match (own, self.ticket_content_days) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Cutoffs {
            drafts: at(self.draft_days),
            screenshots: at(shorter(self.screenshot_days)),
            ai_content: at(shorter(self.ai_content_days)),
            ticket_content: at(self.ticket_content_days),
            audit: at(self.audit_days),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Cutoffs {
    drafts: Option<OffsetDateTime>,
    screenshots: Option<OffsetDateTime>,
    ai_content: Option<OffsetDateTime>,
    ticket_content: Option<OffsetDateTime>,
    audit: Option<OffsetDateTime>,
}

/// Records per category. In a preview: what a run started now would remove.
/// In a run: what it has removed so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct RetentionCounts {
    /// Drafts deleted.
    #[serde(default)]
    #[schema(required = true)]
    pub drafts: i64,
    /// Screenshot records deleted (their files follow).
    #[serde(default)]
    #[schema(required = true)]
    pub screenshots: i64,
    /// AI attempts whose input and suggestion were removed.
    #[serde(default)]
    #[schema(required = true)]
    pub ai_content: i64,
    /// Tickets whose text was removed.
    #[serde(default)]
    #[schema(required = true)]
    pub ticket_content: i64,
    /// Audit events deleted.
    #[serde(default)]
    #[schema(required = true)]
    pub audit_events: i64,
}

impl RetentionCounts {
    pub fn total(&self) -> i64 {
        self.drafts + self.screenshots + self.ai_content + self.ticket_content + self.audit_events
    }
}

/// What `policy` would remove at `as_of`. Reads only.
pub async fn preview(
    db: &mut PgConnection,
    policy: &RetentionPolicy,
    as_of: OffsetDateTime,
) -> Result<RetentionCounts, sqlx::Error> {
    let c = policy.cutoffs(as_of);
    sqlx::query_as(&format!(
        "SELECT
           (SELECT count(*) FROM ticket_drafts d
             WHERE d.submitted_ticket_id IS NULL AND d.updated_at < $1) AS drafts,
           (SELECT count(*) FROM screenshots s JOIN tickets t ON t.id = s.ticket_id
             WHERE {FINAL} AND ticket_final_at(t) < $2) AS screenshots,
           (SELECT count(*) FROM ai_attempts a JOIN tickets t ON t.id = a.ticket_id
             WHERE {FINAL} AND a.state <> 'processing' AND a.content_purged_at IS NULL
               AND ticket_final_at(t) < $3) AS ai_content,
           (SELECT count(*) FROM tickets t
             WHERE {FINAL} AND t.content_purged_at IS NULL AND ticket_final_at(t) < $4) AS ticket_content,
           (SELECT count(*) FROM audit_events
             WHERE created_at < $5 AND event_type <> 'audit_retention_purged') AS audit_events"
    ))
    .bind(c.drafts)
    .bind(c.screenshots)
    .bind(c.ai_content)
    .bind(c.ticket_content)
    .bind(c.audit)
    .fetch_one(db)
    .await
}

// ----------------------------------------------------------------------- runs

/// A claimed run: its row is locked by the transaction that read it.
#[derive(Clone, Debug, FromRow)]
pub struct Run {
    pub id: Uuid,
    pub requested_by: Option<Uuid>,
    pub request_id: Option<Uuid>,
    pub as_of: OffsetDateTime,
    #[sqlx(flatten)]
    pub policy: RetentionPolicy,
}

/// What one [`process`] call did, to act on after its transaction commits.
#[derive(Debug, Default)]
pub struct Advance {
    /// The run has nothing left and is recorded as completed.
    pub completed: bool,
    /// Deleted screenshot records whose files can go now.
    pub files: Vec<Uuid>,
}

const POLICY_COLUMNS: &str = "draft_days::bigint, screenshot_days::bigint, ai_content_days::bigint,
    ticket_content_days::bigint, audit_days::bigint";

/// Locks the pending run for this transaction, unless another worker has it.
pub async fn claim(db: &mut PgConnection) -> Result<Option<Run>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT id, requested_by, request_id, as_of, {POLICY_COLUMNS}
         FROM retention_runs WHERE state = 'pending'
         ORDER BY created_at, id LIMIT 1 FOR UPDATE SKIP LOCKED"
    ))
    .fetch_optional(db)
    .await
}

async fn add(db: &mut PgConnection, run: Uuid, key: &str, n: i64) -> Result<(), sqlx::Error> {
    if n > 0 {
        sqlx::query(
            "UPDATE retention_runs
             SET counts = counts || jsonb_build_object($2::text, COALESCE((counts->>$2)::bigint, 0) + $3)
             WHERE id = $1",
        )
        .bind(run)
        .bind(key)
        .bind(n)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// Tickets in a final status since before `cutoff` that still match `pending`,
/// locked against a concurrent reopen. Rows another transaction holds are left
/// for the next batch or run.
async fn expired_tickets(
    db: &mut PgConnection,
    cutoff: OffsetDateTime,
    pending: &str,
    batch: i64,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT t.id FROM tickets t
         WHERE {FINAL} AND ticket_final_at(t) < $1 AND {pending}
         ORDER BY t.id LIMIT $2 FOR UPDATE OF t SKIP LOCKED"
    ))
    .bind(cutoff)
    .bind(batch)
    .fetch_all(db)
    .await
}

async fn delete_screenshots(
    db: &mut PgConnection,
    tickets: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT retention_delete_screenshots(id) FROM unnest($1::uuid[]) AS ticket (id)",
    )
    .bind(tickets)
    .fetch_all(db)
    .await
}

async fn purge_ai(db: &mut PgConnection, tickets: &[Uuid]) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FILTER (WHERE done) FROM (
             SELECT retention_purge_ai(id) AS done FROM unnest($1::uuid[]) AS ticket (id)) x",
    )
    .bind(tickets)
    .fetch_one(db)
    .await
}

async fn purge_tickets(db: &mut PgConnection, tickets: &[Uuid]) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FILTER (WHERE done) FROM (
             SELECT retention_purge_ticket(id) AS done FROM unnest($1::uuid[]) AS ticket (id)) x",
    )
    .bind(tickets)
    .fetch_one(db)
    .await
}

/// Removes all content of the given final-status tickets now: screenshots, AI
/// content, then text. Returns the deleted screenshot IDs and the counts.
/// Tickets not in a final status are left untouched by the SQL functions.
pub(crate) async fn purge_content(
    db: &mut PgConnection,
    tickets: &[Uuid],
) -> Result<(Vec<Uuid>, RetentionCounts), sqlx::Error> {
    let files = delete_screenshots(db, tickets).await?;
    let counts = RetentionCounts {
        screenshots: files.len() as i64,
        ai_content: purge_ai(db, tickets).await?,
        ticket_content: purge_tickets(db, tickets).await?,
        ..Default::default()
    };
    Ok((files, counts))
}

/// One bounded batch of a claimed run, in the caller's transaction. The first
/// category that still has expired records gets the batch. When none has, the
/// audit period is applied and the run is completed.
pub async fn process(db: &mut PgConnection, run: &Run, batch: i64) -> Result<Advance, ApiError> {
    let c = run.policy.cutoffs(run.as_of);
    if let Some(cutoff) = c.screenshots {
        let pending = "EXISTS (SELECT 1 FROM screenshots s WHERE s.ticket_id = t.id)";
        let tickets = expired_tickets(db, cutoff, pending, batch).await?;
        let files = delete_screenshots(db, &tickets).await?;
        if !files.is_empty() {
            add(db, run.id, "screenshots", files.len() as i64).await?;
            return Ok(Advance {
                completed: false,
                files,
            });
        }
    }
    if let Some(cutoff) = c.ai_content {
        let pending = "EXISTS (SELECT 1 FROM ai_attempts a WHERE a.ticket_id = t.id
                               AND a.state <> 'processing' AND a.content_purged_at IS NULL)";
        let tickets = expired_tickets(db, cutoff, pending, batch).await?;
        let done = purge_ai(db, &tickets).await?;
        if done > 0 {
            add(db, run.id, "ai_content", done).await?;
            return Ok(Advance::default());
        }
    }
    if let Some(cutoff) = c.ticket_content {
        let tickets = expired_tickets(db, cutoff, "t.content_purged_at IS NULL", batch).await?;
        // Screenshots and AI content went in the steps above; this covers a
        // ticket that was locked then.
        let (files, counts) = purge_content(db, &tickets).await?;
        if counts.total() > 0 {
            add(db, run.id, "screenshots", counts.screenshots).await?;
            add(db, run.id, "ai_content", counts.ai_content).await?;
            add(db, run.id, "ticket_content", counts.ticket_content).await?;
            return Ok(Advance {
                completed: false,
                files,
            });
        }
    }
    if let Some(cutoff) = c.drafts {
        let done = sqlx::query(
            "DELETE FROM ticket_drafts WHERE id IN (
                 SELECT id FROM ticket_drafts
                 WHERE submitted_ticket_id IS NULL AND updated_at < $1
                 ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED)",
        )
        .bind(cutoff)
        .bind(batch)
        .execute(&mut *db)
        .await?
        .rows_affected() as i64;
        if done > 0 {
            add(db, run.id, "drafts", done).await?;
            return Ok(Advance::default());
        }
    }
    if let Some(cutoff) = c.audit {
        // The Admin who started the run, while still an active Admin.
        let admin: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM accounts WHERE id = $1 AND role = 'admin' AND is_active",
        )
        .bind(run.requested_by)
        .fetch_optional(&mut *db)
        .await?;
        let who = admin.map_or(Who::Service(audit::RETENTION), Who::Account);
        let done = audit::purge_before(db, cutoff, who).await?;
        add(db, run.id, "audit_events", done).await?;
    }
    let counts: sqlx::types::Json<RetentionCounts> = sqlx::query_scalar(
        "UPDATE retention_runs SET state = 'completed', finished_at = clock_timestamp()
         WHERE id = $1 RETURNING counts",
    )
    .bind(run.id)
    .fetch_one(&mut *db)
    .await?;
    audit::record(
        &mut *db,
        Entry {
            action: "retention_run_completed",
            who: Who::Service(audit::RETENTION),
            subject: None,
            ticket: None,
            metadata: json!({
                "run_id": run.id,
                "requested_by": run.requested_by,
                "policy": run.policy,
                "deleted": counts.0,
            }),
        },
    )
    .await?;
    tracing::info!(
        run_id = %run.id,
        drafts = counts.0.drafts,
        screenshots = counts.0.screenshots,
        ai_content = counts.0.ai_content,
        ticket_content = counts.0.ticket_content,
        audit_events = counts.0.audit_events,
        "retention run completed"
    );
    Ok(Advance {
        completed: true,
        files: Vec::new(),
    })
}

/// Best effort: a file that stays behind has no row and is removed by the
/// stray-file sweep of [`crate::screenshots::cleanup`].
pub(crate) async fn remove_files(dir: &FsPath, ids: &[Uuid]) {
    for id in ids {
        let _ = tokio::fs::remove_file(crate::screenshots::file_path(dir, *id)).await;
    }
}

/// Inserts a run of `policy` unless one is pending. `None`: one is pending.
async fn insert_run(
    db: &mut PgConnection,
    policy: &RetentionPolicy,
    requested_by: Option<Uuid>,
    as_of: OffsetDateTime,
) -> Result<Option<Uuid>, sqlx::Error> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO retention_runs (requested_by, request_id, as_of, draft_days, screenshot_days,
                                     ai_content_days, ticket_content_days, audit_days)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(requested_by)
    .bind(telemetry::request_id())
    .bind(as_of)
    .bind(policy.draft_days.map(|d| d as i32))
    .bind(policy.screenshot_days.map(|d| d as i32))
    .bind(policy.ai_content_days.map(|d| d as i32))
    .bind(policy.ticket_content_days.map(|d| d as i32))
    .bind(policy.audit_days.map(|d| d as i32))
    .fetch_optional(&mut *db)
    .await?;
    if let Some(id) = id {
        audit::record(
            &mut *db,
            Entry {
                action: "retention_run_started",
                who: requested_by.map_or(Who::Service(audit::RETENTION), Who::Account),
                subject: None,
                ticket: None,
                metadata: json!({ "run_id": id, "policy": policy, "as_of": as_of.unix_timestamp() }),
            },
        )
        .await?;
    }
    Ok(id)
}

/// Starts a run of `policy` with the clock at `as_of`. `None` while another run
/// is pending. The HTTP handler and the daily schedule go through this; tests
/// use it to place the clock exactly on a boundary.
pub async fn start_run(
    pool: &PgPool,
    policy: &RetentionPolicy,
    requested_by: Option<Uuid>,
    as_of: OffsetDateTime,
) -> Result<Option<Uuid>, ApiError> {
    let mut tx = pool.begin().await?;
    let id = insert_run(&mut tx, policy, requested_by, as_of).await?;
    tx.commit().await?;
    Ok(id)
}

/// With "apply automatically" on: one run a day under the saved policy.
async fn schedule(pool: &PgPool) -> Result<bool, ApiError> {
    let mut tx = pool.begin().await?;
    let due: Option<RetentionPolicy> = sqlx::query_as(&format!(
        "SELECT {POLICY_COLUMNS} FROM retention_settings s
         WHERE s.automatic
           AND NOT EXISTS (SELECT 1 FROM retention_runs r WHERE r.state = 'pending')
           AND NOT EXISTS (SELECT 1 FROM retention_runs r WHERE r.requested_by IS NULL
                           AND r.created_at > clock_timestamp() - interval '24 hours')
         FOR UPDATE OF s"
    ))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(policy) = due.filter(|p| !p.is_empty()) else {
        return Ok(false);
    };
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    let started = insert_run(&mut tx, &policy, None, now).await?.is_some();
    tx.commit().await?;
    Ok(started)
}

/// One worker step: a batch of the pending run, or the daily schedule check.
/// Returns whether it did something. Safe to run from several API processes.
pub async fn step(
    pool: &PgPool,
    dir: &FsPath,
    metrics: &Metrics,
    batch: i64,
) -> Result<bool, ApiError> {
    let result = try_step(pool, dir, batch).await;
    metrics.step(Worker::Retention, result.is_ok());
    result
}

async fn try_step(pool: &PgPool, dir: &FsPath, batch: i64) -> Result<bool, ApiError> {
    let mut tx = pool.begin().await?;
    let Some(run) = claim(&mut tx).await? else {
        drop(tx);
        // A new job gets its own correlation ID.
        return telemetry::job(Worker::Retention.label(), None, schedule(pool)).await;
    };
    telemetry::job(Worker::Retention.label(), run.request_id, async move {
        let advance = process(&mut tx, &run, batch).await?;
        tx.commit().await?;
        remove_files(dir, &advance.files).await;
        Ok(true)
    })
    .await
}

/// Applies pending runs and the daily schedule until shutdown.
pub async fn worker_loop(state: AppState) {
    poll_loop(Worker::Retention.label(), || {
        let state = state.clone();
        async move { step(&state.pool, &state.screenshot_dir, &state.metrics, BATCH).await }
    })
    .await;
}

// ------------------------------------------------------------------ Admin API

/// Bounds of the retention periods, in days.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionLimits {
    pub min_days: i64,
    pub max_days: i64,
    /// The audit log's retention floor.
    pub audit_min_days: i64,
}

#[derive(Clone, Debug, Serialize, ToSchema, FromRow)]
pub struct RetentionSettings {
    #[sqlx(flatten)]
    pub policy: RetentionPolicy,
    /// Whether the worker applies the policy daily. Off by default.
    pub automatic: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetentionRunState {
    Pending,
    Completed,
}

/// One application of the policy.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionRun {
    pub id: Uuid,
    pub state: RetentionRunState,
    /// The Admin who started it; `null` for an automatic run.
    #[schema(required = true)]
    pub requested_by: Option<Uuid>,
    #[schema(required = true)]
    pub requested_by_username: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    /// The clock the periods were measured from.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub as_of: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub finished_at: Option<OffsetDateTime>,
    /// The periods the run applies, as they were when it started.
    pub policy: RetentionPolicy,
    /// Removed so far (final once `completed`).
    pub deleted: RetentionCounts,
}

#[derive(FromRow)]
struct RunRow {
    id: Uuid,
    state: String,
    requested_by: Option<Uuid>,
    requested_by_username: Option<String>,
    created_at: OffsetDateTime,
    as_of: OffsetDateTime,
    finished_at: Option<OffsetDateTime>,
    #[sqlx(flatten)]
    policy: RetentionPolicy,
    counts: sqlx::types::Json<RetentionCounts>,
}

impl From<RunRow> for RetentionRun {
    fn from(r: RunRow) -> Self {
        Self {
            id: r.id,
            state: if r.state == "completed" {
                RetentionRunState::Completed
            } else {
                RetentionRunState::Pending
            },
            requested_by: r.requested_by,
            requested_by_username: r.requested_by_username,
            created_at: r.created_at,
            as_of: r.as_of,
            finished_at: r.finished_at,
            policy: r.policy,
            deleted: r.counts.0,
        }
    }
}

async fn runs(
    db: &mut PgConnection,
    filter: &str,
    id: Option<Uuid>,
) -> Result<Vec<RetentionRun>, sqlx::Error> {
    let rows: Vec<RunRow> = sqlx::query_as(&format!(
        "SELECT r.id, r.state, r.requested_by, a.username AS requested_by_username, r.created_at,
                r.as_of, r.finished_at, r.draft_days::bigint, r.screenshot_days::bigint,
                r.ai_content_days::bigint, r.ticket_content_days::bigint, r.audit_days::bigint,
                r.counts
         FROM retention_runs r LEFT JOIN accounts a ON a.id = r.requested_by
         {filter} ORDER BY r.created_at DESC, r.id DESC LIMIT $2"
    ))
    .bind(id)
    .bind(RUNS_SHOWN)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

async fn load_settings(
    db: &mut PgConnection,
    lock: bool,
) -> Result<RetentionSettings, sqlx::Error> {
    let lock = if lock { "FOR UPDATE" } else { "" };
    sqlx::query_as(&format!(
        "SELECT {POLICY_COLUMNS}, automatic, updated_at FROM retention_settings {lock}"
    ))
    .fetch_one(db)
    .await
}

/// The retention policy, its bounds, and the latest runs.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionOverview {
    pub settings: RetentionSettings,
    pub limits: RetentionLimits,
    /// Newest first, at most 10.
    pub runs: Vec<RetentionRun>,
}

/// The retention policy and the latest runs (Admin only).
#[utoipa::path(get, path = "/api/admin/retention", tag = "admin",
    responses(
        (status = 200, body = RetentionOverview),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn get_retention(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<RetentionOverview>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let mut db = state.pool.acquire().await?;
    Ok(Json(RetentionOverview {
        settings: load_settings(&mut db, false).await?,
        limits: RetentionLimits {
            min_days: MIN_DAYS,
            max_days: MAX_DAYS,
            audit_min_days: audit::RETENTION_FLOOR_DAYS,
        },
        runs: runs(&mut db, "WHERE $1::uuid IS NULL", None).await?,
    }))
}

/// Every period is required; `null` keeps that category for good.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetentionSettingsWrite {
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub draft_days: Option<i64>,
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub screenshot_days: Option<i64>,
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub ai_content_days: Option<i64>,
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub ticket_content_days: Option<i64>,
    #[schema(required = true)]
    #[serde(deserialize_with = "nullable_days")]
    pub audit_days: Option<i64>,
    pub automatic: bool,
    /// Must be `DELETE` when `automatic` is true and any period is set: saving
    /// then authorizes deletion without a further confirmation.
    #[serde(default)]
    pub confirm: Option<String>,
}

impl RetentionSettingsWrite {
    fn policy(&self) -> RetentionPolicy {
        RetentionPolicy {
            draft_days: self.draft_days,
            screenshot_days: self.screenshot_days,
            ai_content_days: self.ai_content_days,
            ticket_content_days: self.ticket_content_days,
            audit_days: self.audit_days,
        }
    }
}

fn confirmed(confirm: Option<&str>) -> bool {
    confirm == Some(CONFIRM)
}

/// Replaces the retention policy (Admin only). Saving deletes nothing unless
/// `automatic` is on; then the worker applies the policy within a day.
#[utoipa::path(put, path = "/api/admin/retention", tag = "admin", request_body = RetentionSettingsWrite,
    responses(
        (status = 200, body = RetentionSettings),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (a period: `out_of_range`; `confirm`: `invalid`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn put_retention(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<RetentionSettingsWrite>,
) -> Result<Json<RetentionSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let policy = body.policy();
    let mut errors = policy.validate();
    if body.automatic && !policy.is_empty() && !confirmed(body.confirm.as_deref()) {
        errors.insert("confirm".into(), vec!["invalid"]);
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let mut tx = state.pool.begin().await?;
    let old = load_settings(&mut tx, true).await?;
    sqlx::query(
        "UPDATE retention_settings SET draft_days = $1, screenshot_days = $2, ai_content_days = $3,
                ticket_content_days = $4, audit_days = $5, automatic = $6,
                updated_at = now(), updated_by = $7",
    )
    .bind(policy.draft_days.map(|d| d as i32))
    .bind(policy.screenshot_days.map(|d| d as i32))
    .bind(policy.ai_content_days.map(|d| d as i32))
    .bind(policy.ticket_content_days.map(|d| d as i32))
    .bind(policy.audit_days.map(|d| d as i32))
    .bind(body.automatic)
    .bind(user.account_id())
    .execute(&mut *tx)
    .await?;
    let new = load_settings(&mut tx, false).await?;
    let plain = |s: &RetentionSettings| json!({ "policy": s.policy, "automatic": s.automatic });
    audit::record(
        &mut *tx,
        Entry {
            action: "retention_settings_updated",
            who: Who::Account(user.account_id()),
            subject: None,
            ticket: None,
            metadata: json!({ "from": plain(&old), "to": plain(&new) }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(new))
}

/// What a run would remove if it started at `as_of`.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionPreview {
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub as_of: OffsetDateTime,
    pub policy: RetentionPolicy,
    pub affected: RetentionCounts,
}

/// Dry run (Admin only): exact counts per category for the given periods,
/// saved or not. Changes nothing and writes no audit event.
#[utoipa::path(post, path = "/api/admin/retention/preview", tag = "admin", request_body = RetentionPolicy,
    responses(
        (status = 200, body = RetentionPreview),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (a period: `out_of_range`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn preview_retention(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(policy): Json<RetentionPolicy>,
) -> Result<Json<RetentionPreview>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let errors = policy.validate();
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let mut tx = crate::submissions::read_only(&state).await?;
    let as_of: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    let affected = preview(&mut tx, &policy, as_of).await?;
    tx.commit().await?;
    Ok(Json(RetentionPreview {
        as_of,
        policy,
        affected,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartRetentionRun {
    /// Must be `DELETE`.
    pub confirm: String,
}

/// Starts applying the saved policy now (Admin only). The retention worker
/// does the work in batches; `GET /api/admin/retention` shows its progress.
#[utoipa::path(post, path = "/api/admin/retention/runs", tag = "admin", request_body = StartRetentionRun,
    responses(
        (status = 202, body = RetentionRun),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict`: a run is already pending", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`confirm`: `invalid`; `policy`: `not_configured`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn start_retention_run(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<StartRetentionRun>,
) -> Result<(StatusCode, Json<RetentionRun>), ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let invalid = |field: &str, code| ApiError::Validation([(field.to_owned(), vec![code])].into());
    if !confirmed(Some(&body.confirm)) {
        return Err(invalid("confirm", "invalid"));
    }
    let mut tx = state.pool.begin().await?;
    let settings = load_settings(&mut tx, true).await?;
    if settings.policy.is_empty() {
        return Err(invalid("policy", "not_configured"));
    }
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    let id = insert_run(&mut tx, &settings.policy, Some(user.account_id()), now)
        .await?
        .ok_or(ApiError::Conflict)?;
    let run = runs(&mut tx, "WHERE r.id = $1", Some(id))
        .await?
        .pop()
        .ok_or(ApiError::Internal)?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(days: [Option<i64>; 5]) -> RetentionPolicy {
        RetentionPolicy {
            draft_days: days[0],
            screenshot_days: days[1],
            ai_content_days: days[2],
            ticket_content_days: days[3],
            audit_days: days[4],
        }
    }

    #[test]
    fn the_default_policy_keeps_everything() {
        let p = RetentionPolicy::default();
        assert!(p.is_empty());
        assert!(p.validate().is_empty());
        let c = p.cutoffs(OffsetDateTime::UNIX_EPOCH);
        assert!(c.drafts.is_none() && c.screenshots.is_none() && c.ai_content.is_none());
        assert!(c.ticket_content.is_none() && c.audit.is_none());
    }

    #[test]
    fn periods_are_bounded_and_audit_has_the_higher_floor() {
        assert!(
            policy([Some(1), Some(1), Some(1), Some(1), Some(30)])
                .validate()
                .is_empty()
        );
        assert!(policy([Some(3650); 5]).validate().is_empty());
        let errors = policy([Some(0), Some(3651), Some(-1), Some(1), Some(29)]).validate();
        let fields: Vec<_> = errors.keys().map(String::as_str).collect();
        assert_eq!(
            fields,
            [
                "ai_content_days",
                "audit_days",
                "draft_days",
                "screenshot_days"
            ]
        );
    }

    #[test]
    fn ticket_content_bounds_its_screenshots_and_ai_content() {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(1000);
        let days = |d: i64| Some(now - Duration::days(d));
        let c = policy([None, Some(90), None, Some(30), None]).cutoffs(now);
        assert_eq!(c.screenshots, days(30));
        assert_eq!(c.ai_content, days(30));
        assert_eq!(c.ticket_content, days(30));
        let c = policy([None, Some(10), Some(400), Some(30), None]).cutoffs(now);
        assert_eq!(c.screenshots, days(10));
        assert_eq!(c.ai_content, days(30));
        let c = policy([None, Some(10), None, None, None]).cutoffs(now);
        assert_eq!(
            (c.screenshots, c.ai_content, c.ticket_content),
            (days(10), None, None)
        );
    }
}
