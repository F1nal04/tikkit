//! Ticket catalog: Admin-managed categories, priorities, and limited conditional
//! intake fields; the active catalog for the shared ticket form; and the intake
//! validation that ticket submission runs. See docs/schema.md.
//!
//! Nothing is deleted. Archived rows disappear from the new-ticket form and from
//! [`validate_intake`] but stay readable for existing tickets, which keep the
//! [`IntakeSnapshot`] taken at submission. A category never selects a team.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json, Path},
    auth::{CurrentUser, audit, ident, unique_violation},
    authz::{Permission, require_permission},
};

const NAME_MAX: usize = 100;
const GUIDANCE_MAX: usize = 1000;
const CHOICES_MAX: usize = 100;
const ANSWER_MAX: usize = 500;

/// `archived: true` archives (keeping the first archival time), `false` restores,
/// absent leaves it. The flag is always bound as `$2`.
const ARCHIVE: &str = "archived_at = CASE WHEN $2::boolean IS NULL THEN archived_at
                                          WHEN $2 THEN COALESCE(archived_at, now()) END";

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    /// "Skip AI automatically": submissions go straight to the operator queue
    /// with no AI provider call, whatever the requester-skip setting says.
    pub skip_ai: bool,
    pub archived: bool,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Priority {
    pub id: Uuid,
    pub name: String,
    pub archived: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Select,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct IntakeField {
    pub id: Uuid,
    pub label: String,
    /// Help text shown with the field; may be empty.
    pub guidance: String,
    pub kind: FieldKind,
    /// Required while visible. Hidden fields are never required.
    pub required: bool,
    /// `select` only: a free-text answer may be given instead of a choice.
    pub allow_other: bool,
    /// The categories whose selection reveals this field.
    pub category_ids: Vec<Uuid>,
    /// `select` only, in display order.
    pub choices: Vec<Choice>,
    pub archived: bool,
}

/// A select option. Its ID is stable: renaming keeps it, and it is archived, never deleted.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Choice {
    pub id: Uuid,
    pub label: String,
    pub archived: bool,
}

/// Everything in display order.
#[derive(Debug, Serialize, ToSchema)]
pub struct Catalog {
    pub categories: Vec<Category>,
    pub priorities: Vec<Priority>,
    pub fields: Vec<IntakeField>,
}

/// `all` includes archived rows (Admin view). Without it, fields no active
/// category reveals are left out: they cannot appear on the form.
async fn load(db: &mut PgConnection, all: bool) -> Result<Catalog, ApiError> {
    let categories = sqlx::query_as(
        "SELECT id, name, skip_ai, archived_at IS NOT NULL AS archived FROM categories
         WHERE $1 OR archived_at IS NULL ORDER BY sort_order, created_at, id",
    )
    .bind(all)
    .fetch_all(&mut *db)
    .await?;
    let priorities = sqlx::query_as(
        "SELECT id, name, archived_at IS NOT NULL AS archived FROM priorities
         WHERE $1 OR archived_at IS NULL ORDER BY sort_order, created_at, id",
    )
    .bind(all)
    .fetch_all(&mut *db)
    .await?;
    let mut fields = load_fields(db, all, None).await?;
    if !all {
        fields.retain(|f| !f.category_ids.is_empty());
    }
    Ok(Catalog {
        categories,
        priorities,
        fields,
    })
}

