//! Private requester drafts and atomic, idempotent submission. See docs/submission.md.
use std::collections::BTreeMap;

use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, Cursor, FieldErrors, Json, Page, Pagination, Path, Query, TicketStatus},
    auth::{CurrentUser, audit, ident, ident_str},
    catalog::{self, IntakeAnswer, IntakeSnapshot},
    lifecycle::{self, By, Transition},
};

pub const SUBJECT_MAX: usize = 200;
pub const DESCRIPTION_MAX: usize = 20_000;
const ANSWERS_MAX: usize = 100;

/// Full replacement of editable draft input. Missing fields have empty defaults;
/// category/priority and required answers are checked against the catalog at submission.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TicketIntake {
    pub category_id: Option<Uuid>,
    pub priority_id: Option<Uuid>,
    /// Up to 200 characters; required on submission.
    #[serde(default)]
    pub subject: String,
    /// Up to 20,000 characters; required on submission.
    #[serde(default)]
    pub description: String,
    /// Up to 100 conditional answers, keyed by field ID.
    #[serde(default)]
    pub answers: BTreeMap<Uuid, IntakeAnswer>,
    /// Must be empty until validated screenshot uploads (#19) are available.
    #[serde(default)]
    pub screenshot_ids: Vec<Uuid>,
}

/// Original intake, frozen when submission commits; current effective priority
/// stays on the ticket independently of the initial catalog selection here.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SubmittedIntake {
    pub subject: String,
    pub description: String,
    pub catalog: IntakeSnapshot,
    pub screenshot_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Draft {
    pub id: Uuid,
    pub version: i64,
    #[sqlx(json)]
    pub intake: TicketIntake,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftUpdate {
    /// The version last read; stale writes return `409 conflict`.
    pub version: i64,
    pub intake: TicketIntake,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct DraftVersion {
    pub version: i64,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitTicket {
    /// Generate once per logical submission; retain across network retries.
    pub idempotency_key: Uuid,
    pub intake: TicketIntake,
    /// A request, never a bypass authority. The lifecycle checks current policy.
    #[serde(default)]
    pub requester_skip: bool,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitDraft {
    pub idempotency_key: Uuid,
    pub version: i64,
    #[serde(default)]
    pub requester_skip: bool,
}

/// Immutable submission receipt. A replay returns the original submitted status;
/// read `/api/tickets/{id}` for the current status.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubmissionReceipt {
    pub id: Uuid,
    pub reference: i64,
    pub status: TicketStatus,
}

fn check(errors: FieldErrors) -> Result<(), ApiError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

fn text(errors: &mut FieldErrors, field: &str, value: &str, max: usize, required: bool) {
    let code = if value.contains('\0') {
        Some("invalid")
    } else if value.chars().count() > max {
        Some("too_long")
    } else if required && value.trim().is_empty() {
        Some("required")
    } else {
        None
    };
    if let Some(code) = code {
        errors.insert(field.into(), vec![code]);
    }
}

impl TicketIntake {
    fn validate(&self, required: bool) -> Result<(), ApiError> {
        let mut errors = FieldErrors::new();
        text(&mut errors, "subject", &self.subject, SUBJECT_MAX, required);
        text(
            &mut errors,
            "description",
            &self.description,
            DESCRIPTION_MAX,
            required,
        );
        if required {
            if self.category_id.is_none() {
                errors.insert("category_id".into(), vec!["required"]);
            }
            if self.priority_id.is_none() {
                errors.insert("priority_id".into(), vec!["required"]);
            }
        }
        if self.answers.len() > ANSWERS_MAX {
            errors.insert("answers".into(), vec!["too_many"]);
        }
        // Draft storage is bounded too. Submission's active-answer semantics
        // (required/hidden/unknown) belong solely to catalog::validate_intake.
        if !required {
            for (id, answer) in &self.answers {
                if let Some(value) = &answer.text {
                    text(
                        &mut errors,
                        &format!("answers.{id}"),
                        value,
                        catalog::ANSWER_MAX,
                        false,
                    );
                }
            }
        }
        // No uploaded reference is trustworthy before #19 provides ownership,
        // decoded-image validation and limits. Never silently drop screenshots.
        if !self.screenshot_ids.is_empty() {
            errors.insert("screenshot_ids".into(), vec!["invalid"]);
        }
        check(errors)
    }
}

const DRAFT_COLUMNS: &str = "id, version, intake, created_at, updated_at";

async fn audit_draft(
    db: &mut PgConnection,
    user: &CurrentUser,
    event: &str,
    id: Uuid,
    version: i64,
) -> Result<(), ApiError> {
    audit(
        db,
        event,
        Some(user.account_id()),
        Some(user.account_id()),
        json!({"draft_id": id, "version": version}),
    )
    .await?;
    Ok(())
}

/// Save incomplete input privately. Retained until the owner deletes or submits it.
#[utoipa::path(post, path = "/api/ticket-drafts", tag = "tickets", request_body = TicketIntake,
    responses(
        (status = 201, body = Draft),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (input bounds, unsupported screenshots)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn create_draft(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<TicketIntake>,
) -> Result<(StatusCode, Json<Draft>), ApiError> {
    body.validate(false)?;
    let mut tx = state.pool.begin().await?;
    let draft: Draft = sqlx::query_as(&format!("INSERT INTO ticket_drafts (requester_account_id, intake) VALUES ($1, $2) RETURNING {DRAFT_COLUMNS}"))
        .bind(user.account_id()).bind(sqlx::types::Json(body)).fetch_one(&mut *tx).await?;
    audit_draft(&mut tx, &user, "draft_created", draft.id, draft.version).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(draft)))
}

/// Active drafts of this account only (including for staff/Admin accounts).
#[utoipa::path(get, path = "/api/ticket-drafts", tag = "tickets", params(Pagination),
    responses((status = 200, body = Page<Draft>), (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope)))]
