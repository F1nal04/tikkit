//! Saved views: an account's named queue filters. See docs/queues.md "Saved views".
//!
//! Only the owner lists or deletes them; another account's view is `404`. A view
//! stores filters, never results: applying one runs the normal queue query under
//! the caller's current visibility. Category, priority, and team references are
//! re-checked against the current catalog, teams, and the caller's role on every
//! listing and reported as `stale` instead of failing.

use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::types::Json as Jsonb;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json, Path, Role, TicketStatus},
    auth::{CurrentUser, ident_str, unique_violation},
    authz::{self, Actor, Permission},
    queues::{FILTERABLE, QueueSort, QueueView, search_terms},
};

/// Most saved views per account.
pub const VIEWS_MAX: i64 = 50;
const NAME_MAX: usize = 80;

/// The `GET /api/queue` filters a view applies (no cursor or page size).
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueFilters {
    pub view: Option<QueueView>,
    pub team_id: Option<Uuid>,
    pub status: Option<TicketStatus>,
    pub category_id: Option<Uuid>,
    pub priority_id: Option<Uuid>,
    pub unassigned: Option<bool>,
    pub sort: Option<QueueSort>,
    pub q: Option<String>,
}

/// A filter that no longer applies to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StaleFilter {
    /// The caller's role no longer has this view (`all` without Admin, `mine` as Admin).
    View,
    /// Archived, or not the caller's team (non-Admins), or set outside the `team` view.
    TeamId,
    /// Not a queue status.
    Status,
    /// Archived or unknown category.
    CategoryId,
    /// Archived or unknown priority.
    PriorityId,
    /// Search text out of bounds.
    Q,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SavedView {
    pub id: Uuid,
    pub name: String,
    pub filters: QueueFilters,
    /// Filters to drop when applying: the rest still work as saved.
    pub stale: Vec<StaleFilter>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSavedView {
    /// 1-80 characters after trimming, unique per account.
    pub name: String,
    pub filters: QueueFilters,
}

/// Active catalog and team IDs: small Admin-managed sets.
struct Live {
    teams: Vec<Uuid>,
    categories: Vec<Uuid>,
    priorities: Vec<Uuid>,
}

impl Live {
    async fn load(state: &AppState) -> Result<Self, ApiError> {
        let ids = |sql| sqlx::query_scalar(sql).fetch_all(&state.pool);
        Ok(Self {
            teams: ids("SELECT id FROM teams WHERE archived_at IS NULL").await?,
            categories: ids("SELECT id FROM categories WHERE archived_at IS NULL").await?,
            priorities: ids("SELECT id FROM priorities WHERE archived_at IS NULL").await?,
        })
    }
}

/// The filters the queue would reject or that point at archived catalog entries.
fn stale(actor: &Actor, f: &QueueFilters, live: &Live) -> Vec<StaleFilter> {
    let admin = actor.role == Role::Admin;
    let view = f.view.unwrap_or_default();
    let own_team = actor.team.map(|t| t.team_id);
    let mut out = vec![];
    if matches!(
        (view, admin),
        (QueueView::All, false) | (QueueView::Mine, true)
    ) {
        out.push(StaleFilter::View);
    }
    if f.team_id.is_some_and(|team| {
        view != QueueView::Team || !live.teams.contains(&team) || !(admin || own_team == Some(team))
    }) {
        out.push(StaleFilter::TeamId);
    }
    if f.status
        .is_some_and(|s| !FILTERABLE.contains(&ident_str(s).as_str()))
    {
        out.push(StaleFilter::Status);
    }
    if f.category_id.is_some_and(|c| !live.categories.contains(&c)) {
        out.push(StaleFilter::CategoryId);
    }
    if f.priority_id.is_some_and(|p| !live.priorities.contains(&p)) {
        out.push(StaleFilter::PriorityId);
    }
    if search_terms(f.q.as_deref()).is_err() {
        out.push(StaleFilter::Q);
    }
    out
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    filters: Jsonb<QueueFilters>,
    created_at: OffsetDateTime,
}

/// The caller's saved views, by name (at most 50, not paged).
#[utoipa::path(get, path = "/api/saved-views", tag = "queue",
    responses((status = 200, body = Vec<SavedView>),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not staff)", body = crate::api::ErrorEnvelope)))]
pub async fn list_views(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<SavedView>>, ApiError> {
    authz::require_permission(&user, Permission::ViewGeneralQueue)?;
    let actor = Actor::load(&state.pool, &user).await?;
    let live = Live::load(&state).await?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, name, filters, created_at FROM saved_views
         WHERE owner_account_id = $1 ORDER BY name, id",
    )
    .bind(actor.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| SavedView {
                stale: stale(&actor, &r.filters, &live),
                id: r.id,
                name: r.name,
                filters: r.filters.0,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

/// Saves the caller's current filters under a name. Every filter must apply now
/// (`422`, `filters.<name>: invalid`).
#[utoipa::path(post, path = "/api/saved-views", tag = "queue", request_body = CreateSavedView,
    responses((status = 201, body = SavedView),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden` (not staff)", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`name` is `required`, `too_long`, or `taken`; `saved_views: too_many`; or a filter is `invalid`", body = crate::api::ErrorEnvelope)))]
pub async fn create_view(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<CreateSavedView>,
) -> Result<(StatusCode, Json<SavedView>), ApiError> {
    authz::require_permission(&user, Permission::ViewGeneralQueue)?;
    let actor = Actor::load(&state.pool, &user).await?;
    let name = body.name.trim();
    let mut errors = FieldErrors::new();
    if name.is_empty() {
        errors.insert("name".into(), vec!["required"]);
    } else if name.chars().count() > NAME_MAX {
        errors.insert("name".into(), vec!["too_long"]);
    }
    let live = Live::load(&state).await?;
    for field in stale(&actor, &body.filters, &live) {
        errors.insert(format!("filters.{}", ident_str(field)), vec!["invalid"]);
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let filters = QueueFilters {
        q: body
            .filters
            .q
            .map(|q| q.trim().to_owned())
            .filter(|q| !q.is_empty()),
        ..body.filters
    };
    // ponytail: soft cap; two concurrent saves can pass the count together.
    let row: Option<Row> = sqlx::query_as(
        "INSERT INTO saved_views (owner_account_id, name, filters)
         SELECT $1, $2, $3 WHERE (SELECT count(*) FROM saved_views WHERE owner_account_id = $1) < $4
         RETURNING id, name, filters, created_at",
    )
    .bind(actor.id)
    .bind(name)
    .bind(Jsonb(&filters))
    .bind(VIEWS_MAX)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| unique_violation(e, "name"))?;
    let row = row.ok_or_else(|| {
        ApiError::Validation(FieldErrors::from([(
            "saved_views".into(),
            vec!["too_many"],
        )]))
    })?;
    Ok((
        StatusCode::CREATED,
        Json(SavedView {
            id: row.id,
            name: row.name,
            filters: row.filters.0,
            stale: vec![],
            created_at: row.created_at,
        }),
    ))
}

/// Deletes one of the caller's views; anyone else's is `404`.
#[utoipa::path(delete, path = "/api/saved-views/{id}", tag = "queue", params(("id" = Uuid, Path)),
    responses((status = 204),
        (status = 401, body = crate::api::ErrorEnvelope),
        (status = 404, body = crate::api::ErrorEnvelope)))]
pub async fn delete_view(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let deleted = sqlx::query("DELETE FROM saved_views WHERE id = $1 AND owner_account_id = $2")
        .bind(id)
        .bind(user.account_id())
        .execute(&state.pool)
        .await?
        .rows_affected();
    if deleted == 1 {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}
