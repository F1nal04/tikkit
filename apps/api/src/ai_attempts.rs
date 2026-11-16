//! Persisted AI suggestion attempts and the worker that runs them. See
//! docs/ai-attempts.md.
//!
//! A ticket that enters `ai_processing` gets exactly one `ai_attempts` row in
//! the submission transaction ([`create`]), holding the immutable, minimized
//! input. The worker ([`worker_loop`]) claims attempts under a lease, prepares
//! the screenshots, calls the provider through [`AiGuard::suggest`], and ends
//! the attempt in one transaction that re-checks it is still current: either
//! the suggestion is stored and the ticket moves to `awaiting_ai_decision`, or
//! the ticket is handed to operators exactly once.
//!
//! Everything that can end an attempt (the worker, the deadline sweep, an Admin
//! configuration change, a requester cancellation or skip) locks the ticket row
//! first and the attempt second, and only acts while the attempt is still
//! `processing`. A late provider result therefore finds a finished attempt and
//! is dropped.
//!
//! Logs carry ids and stable codes only, never ticket text or image data.

use std::{
    collections::HashMap,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use image::{
    DynamicImage, ImageFormat, ImageReader, Limits, RgbImage,
    codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
    imageops::FilterType,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    ai::{
        self, Cancel, ConditionalAnswer, ImageInput, ImageMediaType, MAX_IMAGE_BYTES,
        MAX_IMAGE_SIDE, MAX_TOTAL_IMAGE_BYTES, SuggestionRequest,
    },
    ai_config::{self, AiStatus},
    ai_guard::{AiGuard, Guarded},
    ai_privacy,
    api::{ApiError, Language},
    auth::{ident, ident_str},
    lifecycle::{self, By, Transition},
    notifications::poll_loop,
    screenshots,
    submission::SubmittedIntake,
};

/// Overall bound of one attempt, from submission. Past it the ticket is handed
/// to operators even if no worker is alive.
pub const ATTEMPT_DEADLINE: Duration = Duration::from_secs(300);
/// Attempts one process works on at the same time. Provider concurrency over
/// all processes is the guard's limit (docs/ai-usage.md).
const WORKER_CONCURRENCY: usize = 4;
/// Tries to commit a finished attempt before leaving it to the lease.
const COMMIT_TRIES: u32 = 3;

// --- Stable outcome codes ---------------------------------------------------------

/// The attempt passed [`ATTEMPT_DEADLINE`].
pub const TIMEOUT: &str = "timeout";
/// An Admin disabled AI.
pub const AI_DISABLED: &str = "ai_disabled";
/// An Admin selected another provider or model.
pub const CONFIGURATION_CHANGED: &str = "configuration_changed";
/// An Admin replaced the selected provider's key.
pub const CREDENTIAL_CHANGED: &str = "credential_changed";
/// The selected provider's key was removed.
pub const CREDENTIAL_MISSING: &str = "credential_missing";
/// An Admin marked the ticket's category "Skip AI automatically".
pub const CATEGORY_EXEMPT: &str = "category_exempt";
/// A snapshotted screenshot is missing or its bytes no longer match the snapshot.
pub const IMAGE_UNAVAILABLE: &str = "image_unavailable";
/// A screenshot could not be brought within the provider image limits.
pub const IMAGE_PREPARATION_FAILED: &str = "image_preparation_failed";
/// The requester skipped AI (only with the Admin toggle).
pub const REQUESTER_SKIP: &str = "requester_skip";
/// The requester cancelled the whole ticket.
pub const REQUESTER_CANCELLED: &str = "requester_cancelled";
/// The ticket was in `ai_processing` without a processing attempt.
pub const ORPHANED: &str = "orphaned";

// --- Snapshot -----------------------------------------------------------------------

/// The immutable attempt input (`ai_attempts.input`): the minimized intake and
/// the intake screenshots it refers to. Nothing else ever reaches the provider.
#[derive(Debug, Serialize, Deserialize)]
pub struct AttemptInput {
    pub category: String,
    pub subject: String,
    pub description: String,
    /// Visible conditional answers (label and value), in display order.
    pub answers: Vec<InputAnswer>,
    pub screenshots: Vec<InputScreenshot>,
    /// How many secrets `ai_privacy::minimize` replaced.
    pub redactions: usize,
    /// Whether any text was cut to the adapter bounds.
    pub truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InputAnswer {
    pub label: String,
    pub value: String,
}

/// One intake screenshot as it was at submission.
#[derive(Debug, Serialize, Deserialize)]
pub struct InputScreenshot {
    pub id: Uuid,
    /// SHA-256 of the stored file, hex. `None`: the file was unreadable at
    /// submission, so the attempt will hand off.
    pub sha256: Option<String>,
    pub content_type: String,
    pub byte_size: i32,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn read_file(path: PathBuf) -> Option<Vec<u8>> {
    tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .ok()?
        .ok()
}

/// Creates the one attempt of a ticket that just entered `ai_processing`, in
/// the submission transaction. The input is built from the frozen intake only
/// and passed through [`ai_privacy::minimize`]; the requester's current
/// language and the configuration it was created under are stored with it.
pub async fn create(
    db: &mut PgConnection,
    screenshot_dir: &Path,
    ticket_id: Uuid,
    requester: Uuid,
    intake: &SubmittedIntake,
) -> Result<(), ApiError> {
    let language: String = sqlx::query_scalar("SELECT language FROM accounts WHERE id = $1")
        .bind(requester)
        .fetch_one(&mut *db)
        .await?;
    let language: Language = ident(language)?;
    // `lifecycle::submit` just routed on this configuration and still holds its
    // share lock, so it cannot have changed.
    let (provider, model, credential_set_at) = ai_config::selection(&mut *db)
        .await?
        .ok_or(ApiError::Internal)?;
    // Only screenshots this submission attached to this ticket.
    let rows: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT id, content_type, byte_size FROM screenshots
         WHERE id = ANY($1) AND ticket_id = $2 AND message_id IS NULL
           AND uploader_account_id = $3",
    )
    .bind(&intake.screenshot_ids)
    .bind(ticket_id)
    .bind(requester)
    .fetch_all(&mut *db)
    .await?;
    let mut rows: HashMap<Uuid, (String, i32)> = rows
        .into_iter()
        .map(|(id, content_type, size)| (id, (content_type, size)))
        .collect();
    let mut screenshots = Vec::with_capacity(intake.screenshot_ids.len());
    for id in &intake.screenshot_ids {
        let (content_type, byte_size) = rows.remove(id).ok_or(ApiError::Internal)?;
        let bytes = read_file(screenshots::file_path(screenshot_dir, *id)).await;
        screenshots.push(InputScreenshot {
            id: *id,
            sha256: bytes.as_deref().map(sha256_hex),
            content_type,
            byte_size,
        });
    }
    let minimized = ai_privacy::minimize(SuggestionRequest {
        language: ai_language(language),
        category: intake.catalog.category_name.clone(),
        subject: intake.subject.clone(),
        description: intake.description.clone(),
        answers: intake
            .catalog
            .answers
            .iter()
            .map(|a| ConditionalAnswer {
                label: a.label.clone(),
                value: a.value.clone(),
            })
            .collect(),
        images: vec![],
    });
    let request = minimized.request;
    let input = AttemptInput {
        category: request.category,
        subject: request.subject,
        description: request.description,
        answers: request
            .answers
            .into_iter()
            .map(|a| InputAnswer {
                label: a.label,
                value: a.value,
            })
            .collect(),
        screenshots,
        redactions: minimized.redactions,
        truncated: minimized.truncated,
    };
    sqlx::query(
        "INSERT INTO ai_attempts (ticket_id, language, input, provider, model,
                                  credential_set_at, deadline_at)
         VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp() + make_interval(secs => $7))",
    )
    .bind(ticket_id)
    .bind(ident_str(language))
    .bind(sqlx::types::Json(&input))
    .bind(provider)
    .bind(model)
    .bind(credential_set_at)
    .bind(ATTEMPT_DEADLINE.as_secs_f64())
    .execute(&mut *db)
    .await?;
    Ok(())
}