async fn load_fields(
    db: &mut PgConnection,
    all: bool,
    only: Option<Uuid>,
) -> Result<Vec<IntakeField>, ApiError> {
    type Row = (Uuid, String, String, String, bool, bool, Vec<Uuid>, bool);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT f.id, f.label, f.guidance, f.kind, f.required, f.allow_other,
                ARRAY(SELECT c.id FROM intake_field_categories fc
                      JOIN categories c ON c.id = fc.category_id
                      WHERE fc.field_id = f.id AND ($1 OR c.archived_at IS NULL)
                      ORDER BY c.sort_order, c.created_at, c.id),
                f.archived_at IS NOT NULL
         FROM intake_fields f
         WHERE ($1 OR f.archived_at IS NULL) AND ($2::uuid IS NULL OR f.id = $2)
         ORDER BY f.sort_order, f.created_at, f.id",
    )
    .bind(all)
    .bind(only)
    .fetch_all(&mut *db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let choices: Vec<(Uuid, Uuid, String, bool)> = sqlx::query_as(
        "SELECT field_id, id, label, archived_at IS NOT NULL FROM intake_field_choices
         WHERE field_id = ANY($1) AND ($2 OR archived_at IS NULL)
         ORDER BY archived_at IS NOT NULL, sort_order, created_at, id",
    )
    .bind(&ids)
    .bind(all)
    .fetch_all(&mut *db)
    .await?;
    let mut by_field: HashMap<Uuid, Vec<Choice>> = HashMap::new();
    for (field_id, id, label, archived) in choices {
        by_field.entry(field_id).or_default().push(Choice {
            id,
            label,
            archived,
        });
    }
    rows.into_iter()
        .map(|r| {
            Ok(IntakeField {
                id: r.0,
                label: r.1,
                guidance: r.2,
                kind: ident(r.3)?,
                required: r.4,
                allow_other: r.5,
                category_ids: r.6,
                choices: by_field.remove(&r.0).unwrap_or_default(),
                archived: r.7,
            })
        })
        .collect()
}