pub async fn list_drafts(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(page): Query<Pagination>,
) -> Result<Json<Page<Draft>>, ApiError> {
    let cursor = page.cursor()?;
    let rows = sqlx::query_as(&format!("SELECT {DRAFT_COLUMNS} FROM ticket_drafts WHERE requester_account_id = $1 AND submitted_ticket_id IS NULL
        AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3)) ORDER BY created_at DESC, id DESC LIMIT $4"))
        .bind(user.account_id()).bind(cursor.map(|c| c.created_at)).bind(cursor.map(|c| c.id)).bind(page.limit() + 1)
        .fetch_all(&state.pool).await?;
    Ok(Json(Page::from_rows(rows, page.limit(), |d: &Draft| {
        Cursor {
            created_at: d.created_at,
            id: d.id,
        }
    })))
}

/// Resume one owned draft. Other accounts, including Admins, get `404`.
#[utoipa::path(get, path = "/api/ticket-drafts/{id}", tag = "tickets", params(("id" = Uuid, Path)),
    responses((status = 200, body = Draft), (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope)))]
pub async fn get_draft(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Draft>, ApiError> {
    let draft = sqlx::query_as(&format!("SELECT {DRAFT_COLUMNS} FROM ticket_drafts WHERE id = $1 AND requester_account_id = $2 AND submitted_ticket_id IS NULL"))
        .bind(id).bind(user.account_id()).fetch_optional(&state.pool).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(draft))
}

async fn lock_draft(
    db: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
    version: i64,
) -> Result<TicketIntake, ApiError> {
    let (current, intake): (i64, Option<sqlx::types::Json<TicketIntake>>) = sqlx::query_as(
        "SELECT version, intake FROM ticket_drafts WHERE id = $1 AND requester_account_id = $2 FOR UPDATE")
        .bind(id).bind(owner).fetch_optional(db).await?.ok_or(ApiError::NotFound)?;
    if current != version {
        return Err(ApiError::Conflict);
    }
    intake.map(|i| i.0).ok_or(ApiError::Conflict)
}