fn ai_language(language: Language) -> ai::Language {
    match language {
        Language::En => ai::Language::En,
        Language::De => ai::Language::De,
    }
}

// --- Ending an attempt --------------------------------------------------------------

/// Ends the ticket's attempt if it is still processing. The caller holds the
/// ticket row lock.
pub(crate) async fn close(
    db: &mut PgConnection,
    ticket_id: Uuid,
    state: &str,
    outcome: &str,
    usage_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let closed = sqlx::query(
        "UPDATE ai_attempts SET state = $2, outcome = $3, usage_id = COALESCE($4, usage_id),
                finished_at = clock_timestamp(), claimed_until = NULL
         WHERE ticket_id = $1 AND state = 'processing'",
    )
    .bind(ticket_id)
    .bind(state)
    .bind(outcome)
    .bind(usage_id)
    .execute(db)
    .await?
    .rows_affected();
    Ok(closed == 1)
}

/// Hands a ticket that is still in `ai_processing` to the operator queue and
/// ends its attempt with `outcome`, in the caller's transaction. A ticket that
/// already left `ai_processing` (cancelled, skipped, handed off, or answered)
/// is left alone, so this can never queue a ticket twice. Returns whether it
/// queued the ticket.
async fn hand_off(
    tx: &mut Transaction<'_, Postgres>,
    ticket_id: Uuid,
    outcome: &str,
    usage_id: Option<Uuid>,
) -> Result<bool, ApiError> {
    // Locks the ticket row, then checks its status under that lock.
    match lifecycle::transition(tx, ticket_id, By::System, &Transition::AiFailed).await {
        Ok(_) => {
            close(tx, ticket_id, "handed_off", outcome, usage_id).await?;
            Ok(true)
        }
        Err(ApiError::Conflict) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Stops every processing attempt (of one category, or all) and hands those
/// tickets to operators, in the Admin's own transaction, so the change and the
/// handoffs commit together. A provider call still in flight finds its attempt
/// finished and its result is dropped. Returns how many tickets were queued.
pub async fn invalidate(
    tx: &mut Transaction<'_, Postgres>,
    category_id: Option<Uuid>,
    outcome: &str,
) -> Result<u64, ApiError> {
    // Ordered, so two invalidations take ticket locks in the same order.
    let tickets: Vec<Uuid> = sqlx::query_scalar(
        "SELECT a.ticket_id FROM ai_attempts a JOIN tickets t ON t.id = a.ticket_id
         WHERE a.state = 'processing' AND ($1::uuid IS NULL OR t.category_id = $1)
         ORDER BY a.ticket_id",
    )
    .bind(category_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut queued = 0;
    for ticket_id in tickets {
        if hand_off(tx, ticket_id, outcome, None).await? {
            queued += 1;
        }
    }
    Ok(queued)
}

// --- Requester view -------------------------------------------------------------------

/// Where a ticket's single AI attempt stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiAttemptState {
    /// The suggestion is being prepared. Poll `GET /api/tickets/{id}/activity`.
    Processing,
    /// The one suggestion is stored in `suggestion`.
    SuggestionReady,
    /// There is no suggestion; the ticket went to the help desk. See `reason`.
    HandedOff,
    /// The requester cancelled the ticket while the suggestion was being prepared.
    Cancelled,
}

/// Why a ticket went to the help desk without a suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiHandoffReason {
    /// The suggestion took too long.
    Timeout,
    /// The requester skipped AI.
    RequesterSkip,
    /// AI assistance was switched off or reconfigured, or no longer applies to
    /// the category.
    AiUnavailable,
    /// The suggestion could not be produced (provider, limits, or screenshots).
    AiFailed,
}

/// The single AI suggestion of a ticket.
#[derive(Debug, Serialize, ToSchema)]
pub struct AiSuggestion {
    pub explanation: String,
    /// Ordered steps, 1 to 12.
    pub steps: Vec<String>,
}

/// The AI attempt of a ticket, as its requester sees it.
#[derive(Debug, Serialize, ToSchema)]
pub struct TicketAi {
    pub state: AiAttemptState,
    /// The language the suggestion was requested in: the requester's language
    /// at submission. It never changes afterwards.
    pub language: Language,
    /// Set once `state` was `suggestion_ready`. There is never a second one.
    #[schema(required = true)]
    pub suggestion: Option<AiSuggestion>,
    /// Set when `state` is `handed_off`.
    #[schema(required = true)]
    pub reason: Option<AiHandoffReason>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub finished_at: Option<OffsetDateTime>,
}

/// The public reason for a stored outcome code. Requesters do not learn budget,
/// credential, or provider details.
fn public_reason(outcome: &str) -> AiHandoffReason {
    match outcome {
        TIMEOUT => AiHandoffReason::Timeout,
        REQUESTER_SKIP => AiHandoffReason::RequesterSkip,
        AI_DISABLED
        | CONFIGURATION_CHANGED
        | CREDENTIAL_CHANGED
        | CREDENTIAL_MISSING
        | CATEGORY_EXEMPT
        | "credential_unreadable"
        | "model_unsupported" => AiHandoffReason::AiUnavailable,
        _ => AiHandoffReason::AiFailed,
    }
}

type ViewRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    Option<sqlx::types::Json<Vec<String>>>,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

/// The ticket's attempt for its requester, or `None` if the ticket never went
/// through AI. The caller has already decided the ticket is the caller's own.
pub async fn view(db: &mut PgConnection, ticket_id: Uuid) -> Result<Option<TicketAi>, ApiError> {
    let row: Option<ViewRow> = sqlx::query_as(
        "SELECT state, outcome, language, explanation, steps, created_at, finished_at
         FROM ai_attempts WHERE ticket_id = $1",
    )
    .bind(ticket_id)
    .fetch_optional(db)
    .await?;
    let Some((state, outcome, language, explanation, steps, created_at, finished_at)) = row else {
        return Ok(None);
    };
    let state: AiAttemptState = ident(state)?;
    Ok(Some(TicketAi {
        state,
        language: ident(language)?,
        suggestion: explanation
            .zip(steps)
            .map(|(explanation, steps)| AiSuggestion {
                explanation,
                steps: steps.0,
            }),
        reason: (state == AiAttemptState::HandedOff)
            .then(|| public_reason(outcome.as_deref().unwrap_or_default())),
        created_at,
        finished_at,
    }))
}

// --- Worker ---------------------------------------------------------------------------

/// Worker timing. Tests shorten it.
#[derive(Clone, Copy, Debug)]
pub struct WorkerOptions {
    /// How long a claim lasts without a heartbeat. After a crash another worker
    /// takes the attempt over once this has passed.
    pub lease: Duration,
    /// How often a running attempt renews its lease and checks it is still current.
    pub heartbeat: Duration,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(30),
            heartbeat: Duration::from_secs(5),
        }
    }
}