/// The active catalog for the shared ticket form: unarchived categories,
/// priorities, fields, and choices in display order. Any signed-in account.
#[utoipa::path(get, path = "/api/catalog", tag = "catalog",
    responses(
        (status = 200, body = Catalog),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn active_catalog(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Catalog>, ApiError> {
    require_permission(&user, Permission::SubmitTicket)?;
    Ok(Json(load(&mut *state.pool.acquire().await?, false).await?))
}

/// The whole catalog including archived entries (Admin only).
#[utoipa::path(get, path = "/api/catalog/all", tag = "catalog",
    responses(
        (status = 200, body = Catalog),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn full_catalog(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Catalog>, ApiError> {
    require_permission(&user, Permission::ManageCategories)?;
    Ok(Json(load(&mut *state.pool.acquire().await?, true).await?))
}

/// Trims `value` and records `required` / `too_long` under `field`.
fn text(errors: &mut FieldErrors, field: &str, value: &str, max: usize, required: bool) -> String {
    let value = value.trim();
    if required && value.is_empty() {
        errors.entry(field.into()).or_default().push("required");
    } else if value.chars().count() > max {
        errors.entry(field.into()).or_default().push("too_long");
    }
    value.to_owned()
}

fn check(errors: FieldErrors) -> Result<(), ApiError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

async fn audit_item(
    db: &mut PgConnection,
    user: &CurrentUser,
    event: &str,
    kind: &str,
    item: impl Serialize,
) -> Result<(), ApiError> {
    let metadata = json!({ "kind": kind, "item": item });
    Ok(audit(db, event, Some(user.account_id()), None, metadata).await?)
}

// ----------------------------------------------------------------- categories

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCategory {
    /// 1-100 characters, unique among active categories.
    pub name: String,
    /// Default `false`: follow the organization AI policy.
    pub skip_ai: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryUpdate {
    pub name: Option<String>,
    pub skip_ai: Option<bool>,
    /// `true` hides it from new tickets; `false` restores it. Existing tickets are unaffected.
    pub archived: Option<bool>,
}

/// Adds a category at the end of the order (Admin only).
#[utoipa::path(post, path = "/api/categories", tag = "catalog", request_body = NewCategory,
    responses(
        (status = 201, body = Category),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`name`: `required`, `too_long`, `taken`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn create_category(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewCategory>,
) -> Result<(StatusCode, Json<Category>), ApiError> {
    require_permission(&user, Permission::ManageCategories)?;
    let mut errors = FieldErrors::new();
    let name = text(&mut errors, "name", &body.name, NAME_MAX, true);
    check(errors)?;
    let mut tx = state.pool.begin().await?;
    let category: Category = sqlx::query_as(
        "INSERT INTO categories (name, skip_ai, sort_order)
         VALUES ($1, $2, (SELECT COALESCE(max(sort_order) + 1, 0) FROM categories))
         RETURNING id, name, skip_ai, false AS archived",
    )
    .bind(name)
    .bind(body.skip_ai.unwrap_or(false))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| unique_violation(e, "name"))?;
    audit_item(
        &mut tx,
        &user,
        "catalog_item_created",
        "category",
        &category,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(category)))
}

/// Renames, sets "Skip AI automatically", archives, or restores a category
/// (Admin only). Submitted tickets keep their snapshot of the category.
#[utoipa::path(patch, path = "/api/categories/{id}", tag = "catalog", request_body = CategoryUpdate,
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = Category),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`name`: `required`, `too_long`, `taken`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn update_category(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CategoryUpdate>,
) -> Result<Json<Category>, ApiError> {
    require_permission(&user, Permission::ManageCategories)?;
    let mut errors = FieldErrors::new();
    let name = body
        .name
        .map(|n| text(&mut errors, "name", &n, NAME_MAX, true));
    check(errors)?;
    let mut tx = state.pool.begin().await?;
    let category: Category = sqlx::query_as(&format!(
        "UPDATE categories SET name = COALESCE($3, name), skip_ai = COALESCE($4, skip_ai), {ARCHIVE}
         WHERE id = $1 RETURNING id, name, skip_ai, archived_at IS NOT NULL AS archived"
    ))
    .bind(id)
    .bind(body.archived)
    .bind(name)
    .bind(body.skip_ai)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| unique_violation(e, "name"))?
    .ok_or(ApiError::NotFound)?;
    audit_item(
        &mut tx,
        &user,
        "catalog_item_updated",
        "category",
        &category,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(category))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Order {
    /// Every ID of this kind, archived ones included, in the new display order.
    pub ids: Vec<Uuid>,
}

/// Sets the display order of all categories (Admin only).
#[utoipa::path(put, path = "/api/categories/order", tag = "catalog", request_body = Order,
    responses(
        (status = 204, description = "Reordered"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict`: the IDs are not exactly the current categories (reload)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn reorder_categories(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<Order>,
) -> Result<StatusCode, ApiError> {
    reorder(&state, &user, "categories", "category", body.ids).await
}

/// `table` is one of the catalog tables, never client input.
async fn reorder(
    state: &AppState,
    user: &CurrentUser,
    table: &str,
    kind: &str,
    ids: Vec<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_permission(user, Permission::ManageCategories)?;
    let mut tx = state.pool.begin().await?;
    // Row locks make a concurrent reorder wait, then compare against the same set.
    let mut current: Vec<Uuid> = sqlx::query_scalar(&format!("SELECT id FROM {table} FOR UPDATE"))
        .fetch_all(&mut *tx)
        .await?;
    let mut given = ids.clone();
    current.sort();
    given.sort();
    if given != current {
        return Err(ApiError::Conflict);
    }
    sqlx::query(&format!(
        "UPDATE {table} SET sort_order = array_position($1, id) WHERE id = ANY($1)"
    ))
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    let metadata = json!({ "kind": kind, "ids": ids });
    audit(
        &mut *tx,
        "catalog_reordered",
        Some(user.account_id()),
        None,
        metadata,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----------------------------------------------------------------- priorities

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPriority {
    /// 1-100 characters, unique among active priorities.
    pub name: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PriorityUpdate {
    pub name: Option<String>,
    /// `true` hides it from new tickets; `false` restores it. Existing tickets are unaffected.
    pub archived: Option<bool>,
}

/// Adds a priority level at the end of the order (Admin only).
#[utoipa::path(post, path = "/api/priorities", tag = "catalog", request_body = NewPriority,
    responses(
        (status = 201, body = Priority),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`name`: `required`, `too_long`, `taken`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn create_priority(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewPriority>,
) -> Result<(StatusCode, Json<Priority>), ApiError> {
    require_permission(&user, Permission::ManageCategories)?;
    let mut errors = FieldErrors::new();
    let name = text(&mut errors, "name", &body.name, NAME_MAX, true);
    check(errors)?;
    let mut tx = state.pool.begin().await?;
    let priority: Priority = sqlx::query_as(
        "INSERT INTO priorities (name, sort_order)
         VALUES ($1, (SELECT COALESCE(max(sort_order) + 1, 0) FROM priorities))
         RETURNING id, name, false AS archived",
    )
    .bind(name)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| unique_violation(e, "name"))?;
    audit_item(
        &mut tx,
        &user,
        "catalog_item_created",
        "priority",
        &priority,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(priority)))
}

/// Renames, archives, or restores a priority level (Admin only).
#[utoipa::path(patch, path = "/api/priorities/{id}", tag = "catalog", request_body = PriorityUpdate,
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = Priority),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`name`: `required`, `too_long`, `taken`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn update_priority(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PriorityUpdate>,
) -> Result<Json<Priority>, ApiError> {
    require_permission(&user, Permission::ManageCategories)?;
    let mut errors = FieldErrors::new();
    let name = body
        .name
        .map(|n| text(&mut errors, "name", &n, NAME_MAX, true));
    check(errors)?;
    let mut tx = state.pool.begin().await?;
    let priority: Priority = sqlx::query_as(&format!(
        "UPDATE priorities SET name = COALESCE($3, name), {ARCHIVE}
         WHERE id = $1 RETURNING id, name, archived_at IS NOT NULL AS archived"
    ))
    .bind(id)
    .bind(body.archived)
    .bind(name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| unique_violation(e, "name"))?
    .ok_or(ApiError::NotFound)?;
    audit_item(
        &mut tx,
        &user,
        "catalog_item_updated",
        "priority",
        &priority,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(priority))
}

/// Sets the display order of all priority levels (Admin only).
#[utoipa::path(put, path = "/api/priorities/order", tag = "catalog", request_body = Order,
    responses(
        (status = 204, description = "Reordered"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict`: the IDs are not exactly the current priorities (reload)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn reorder_priorities(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<Order>,
) -> Result<StatusCode, ApiError> {
    reorder(&state, &user, "priorities", "priority", body.ids).await
}

// ------------------------------------------------------------- intake fields

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChoiceInput {
    /// An existing choice of this field to update; omit to add a new choice.
    pub id: Option<Uuid>,
    /// 1-100 characters, unique among the field's active choices.
    pub label: String,
    pub archived: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewIntakeField {
    /// 1-100 characters.
    pub label: String,
    /// Up to 1000 characters.
    pub guidance: Option<String>,
    /// Fixed after creation.
    pub kind: FieldKind,
    pub required: Option<bool>,
    /// `select` only.
    pub allow_other: Option<bool>,
    /// The categories that reveal this field.
    pub category_ids: Vec<Uuid>,
    /// `select` only, in display order (new choices, so without `id`).
    pub choices: Option<Vec<ChoiceInput>>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IntakeFieldUpdate {
    pub label: Option<String>,
    pub guidance: Option<String>,
    pub required: Option<bool>,
    pub allow_other: Option<bool>,
    /// Replaces the revealing categories.
    pub category_ids: Option<Vec<Uuid>>,
    /// The complete choice list in display order. Existing choices left out are
    /// archived, never deleted, so their IDs stay valid for existing tickets.
    pub choices: Option<Vec<ChoiceInput>>,
    pub archived: Option<bool>,
}

/// Field-level checks that need no database; returns the trimmed labels.
fn check_choices(
    errors: &mut FieldErrors,
    kind: FieldKind,
    choices: &[ChoiceInput],
) -> Vec<String> {
    if kind == FieldKind::Text && !choices.is_empty() {
        errors.entry("choices".into()).or_default().push("invalid");
    }
    if choices.len() > CHOICES_MAX {
        errors.entry("choices".into()).or_default().push("too_many");
    }
    let (mut labels, mut ids) = (HashSet::new(), HashSet::new());
    let mut trimmed = Vec::with_capacity(choices.len());
    for (i, c) in choices.iter().enumerate() {
        let path = format!("choices.{i}.label");
        let label = text(errors, &path, &c.label, NAME_MAX, true);
        if !c.archived.unwrap_or(false) && !labels.insert(label.to_lowercase()) {
            errors.entry(path).or_default().push("taken");
        }
        if c.id.is_some_and(|id| !ids.insert(id)) {
            errors
                .entry(format!("choices.{i}.id"))
                .or_default()
                .push("invalid");
        }
        trimmed.push(label);
    }
    trimmed
}

/// Replaces the revealing categories; unknown category IDs are `invalid`.
async fn set_categories(
    db: &mut PgConnection,
    errors: &mut FieldErrors,
    field_id: Uuid,
    ids: &[Uuid],
) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM intake_field_categories WHERE field_id = $1")
        .bind(field_id)
        .execute(&mut *db)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO intake_field_categories (field_id, category_id)
         SELECT $1, id FROM categories WHERE id = ANY($2)",
    )
    .bind(field_id)
    .bind(ids)
    .execute(&mut *db)
    .await?
    .rows_affected();
    if inserted as usize != ids.iter().collect::<HashSet<_>>().len() {
        errors
            .entry("category_ids".into())
            .or_default()
            .push("invalid");
    }
    Ok(())
}

/// Applies the complete choice list; existing choices not in it are archived.
async fn set_choices(
    db: &mut PgConnection,
    errors: &mut FieldErrors,
    field_id: Uuid,
    choices: &[ChoiceInput],
    labels: &[String],
) -> Result<(), ApiError> {
    let mut keep: Vec<Uuid> = Vec::with_capacity(choices.len());
    for (i, (c, label)) in choices.iter().zip(labels).enumerate() {
        let archived = c.archived.unwrap_or(false);
        let id: Option<Uuid> = match c.id {
            Some(id) => {
                sqlx::query_scalar(
                    "UPDATE intake_field_choices SET label = $3, sort_order = $4,
                        archived_at = CASE WHEN $5 THEN COALESCE(archived_at, now()) END
                 WHERE id = $1 AND field_id = $2 RETURNING id",
                )
                .bind(id)
                .bind(field_id)
                .bind(label)
                .bind(i as i32)
                .bind(archived)
                .fetch_optional(&mut *db)
                .await?
            }
            None => Some(
                sqlx::query_scalar(
                    "INSERT INTO intake_field_choices (field_id, label, sort_order, archived_at)
                     VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END) RETURNING id",
                )
                .bind(field_id)
                .bind(label)
                .bind(i as i32)
                .bind(archived)
                .fetch_one(&mut *db)
                .await?,
            ),
        };
        match id {
            Some(id) => keep.push(id),
            None => errors
                .entry(format!("choices.{i}.id"))
                .or_default()
                .push("invalid"),
        }
    }
    sqlx::query(
        "UPDATE intake_field_choices SET archived_at = now()
         WHERE field_id = $1 AND archived_at IS NULL AND id <> ALL($2)",
    )
    .bind(field_id)
    .bind(&keep)
    .execute(&mut *db)
    .await?;
    Ok(())
}

/// A select field needs an active choice or the free-text alternative to be answerable.
async fn check_answerable(
    db: &mut PgConnection,
    errors: &mut FieldErrors,
    field_id: Uuid,
) -> Result<(), ApiError> {
    let answerable: bool = sqlx::query_scalar(
        "SELECT kind = 'text' OR allow_other OR EXISTS (
                    SELECT 1 FROM intake_field_choices c WHERE c.field_id = f.id AND c.archived_at IS NULL)
         FROM intake_fields f WHERE id = $1",
    )
    .bind(field_id)
    .fetch_one(&mut *db)
    .await?;
    if !answerable {
        errors.entry("choices".into()).or_default().push("required");
    }
    Ok(())
}

async fn load_field(db: &mut PgConnection, id: Uuid) -> Result<IntakeField, ApiError> {
    load_fields(db, true, Some(id))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)
}

/// Adds a conditional field at the end of the order (Admin only). A `select`
/// field needs at least one choice or `allow_other`.
#[utoipa::path(post, path = "/api/intake-fields", tag = "catalog", request_body = NewIntakeField,
    responses(
        (status = 201, body = IntakeField),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`label`, `guidance`, `allow_other`, `category_ids`, `choices`, `choices.N.label`, `choices.N.id`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn create_intake_field(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<NewIntakeField>,
) -> Result<(StatusCode, Json<IntakeField>), ApiError> {
    require_permission(&user, Permission::ManageCategories)?;
    let mut errors = FieldErrors::new();
    let label = text(&mut errors, "label", &body.label, NAME_MAX, true);
    let guidance = body.guidance.as_deref().unwrap_or_default();
    let guidance = text(&mut errors, "guidance", guidance, GUIDANCE_MAX, false);
    let allow_other = body.allow_other.unwrap_or(false);
    if allow_other && body.kind == FieldKind::Text {
        errors
            .entry("allow_other".into())
            .or_default()
            .push("invalid");
    }
    let choices = body.choices.unwrap_or_default();
    let labels = check_choices(&mut errors, body.kind, &choices);
    check(errors)?;

    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO intake_fields (label, guidance, kind, required, allow_other, sort_order)
         VALUES ($1, $2, $3, $4, $5, (SELECT COALESCE(max(sort_order) + 1, 0) FROM intake_fields))
         RETURNING id",
    )
    .bind(label)
    .bind(guidance)
    .bind(crate::auth::ident_str(body.kind))
    .bind(body.required.unwrap_or(false))
    .bind(allow_other)
    .fetch_one(&mut *tx)
    .await?;
    let mut errors = FieldErrors::new();
    set_categories(&mut tx, &mut errors, id, &body.category_ids).await?;
    set_choices(&mut tx, &mut errors, id, &choices, &labels).await?;
    check_answerable(&mut tx, &mut errors, id).await?;
    check(errors)?;
    let field = load_field(&mut tx, id).await?;
    audit_item(
        &mut tx,
        &user,
        "catalog_item_created",
        "intake_field",
        &field,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(field)))
}

/// Edits, archives, or restores a conditional field, its choices, and the
/// categories that reveal it (Admin only). The kind cannot change.
#[utoipa::path(patch, path = "/api/intake-fields/{id}", tag = "catalog", request_body = IntakeFieldUpdate,
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = IntakeField),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`label`, `guidance`, `allow_other`, `category_ids`, `choices`, `choices.N.label`, `choices.N.id`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn update_intake_field(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<IntakeFieldUpdate>,
) -> Result<Json<IntakeField>, ApiError> {
    require_permission(&user, Permission::ManageCategories)?;
    let mut tx = state.pool.begin().await?;
    // Locked: concurrent edits of one field apply one after another.
    let kind: String =
        sqlx::query_scalar("SELECT kind FROM intake_fields WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
    let kind: FieldKind = ident(kind)?;
    let mut errors = FieldErrors::new();
    let label = body
        .label
        .map(|l| text(&mut errors, "label", &l, NAME_MAX, true));
    let guidance = body
        .guidance
        .map(|g| text(&mut errors, "guidance", &g, GUIDANCE_MAX, false));
    if body.allow_other == Some(true) && kind == FieldKind::Text {
        errors
            .entry("allow_other".into())
            .or_default()
            .push("invalid");
    }
    let labels = body
        .choices
        .as_deref()
        .map(|c| check_choices(&mut errors, kind, c));
    check(errors)?;

    sqlx::query(&format!(
        "UPDATE intake_fields SET label = COALESCE($3, label), guidance = COALESCE($4, guidance),
                required = COALESCE($5, required), allow_other = COALESCE($6, allow_other), {ARCHIVE}
         WHERE id = $1"
    ))
    .bind(id)
    .bind(body.archived)
    .bind(label)
    .bind(guidance)
    .bind(body.required)
    .bind(body.allow_other)
    .execute(&mut *tx)
    .await?;
    let mut errors = FieldErrors::new();
    if let Some(ids) = &body.category_ids {
        set_categories(&mut tx, &mut errors, id, ids).await?;
    }
    if let (Some(choices), Some(labels)) = (&body.choices, &labels) {
        set_choices(&mut tx, &mut errors, id, choices, labels).await?;
    }
    check_answerable(&mut tx, &mut errors, id).await?;
    check(errors)?;
    let field = load_field(&mut tx, id).await?;
    audit_item(
        &mut tx,
        &user,
        "catalog_item_updated",
        "intake_field",
        &field,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(field))
}

/// Sets the display order of all conditional fields (Admin only).
#[utoipa::path(put, path = "/api/intake-fields/order", tag = "catalog", request_body = Order,
    responses(
        (status = 204, description = "Reordered"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not an Admin)", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`conflict`: the IDs are not exactly the current fields (reload)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn reorder_intake_fields(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<Order>,
) -> Result<StatusCode, ApiError> {
    reorder(&state, &user, "intake_fields", "intake_field", body.ids).await
}

// ---------------------------------------------------------- intake validation

/// One conditional answer on the shared form: the `choice_id` of a `select`
/// field, or `text` (a `text` field, or a `select` field's free-text alternative).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeAnswer {
    pub choice_id: Option<Uuid>,
    pub text: Option<String>,
}