/// Replace editable draft input using the last read version. Never edits a submitted ticket.
#[utoipa::path(patch, path = "/api/ticket-drafts/{id}", tag = "tickets", request_body = DraftUpdate, params(("id" = Uuid, Path)),
    responses((status = 200, body = Draft), (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict` (stale version or already submitted)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed`", body = crate::api::ErrorEnvelope)))]
pub async fn update_draft(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<DraftUpdate>,
) -> Result<Json<Draft>, ApiError> {
    let mut tx = state.pool.begin().await?;
    lock_draft(&mut tx, user.account_id(), id, body.version).await?;
    body.intake.validate(false)?;
    let draft: Draft = sqlx::query_as(&format!("UPDATE ticket_drafts SET intake = $2, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {DRAFT_COLUMNS}"))
        .bind(id).bind(sqlx::types::Json(body.intake)).fetch_one(&mut *tx).await?;
    audit_draft(&mut tx, &user, "draft_updated", id, draft.version).await?;
    tx.commit().await?;
    Ok(Json(draft))
}

/// Delete an owned, unsubmitted draft at the last read version.
#[utoipa::path(delete, path = "/api/ticket-drafts/{id}", tag = "tickets", params(("id" = Uuid, Path), DraftVersion),
    responses((status = 204, description = "Deleted"), (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict` (stale version or already submitted)", body = crate::api::ErrorEnvelope)))]
pub async fn delete_draft(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(body): Query<DraftVersion>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    lock_draft(&mut tx, user.account_id(), id, body.version).await?;
    sqlx::query("DELETE FROM ticket_drafts WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit_draft(&mut tx, &user, "draft_deleted", id, body.version).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn request_hash(body: &impl Serialize) -> Result<Vec<u8>, ApiError> {
    let bytes = serde_json::to_vec(body).map_err(|_| ApiError::Internal)?;
    Ok(Sha256::digest(bytes).to_vec())
}

/// Serialize duplicate requests across API processes. Hash collisions only
/// serialize unrelated requests; the primary key below is the full UUID pair.
async fn replay(
    db: &mut PgConnection,
    owner: Uuid,
    key: Uuid,
    hash: &[u8],
) -> Result<Option<SubmissionReceipt>, ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))")
        .bind(format!("ticket-submission:{owner}"))
        .bind(key.to_string())
        .execute(&mut *db)
        .await?;
    let row: Option<(Vec<u8>, Uuid, i64, String)> = sqlx::query_as(
        "SELECT k.request_hash, t.id, t.reference, k.submitted_status::text FROM ticket_submission_keys k
         JOIN tickets t ON t.id = k.ticket_id WHERE k.requester_account_id = $1 AND k.idempotency_key = $2")
        .bind(owner).bind(key).fetch_optional(db).await?;
    let Some((stored_hash, id, reference, status)) = row else {
        return Ok(None);
    };
    if stored_hash != hash {
        return Err(ApiError::Conflict);
    }
    Ok(Some(SubmissionReceipt {
        id,
        reference,
        status: ident(status)?,
    }))
}

async fn persist_submission(
    db: &mut Transaction<'_, Postgres>,
    owner: Uuid,
    key: Uuid,
    hash: &[u8],
    intake: TicketIntake,
    requester_skip: bool,
) -> Result<SubmissionReceipt, ApiError> {
    intake.validate(true)?;
    // Catalog edits span several tables. Hold one coherent catalog until the
    // snapshot and routing commit; no provider calls happen under these locks.
    sqlx::query("LOCK TABLE categories, priorities, intake_fields, intake_field_categories, intake_field_choices IN SHARE MODE")
        .execute(&mut **db).await?;
    let catalog = catalog::validate_intake(
        db,
        intake.category_id.ok_or(ApiError::Internal)?,
        intake.priority_id.ok_or(ApiError::Internal)?,
        &intake.answers,
    )
    .await?;
    let snapshot = SubmittedIntake {
        subject: intake.subject.trim().to_owned(),
        description: intake.description.trim().to_owned(),
        catalog,
        screenshot_ids: intake.screenshot_ids,
    };
    let (id, reference): (Uuid, i64) = sqlx::query_as(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description, intake_snapshot)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, reference")
        .bind(owner).bind(snapshot.catalog.category_id).bind(snapshot.catalog.priority_id)
        .bind(&snapshot.subject).bind(&snapshot.description).bind(sqlx::types::Json(&snapshot)).fetch_one(&mut **db).await?;
    let status = lifecycle::transition(
        db,
        id,
        By::Account(owner),
        &Transition::Submit { requester_skip },
    )
    .await?;
    sqlx::query("INSERT INTO ticket_submission_keys (requester_account_id, idempotency_key, request_hash, ticket_id, submitted_status) VALUES ($1, $2, $3, $4, $5)")
        .bind(owner).bind(key).bind(hash).bind(id).bind(ident_str(status)).execute(&mut **db).await?;
    Ok(SubmissionReceipt {
        id,
        reference,
        status,
    })
}

/// Validate and submit new input. Owner is always the signed-in account. A
/// key reused with different input is `409`; identical retries replay the receipt.
#[utoipa::path(post, path = "/api/tickets", tag = "tickets", request_body = SubmitTicket,
    responses((status = 201, description = "Committed", body = SubmissionReceipt),
        (status = 200, description = "Original receipt (idempotent replay)", body = SubmissionReceipt),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (AI skip policy)", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict` (key already used for different input)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (intake/catalog)", body = crate::api::ErrorEnvelope)))]
pub async fn submit_ticket(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<SubmitTicket>,
) -> Result<(StatusCode, Json<SubmissionReceipt>), ApiError> {
    let hash = request_hash(&("ticket", &body))?;
    let mut tx = state.pool.begin().await?;
    if let Some(receipt) = replay(&mut tx, user.account_id(), body.idempotency_key, &hash).await? {
        tx.commit().await?;
        return Ok((StatusCode::OK, Json(receipt)));
    }
    let receipt = persist_submission(
        &mut tx,
        user.account_id(),
        body.idempotency_key,
        &hash,
        body.intake,
        body.requester_skip,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(receipt)))
}

/// Submit exactly the owned draft version last read. On failure the draft stays
/// editable with the same version and input; success leaves an immutable tombstone.
#[utoipa::path(post, path = "/api/ticket-drafts/{id}/submit", tag = "tickets", request_body = SubmitDraft, params(("id" = Uuid, Path)),
    responses((status = 201, description = "Committed", body = SubmissionReceipt),
        (status = 200, description = "Original receipt (idempotent replay)", body = SubmissionReceipt),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (AI skip policy)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict` (stale/submitted draft or key reuse)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (intake/catalog)", body = crate::api::ErrorEnvelope)))]
pub async fn submit_draft(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SubmitDraft>,
) -> Result<(StatusCode, Json<SubmissionReceipt>), ApiError> {
    let hash = request_hash(&("draft", id, &body))?;
    let mut tx = state.pool.begin().await?;
    if let Some(receipt) = replay(&mut tx, user.account_id(), body.idempotency_key, &hash).await? {
        tx.commit().await?;
        return Ok((StatusCode::OK, Json(receipt)));
    }
    let intake = lock_draft(&mut tx, user.account_id(), id, body.version).await?;
    let receipt = persist_submission(
        &mut tx,
        user.account_id(),
        body.idempotency_key,
        &hash,
        intake,
        body.requester_skip,
    )
    .await?;
    sqlx::query("UPDATE ticket_drafts SET intake = NULL, submitted_ticket_id = $2, version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id).bind(receipt.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(receipt)))
}