/// Everything the attempt worker needs. Cheap to clone.
#[derive(Clone)]
pub struct Worker {
    pool: PgPool,
    screenshot_dir: Arc<Path>,
    guard: AiGuard,
    encryption_key: [u8; 32],
    options: WorkerOptions,
}

impl Worker {
    pub fn new(state: &AppState, encryption_key: [u8; 32]) -> Self {
        Self::with(state, AiGuard::from_state(state), encryption_key)
    }

    /// A worker with its own guard (tests use a client with short timeouts).
    pub fn with(state: &AppState, guard: AiGuard, encryption_key: [u8; 32]) -> Self {
        Self {
            pool: state.pool.clone(),
            screenshot_dir: state.screenshot_dir.clone(),
            guard,
            encryption_key,
            options: WorkerOptions::default(),
        }
    }

    pub fn options(mut self, options: WorkerOptions) -> Self {
        self.options = options;
        self
    }
}

#[derive(sqlx::FromRow)]
struct Job {
    id: Uuid,
    ticket_id: Uuid,
    requester_account_id: Uuid,
    language: String,
    input: sqlx::types::Json<AttemptInput>,
    provider: String,
    model: String,
    credential_set_at: OffsetDateTime,
    claims: i32,
}

/// What processing an attempt produced, before it is committed.
enum Outcome {
    Suggested(Box<Guarded>),
    Failed {
        code: &'static str,
        usage_id: Option<Uuid>,
    },
}