/// What a submitted ticket stores: names, labels, and answers as they were at
/// submission, so later edits and archival never rewrite history, and the AI
/// route (`skip_ai`) is fixed for the whole attempt.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeSnapshot {
    pub category_id: Uuid,
    pub category_name: String,
    pub skip_ai: bool,
    pub priority_id: Uuid,
    pub priority_name: String,
    /// Answers to fields visible for the category, in display order.
    pub answers: Vec<AnswerSnapshot>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerSnapshot {
    pub field_id: Uuid,
    pub label: String,
    /// Set when a listed choice was picked; `None` for typed text.
    pub choice_id: Option<Uuid>,
    /// The choice label or the typed text.
    pub value: String,
}

/// Validates a requester's selections against the active catalog, for ticket
/// submission. Unknown or archived category, priority, and choice IDs are
/// `invalid`; answers keyed by field IDs that do not exist are `unknown_field`;
/// a required visible field without an answer is `required`. Answers to fields
/// hidden for this category or archived are dropped: they never reach the
/// committed intake or AI input. Error paths: `category_id`, `priority_id`,
/// `answers.<field_id>`.
pub async fn validate_intake(
    db: &mut PgConnection,
    category_id: Uuid,
    priority_id: Uuid,
    answers: &BTreeMap<Uuid, IntakeAnswer>,
) -> Result<IntakeSnapshot, ApiError> {
    let mut errors = FieldErrors::new();
    let category: Option<(String, bool)> = sqlx::query_as(
        "SELECT name, skip_ai FROM categories WHERE id = $1 AND archived_at IS NULL",
    )
    .bind(category_id)
    .fetch_optional(&mut *db)
    .await?;
    let priority: Option<String> =
        sqlx::query_scalar("SELECT name FROM priorities WHERE id = $1 AND archived_at IS NULL")
            .bind(priority_id)
            .fetch_optional(&mut *db)
            .await?;
    if category.is_none() {
        errors.insert("category_id".into(), vec!["invalid"]);
    }
    if priority.is_none() {
        errors.insert("priority_id".into(), vec!["invalid"]);
    }
    let ids: Vec<Uuid> = answers.keys().copied().collect();
    let known: HashSet<Uuid> =
        sqlx::query_scalar("SELECT id FROM intake_fields WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&mut *db)
            .await?
            .into_iter()
            .collect();
    for id in ids.iter().filter(|id| !known.contains(id)) {
        errors.insert(format!("answers.{id}"), vec!["unknown_field"]);
    }

    type Visible = (Uuid, String, String, bool, bool);
    let visible: Vec<Visible> = sqlx::query_as(
        "SELECT f.id, f.label, f.kind, f.required, f.allow_other
         FROM intake_fields f JOIN intake_field_categories fc ON fc.field_id = f.id
         WHERE fc.category_id = $1 AND f.archived_at IS NULL
         ORDER BY f.sort_order, f.created_at, f.id",
    )
    .bind(category.is_some().then_some(category_id))
    .fetch_all(&mut *db)
    .await?;
    let mut snapshot = Vec::new();
    for (field_id, label, kind, required, allow_other) in visible {
        let answer = answers.get(&field_id).cloned().unwrap_or_default();
        let typed = answer
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let kind: FieldKind = ident(kind)?;
        let result = match (answer.choice_id, typed) {
            (Some(_), Some(_)) => Err("invalid"),
            (Some(_), None) if kind == FieldKind::Text => Err("invalid"),
            (Some(choice), None) => sqlx::query_scalar(
                "SELECT label FROM intake_field_choices
                 WHERE id = $1 AND field_id = $2 AND archived_at IS NULL",
            )
            .bind(choice)
            .bind(field_id)
            .fetch_optional(&mut *db)
            .await?
            .map(|value| Some((Some(choice), value)))
            .ok_or("invalid"),
            (None, Some(_)) if kind == FieldKind::Select && !allow_other => Err("invalid"),
            (None, Some(t)) if t.chars().count() > ANSWER_MAX => Err("too_long"),
            (None, Some(t)) => Ok(Some((None, t.to_owned()))),
            (None, None) if required => Err("required"),
            (None, None) => Ok(None),
        };
        match result {
            Ok(Some((choice_id, value))) => snapshot.push(AnswerSnapshot {
                field_id,
                label,
                choice_id,
                value,
            }),
            Ok(None) => {}
            Err(code) => {
                errors.insert(format!("answers.{field_id}"), vec![code]);
            }
        }
    }
    check(errors)?;
    let (Some((category_name, skip_ai)), Some(priority_name)) = (category, priority) else {
        unreachable!("missing category or priority is a validation error above");
    };
    Ok(IntakeSnapshot {
        category_id,
        category_name,
        skip_ai,
        priority_id,
        priority_name,
        answers: snapshot,
    })
}