fn failed(code: &'static str) -> Outcome {
    Outcome::Failed {
        code,
        usage_id: None,
    }
}

/// Runs until [`crate::notifications::stop_workers`]: claims attempts and works
/// on up to [`WORKER_CONCURRENCY`] at once, and separately hands off attempts
/// that passed their deadline. Any number of processes may run it.
pub async fn worker_loop(worker: Worker) {
    let slots = Arc::new(Semaphore::new(WORKER_CONCURRENCY));
    let run = poll_loop("ai_attempts", || {
        let (worker, slots) = (worker.clone(), slots.clone());
        async move {
            let permit = slots
                .acquire_owned()
                .await
                .map_err(|_| ApiError::Internal)?;
            let Some(job) = claim(&worker).await? else {
                return Ok(false);
            };
            tokio::spawn(async move {
                let _permit = permit;
                // A failure leaves the attempt to its lease or the deadline sweep.
                let _ = run_attempt(&worker, job).await;
            });
            Ok(true)
        }
    });
    let sweep = poll_loop("ai_attempt_sweep", || sweep_one(&worker.pool));
    tokio::join!(run, sweep);
}

/// Claims one attempt and works on it to the end. Returns whether there was one.
pub async fn process_one(worker: &Worker) -> Result<bool, ApiError> {
    let Some(job) = claim(worker).await? else {
        return Ok(false);
    };
    run_attempt(worker, job).await?;
    Ok(true)
}

/// Takes the oldest attempt that no live worker holds. One statement: the row
/// lock makes concurrent claimers skip it, the lease keeps it taken afterwards.
async fn claim(worker: &Worker) -> Result<Option<Job>, ApiError> {
    let job = sqlx::query_as(
        "UPDATE ai_attempts a
         SET claimed_until = clock_timestamp() + make_interval(secs => $1), claims = a.claims + 1
         FROM tickets t
         WHERE t.id = a.ticket_id AND a.id = (
             SELECT id FROM ai_attempts
             WHERE state = 'processing' AND deadline_at > clock_timestamp()
               AND (claimed_until IS NULL OR claimed_until <= clock_timestamp())
             ORDER BY created_at, id LIMIT 1 FOR UPDATE SKIP LOCKED)
         RETURNING a.id, a.ticket_id, t.requester_account_id, a.language, a.input, a.provider,
                   a.model, a.credential_set_at, a.claims",
    )
    .bind(worker.options.lease.as_secs_f64())
    .fetch_optional(&worker.pool)
    .await?;
    Ok(job)
}

/// Renews the lease. `false`: the attempt is finished, overdue, or claimed by
/// another worker, so the running call should stop.
async fn renew(worker: &Worker, job: &Job) -> bool {
    let renewed = sqlx::query(
        "UPDATE ai_attempts SET claimed_until = clock_timestamp() + make_interval(secs => $3)
         WHERE id = $1 AND claims = $2 AND state = 'processing'
           AND deadline_at > clock_timestamp()",
    )
    .bind(job.id)
    .bind(job.claims)
    .bind(worker.options.lease.as_secs_f64())
    .execute(&worker.pool)
    .await;
    // A database hiccup is not a reason to abandon the call.
    renewed.map_or(true, |done| done.rows_affected() == 1)
}

async fn run_attempt(worker: &Worker, job: Job) -> Result<(), ApiError> {
    let cancel = Cancel::new();
    let outcome = {
        let work = attempt(worker, &job, &cancel);
        tokio::pin!(work);
        let start = tokio::time::Instant::now() + worker.options.heartbeat;
        let mut beat = tokio::time::interval_at(start, worker.options.heartbeat);
        loop {
            tokio::select! {
                outcome = &mut work => break outcome,
                _ = beat.tick() => {
                    if !cancel.is_cancelled() && !renew(worker, &job).await {
                        cancel.cancel();
                    }
                }
            }
        }
    };
    let mut tries = 0;
    loop {
        tries += 1;
        match finish(worker, &job, &outcome).await {
            Ok(()) => return Ok(()),
            Err(error) if tries >= COMMIT_TRIES => {
                tracing::error!(
                    worker = "ai_attempts",
                    attempt_id = %job.id,
                    "AI attempt result could not be committed; leaving it to the lease"
                );
                return Err(error);
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Whether the attempt may still run: AI is usable, the selection and key are
/// the ones the attempt was created under, and the category is not exempt.
/// Inside a transaction the share locks make a concurrent Admin change wait.
async fn still_allowed(
    db: &mut PgConnection,
    job: &Job,
    encryption_key: &[u8; 32],
) -> Result<Result<ai_config::ActiveAi, &'static str>, sqlx::Error> {
    let active = match ai_config::load_active(&mut *db, encryption_key).await? {
        Ok(active) => active,
        Err(status) => {
            return Ok(Err(match status {
                AiStatus::CredentialMissing => CREDENTIAL_MISSING,
                AiStatus::CredentialUnreadable => "credential_unreadable",
                AiStatus::ModelUnsupported => "model_unsupported",
                AiStatus::Disabled | AiStatus::Ready => AI_DISABLED,
            }));
        }
    };
    match ai_config::selection(&mut *db).await? {
        Some((provider, model, set_at)) if provider == job.provider && model == job.model => {
            if set_at != job.credential_set_at {
                return Ok(Err(CREDENTIAL_CHANGED));
            }
        }
        _ => return Ok(Err(CONFIGURATION_CHANGED)),
    }
    let exempt: bool = sqlx::query_scalar(
        "SELECT c.skip_ai FROM categories c JOIN tickets t ON t.category_id = c.id
         WHERE t.id = $1 FOR SHARE OF c",
    )
    .bind(job.ticket_id)
    .fetch_one(&mut *db)
    .await?;
    Ok(if exempt {
        Err(CATEGORY_EXEMPT)
    } else {
        Ok(active)
    })
}

/// The provider call of one attempt: policy check, screenshots, guarded call.
/// Holds no transaction or lock while the provider answers.
async fn attempt(worker: &Worker, job: &Job, cancel: &Cancel) -> Outcome {
    let allowed = async {
        let mut db = worker.pool.acquire().await?;
        still_allowed(&mut db, job, &worker.encryption_key).await
    };
    let active = match allowed.await {
        Ok(Ok(active)) => active,
        Ok(Err(code)) => return failed(code),
        Err(_) => return failed("guard_unavailable"),
    };
    let Ok(language) = ident::<Language>(job.language.clone()) else {
        return failed("invalid_input");
    };
    let images = match load_images(worker, job).await {
        Ok(images) => images,
        Err(code) => return failed(code),
    };
    if cancel.is_cancelled() {
        return failed("cancelled");
    }
    let input = &job.input.0;
    let request = SuggestionRequest {
        language: ai_language(language),
        category: input.category.clone(),
        subject: input.subject.clone(),
        description: input.description.clone(),
        answers: input
            .answers
            .iter()
            .map(|a| ConditionalAnswer {
                label: a.label.clone(),
                value: a.value.clone(),
            })
            .collect(),
        images,
    };
    match worker
        .guard
        .suggest(job.requester_account_id, &active, &request, cancel)
        .await
    {
        Ok(guarded) => Outcome::Suggested(Box::new(guarded)),
        Err(refused) => Outcome::Failed {
            code: refused.refusal.code(),
            usage_id: refused.usage_id,
        },
    }
}

/// Resolves the snapshotted screenshots against the ticket's own intake, checks
/// each file is still the snapshotted content, and brings them within the
/// provider image limits. Every screenshot is sent or the attempt fails.
async fn load_images(worker: &Worker, job: &Job) -> Result<Vec<ImageInput>, &'static str> {
    let wanted = &job.input.0.screenshots;
    if wanted.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<Uuid> = wanted.iter().map(|s| s.id).collect();
    let owned: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, content_type FROM screenshots
         WHERE id = ANY($1) AND ticket_id = $2 AND message_id IS NULL
           AND uploader_account_id = $3",
    )
    .bind(&ids)
    .bind(job.ticket_id)
    .bind(job.requester_account_id)
    .fetch_all(&worker.pool)
    .await
    .map_err(|_| IMAGE_UNAVAILABLE)?;
    let owned: HashMap<Uuid, String> = owned.into_iter().collect();
    let mut images = Vec::with_capacity(wanted.len());
    for shot in wanted {
        let content_type = owned.get(&shot.id).ok_or(IMAGE_UNAVAILABLE)?;
        let media_type = ImageMediaType::parse(content_type).ok_or(IMAGE_UNAVAILABLE)?;
        let bytes = read_file(screenshots::file_path(&worker.screenshot_dir, shot.id))
            .await
            .ok_or(IMAGE_UNAVAILABLE)?;
        if shot.sha256.as_deref() != Some(sha256_hex(&bytes).as_str()) {
            return Err(IMAGE_UNAVAILABLE);
        }
        images.push(ImageInput { media_type, bytes });
    }
    tokio::task::spawn_blocking(move || prepare_images(images))
        .await
        .map_err(|_| IMAGE_PREPARATION_FAILED)?
        .map_err(|PrepareError| IMAGE_PREPARATION_FAILED)
}

/// Ends the attempt with `outcome` in one transaction, after re-checking under
/// the ticket and attempt locks that it is still processing, within its
/// deadline, and still allowed by the current configuration. Otherwise the
/// result is dropped (finished attempt) or replaced by a handoff.
async fn finish(worker: &Worker, job: &Job, outcome: &Outcome) -> Result<(), ApiError> {
    let mut tx = worker.pool.begin().await?;
    // Lock order everywhere: AI settings, category, ticket, attempt.
    let allowed = still_allowed(&mut tx, job, &worker.encryption_key).await?;
    sqlx::query("SELECT 1 FROM tickets WHERE id = $1 FOR UPDATE")
        .bind(job.ticket_id)
        .execute(&mut *tx)
        .await?;
    // Still processing, and still this worker's claim: after a lost lease the
    // attempt belongs to the worker that took it over.
    let current: Option<(bool, bool)> = sqlx::query_as(
        "SELECT state = 'processing' AND claims = $2, deadline_at <= clock_timestamp()
         FROM ai_attempts WHERE id = $1 FOR UPDATE",
    )
    .bind(job.id)
    .bind(job.claims)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((true, overdue)) = current else {
        tracing::info!(
            worker = "ai_attempts",
            attempt_id = %job.id,
            "AI attempt already finished or taken over; late result ignored"
        );
        return Ok(());
    };
    let (code, usage_id) = match (overdue, allowed, outcome) {
        (true, ..) => (TIMEOUT, None),
        (false, Err(code), _) => (code, None),
        (false, Ok(_), Outcome::Failed { code, usage_id }) => (*code, *usage_id),
        (false, Ok(_), Outcome::Suggested(guarded)) => {
            lifecycle::transition(
                &mut tx,
                job.ticket_id,
                By::System,
                &Transition::AiSuggestionReady,
            )
            .await?;
            sqlx::query(
                "UPDATE ai_attempts SET state = 'suggestion_ready', explanation = $2, steps = $3,
                        usage_id = $4, finished_at = clock_timestamp(), claimed_until = NULL
                 WHERE id = $1",
            )
            .bind(job.id)
            .bind(&guarded.suggestion.explanation)
            .bind(sqlx::types::Json(&guarded.suggestion.steps))
            .bind(guarded.usage_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::info!(
                worker = "ai_attempts",
                attempt_id = %job.id,
                "AI suggestion stored"
            );
            return Ok(());
        }
    };
    // A suggestion that arrived too late or under a changed policy is not stored.
    let usage_id = match outcome {
        Outcome::Suggested(guarded) => Some(guarded.usage_id),
        Outcome::Failed { .. } => usage_id,
    };
    hand_off(&mut tx, job.ticket_id, code, usage_id).await?;
    tx.commit().await?;
    tracing::info!(
        worker = "ai_attempts",
        attempt_id = %job.id,
        outcome = code,
        "AI attempt handed off to operators"
    );
    Ok(())
}

/// Hands off one ticket that is in `ai_processing` with an overdue attempt (or
/// with no processing attempt at all), whatever any worker is doing. Returns
/// whether it found one. All state is in the database, so this also recovers
/// attempts whose worker died.
pub async fn sweep_one(pool: &PgPool) -> Result<bool, ApiError> {
    let due: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT t.id, a.state IS NOT DISTINCT FROM 'processing'
         FROM tickets t LEFT JOIN ai_attempts a ON a.ticket_id = t.id
         WHERE t.status = 'ai_processing'
           AND (a.state IS DISTINCT FROM 'processing' OR a.deadline_at <= clock_timestamp())
         ORDER BY t.created_at, t.id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    let Some((ticket_id, overdue)) = due else {
        return Ok(false);
    };
    let outcome = if overdue { TIMEOUT } else { ORPHANED };
    let mut tx = pool.begin().await?;
    let queued = hand_off(&mut tx, ticket_id, outcome, None).await?;
    tx.commit().await?;
    if queued {
        tracing::warn!(
            worker = "ai_attempt_sweep",
            %ticket_id,
            outcome,
            "AI attempt handed off to operators"
        );
    }
    Ok(true)
}

// --- Image preparation ----------------------------------------------------------------

/// A screenshot could not be brought within the provider limits.
#[derive(Debug, PartialEq, Eq)]
pub struct PrepareError;

/// JPEG qualities tried at full resolution before anything is scaled down.
const JPEG_QUALITIES: [u8; 2] = [90, 80];
/// Quality of a scaled-down screenshot.
const SCALED_QUALITY: u8 = 85;
/// A screenshot is never scaled below this fraction of its size.
const MIN_SCALE: f64 = 0.1;

/// Brings stored screenshots within the adapter limits ([`MAX_IMAGE_BYTES`],
/// [`MAX_IMAGE_SIDE`], [`MAX_TOTAL_IMAGE_BYTES`]). Images already within them
/// pass through byte for byte. A larger one is re-encoded, keeping as much
/// detail as fits so text in it stays readable: lossless at full size, then
/// high-quality JPEG at full size, then scaled down in small steps. No image is
/// dropped: if one cannot be made to fit, the whole preparation fails.
pub fn prepare_images(images: Vec<ImageInput>) -> Result<Vec<ImageInput>, PrepareError> {
    let fit_all = |budget: usize| -> Result<Vec<ImageInput>, PrepareError> {
        images.iter().map(|image| fit(image, budget)).collect()
    };
    let prepared = fit_all(MAX_IMAGE_BYTES)?;
    let total: usize = prepared.iter().map(|i| i.bytes.len()).sum();
    if total <= MAX_TOTAL_IMAGE_BYTES {
        return Ok(prepared);
    }
    // Too much together: give every image an equal share, starting again from
    // the stored files so nothing is compressed twice.
    fit_all(MAX_TOTAL_IMAGE_BYTES / images.len())
}

fn format_of(media_type: ImageMediaType) -> ImageFormat {
    match media_type {
        ImageMediaType::Png => ImageFormat::Png,
        ImageMediaType::Jpeg => ImageFormat::Jpeg,
        ImageMediaType::WebP => ImageFormat::WebP,
    }
}

fn reader(input: &ImageInput) -> ImageReader<Cursor<&[u8]>> {
    let mut reader =
        ImageReader::with_format(Cursor::new(&input.bytes[..]), format_of(input.media_type));
    // The same bounds stored screenshots were validated against.
    let mut limits = Limits::default();
    limits.max_image_width = Some(screenshots::MAX_SIDE);
    limits.max_image_height = Some(screenshots::MAX_SIDE);
    limits.max_alloc = Some(screenshots::MAX_PIXELS * 8 + (64 << 20));
    reader.limits(limits);
    reader
}

fn fit(input: &ImageInput, budget: usize) -> Result<ImageInput, PrepareError> {
    let (width, height) = reader(input).into_dimensions().map_err(|_| PrepareError)?;
    if u64::from(width) * u64::from(height) > screenshots::MAX_PIXELS {
        return Err(PrepareError);
    }
    let too_long = width.max(height) > MAX_IMAGE_SIDE;
    if input.bytes.len() <= budget && !too_long {
        return Ok(input.clone());
    }
    let mut image = reader(input).decode().map_err(|_| PrepareError)?;
    if too_long {
        image = image.resize(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE, FilterType::Lanczos3);
        // Only the size was in the way: keep the pixels lossless if that fits.
        if let Some(bytes) = lossless(&image, input.media_type).filter(|b| b.len() <= budget) {
            return Ok(ImageInput {
                media_type: input.media_type,
                bytes,
            });
        }
    }
    let rgb = on_white(image);
    let done = |bytes: Vec<u8>| ImageInput {
        media_type: ImageMediaType::Jpeg,
        bytes,
    };
    let mut smallest = usize::MAX;
    for quality in JPEG_QUALITIES {
        let bytes = jpeg(&rgb, quality)?;
        if bytes.len() <= budget {
            return Ok(done(bytes));
        }
        smallest = bytes.len();
    }
    // JPEG size is roughly proportional to the pixel count: start near the
    // scale that should fit instead of walking down from full size.
    let mut scale = ((budget as f64 / smallest as f64).sqrt() * 0.95).min(0.9);
    while scale >= MIN_SCALE {
        let side = |v: u32| ((f64::from(v) * scale).round() as u32).max(1);
        let scaled = image::imageops::resize(
            &rgb,
            side(rgb.width()),
            side(rgb.height()),
            FilterType::Lanczos3,
        );
        let bytes = jpeg(&scaled, SCALED_QUALITY)?;
        if bytes.len() <= budget {
            return Ok(done(bytes));
        }
        scale *= 0.85;
    }
    Err(PrepareError)
}

fn lossless(image: &DynamicImage, media_type: ImageMediaType) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match media_type {
        ImageMediaType::Png => image.write_with_encoder(PngEncoder::new(&mut out)).ok()?,
        ImageMediaType::WebP => {
            // The lossless WebP encoder takes 8-bit RGB(A) only.
            let image = if image.color().has_alpha() {
                DynamicImage::ImageRgba8(image.to_rgba8())
            } else {
                DynamicImage::ImageRgb8(image.to_rgb8())
            };
            image
                .write_with_encoder(WebPEncoder::new_lossless(&mut out))
                .ok()?
        }
        ImageMediaType::Jpeg => return None,
    }
    Some(out)
}

/// JPEG has no transparency: put the image on white, as a screenshot viewer would.
fn on_white(image: DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.into_rgb8();
    }
    let rgba = image.into_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (to, from) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u32::from(from[3]);
        for channel in 0..3 {
            let value = u32::from(from[channel]) * alpha + 255 * (255 - alpha);
            to[channel] = ((value + 127) / 255) as u8;
        }
    }
    rgb
}

fn jpeg(image: &RgbImage, quality: u8) -> Result<Vec<u8>, PrepareError> {
    let mut out = Vec::new();
    image
        .write_with_encoder(JpegEncoder::new_with_quality(&mut out, quality))
        .map_err(|_| PrepareError)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Incompressible pixels, deterministic.
    fn noise(width: u32, height: u32) -> RgbImage {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        RgbImage::from_fn(width, height, |_, _| {
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            };
            image::Rgb([next(), next(), next()])
        })
    }

    fn png(image: &RgbImage) -> ImageInput {
        let mut out = Vec::new();
        image.write_with_encoder(PngEncoder::new(&mut out)).unwrap();
        ImageInput {
            media_type: ImageMediaType::Png,
            bytes: out,
        }
    }

    fn size(image: &ImageInput) -> (u32, u32) {
        reader(image).into_dimensions().unwrap()
    }

    #[test]
    fn images_within_the_limits_pass_through_unchanged() {
        let small = png(&RgbImage::from_pixel(640, 480, image::Rgb([240, 240, 240])));
        let prepared = prepare_images(vec![small.clone(), small.clone()]).unwrap();
        assert_eq!(prepared.len(), 2);
        for image in prepared {
            assert_eq!(image.media_type, ImageMediaType::Png);
            assert_eq!(image.bytes, small.bytes);
        }
        assert_eq!(prepare_images(vec![]).unwrap().len(), 0);
    }

    #[test]
    fn an_oversized_screenshot_is_reencoded_to_fit_and_stays_decodable() {
        let big = png(&noise(1700, 1700));
        assert!(big.bytes.len() > MAX_IMAGE_BYTES);
        let prepared = prepare_images(vec![big]).unwrap();
        let [image] = &prepared[..] else {
            panic!("one image in, one image out");
        };
        assert!(image.bytes.len() <= MAX_IMAGE_BYTES);
        assert_eq!(image.media_type, ImageMediaType::Jpeg);
        let (width, height) = size(image);
        // Scaled only as far as needed: most of the resolution is kept.
        assert_eq!(width, height);
        assert!((600..=1700).contains(&width), "{width}");
        reader(image).decode().unwrap();
    }

    #[test]
    fn full_resolution_is_kept_when_high_quality_compression_is_enough() {
        // A flat screenshot with a block of noise: too big as PNG at this
        // budget, small enough as a high-quality JPEG without scaling.
        let mut shot = RgbImage::from_pixel(1200, 900, image::Rgb([250, 250, 250]));
        image::imageops::replace(&mut shot, &noise(400, 300), 100, 100);
        let input = png(&shot);
        let budget = input.bytes.len() * 2 / 3;
        let fitted = fit(&input, budget).unwrap();
        assert!(fitted.bytes.len() <= budget);
        assert_eq!(size(&fitted), (1200, 900));
    }

    #[test]
    fn a_too_long_side_is_scaled_down_losslessly() {
        let wide = png(&RgbImage::from_pixel(
            MAX_IMAGE_SIDE + 1000,
            40,
            image::Rgb([10, 120, 200]),
        ));
        let prepared = prepare_images(vec![wide]).unwrap();
        assert_eq!(prepared[0].media_type, ImageMediaType::Png);
        let (width, height) = size(&prepared[0]);
        assert_eq!(width, MAX_IMAGE_SIDE);
        assert!(height >= 35, "{height}");
    }

    #[test]
    fn the_total_limit_shrinks_images_but_never_drops_one() {
        let one = png(&noise(1250, 1250));
        assert!(one.bytes.len() < MAX_IMAGE_BYTES);
        let images = vec![one.clone(), one.clone(), one.clone(), one];
        assert!(images.iter().map(|i| i.bytes.len()).sum::<usize>() > MAX_TOTAL_IMAGE_BYTES);
        let prepared = prepare_images(images).unwrap();
        assert_eq!(prepared.len(), 4);
        assert!(prepared.iter().map(|i| i.bytes.len()).sum::<usize>() <= MAX_TOTAL_IMAGE_BYTES);
    }

    #[test]
    fn transparency_is_flattened_onto_white() {
        let clear = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let rgb = on_white(DynamicImage::ImageRgba8(clear));
        assert_eq!(rgb.get_pixel(0, 0), &image::Rgb([255, 255, 255]));
        let solid = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        let rgb = on_white(DynamicImage::ImageRgba8(solid));
        assert_eq!(rgb.get_pixel(1, 1), &image::Rgb([10, 20, 30]));
    }

    #[test]
    fn undecodable_bytes_fail_the_preparation() {
        let broken = ImageInput {
            media_type: ImageMediaType::Png,
            bytes: b"not an image".to_vec(),
        };
        assert!(prepare_images(vec![broken]).is_err());
    }

    #[test]
    fn requesters_only_see_coarse_handoff_reasons() {
        use AiHandoffReason::*;
        assert_eq!(public_reason(TIMEOUT), Timeout);
        assert_eq!(public_reason(REQUESTER_SKIP), RequesterSkip);
        for code in [
            AI_DISABLED,
            CONFIGURATION_CHANGED,
            CREDENTIAL_CHANGED,
            CREDENTIAL_MISSING,
            CATEGORY_EXEMPT,
        ] {
            assert_eq!(public_reason(code), AiUnavailable);
        }
        for code in [
            "budget_org_requests",
            "circuit_open",
            "rate_limited",
            IMAGE_UNAVAILABLE,
        ] {
            assert_eq!(public_reason(code), AiFailed);
        }
    }
}
