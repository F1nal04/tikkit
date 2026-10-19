//! Admin account/team edits and organization policy, extending the foundation schema.
use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json, Language, Page, Pagination, Path, Query, Role},
    auth::{CurrentUser, audit, ident, ident_str},
    authz::{Permission, require_permission},
};
use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) fn invalid(field: &str) -> ApiError {
    ApiError::Validation(FieldErrors::from([(field.into(), vec!["invalid"])]))
}

/// Share the last-admin lock before taking row locks; recheck the actor after waiting.
/// Admin edits are rare and serializing them keeps multi-row role/team changes atomic.
pub(crate) async fn transaction<'a>(
    state: &'a AppState,
    user: &CurrentUser,
) -> Result<Transaction<'a, Postgres>, ApiError> {
    require_permission(user, Permission::ManageAccounts)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('tikkit.active_admins'))")
        .execute(&mut *tx)
        .await?;
    let allowed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM accounts WHERE id = $1 AND role = 'admin' AND is_active)",
    )
    .bind(user.account_id())
    .fetch_one(&mut *tx)
    .await?;
    if !allowed {
        return Err(ApiError::Forbidden);
    }
    Ok(tx)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAccountRequest {
    pub role: Role,
    /// Descriptive only; only Specialist accounts may have expertise. Maximum 120 characters.
    pub expertise: Option<String>,
    /// One current team; null removes membership. Users/Admins must use null.
    pub team_id: Option<Uuid>,
}

#[utoipa::path(put, path = "/api/accounts/{id}", tag = "accounts", params(("id" = Uuid, Path)), request_body = UpdateAccountRequest,
    responses((status = 204, description = "Updated atomically"),
        (status = 401, body = crate::api::ErrorEnvelope), (status = 403, body = crate::api::ErrorEnvelope),
        (status = 404, body = crate::api::ErrorEnvelope), (status = 409, description = "last_active_admin, active_team_lead, conflict", body = crate::api::ErrorEnvelope),
        (status = 422, body = crate::api::ErrorEnvelope)))]
pub async fn update_account(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateAccountRequest>,
) -> Result<StatusCode, ApiError> {
    let mut tx = transaction(&state, &user).await?;
    let old: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT role::text, expertise FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (old_role, old_expertise) = old.ok_or(ApiError::NotFound)?;
    let expertise = body
        .expertise
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if expertise
        .as_ref()
        .is_some_and(|s| s.chars().count() > 120 || body.role != Role::Specialist)
    {
        return Err(invalid("expertise"));
    }
    // Deactivation keeps membership; joining a new team (set_membership) requires an active account.
    if body.team_id.is_some() && matches!(body.role, Role::User | Role::Admin) {
        return Err(invalid("team_id"));
    }
    let old_team: Option<Uuid> =
        sqlx::query_scalar("SELECT team_id FROM team_memberships WHERE account_id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let leads: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM teams WHERE lead_account_id = $1 AND archived_at IS NULL)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if leads && (body.role != Role::TeamLead || body.team_id != old_team) {
        return Err(ApiError::ActiveTeamLead);
    }
    if body.team_id != old_team {
        // Remove before changing role: the existing role-carrying FK rejects Admin/User members.
        set_membership(&mut tx, &user, id, None).await?;
    }
    sqlx::query("UPDATE accounts SET role = $2, expertise = $3 WHERE id = $1")
        .bind(id)
        .bind(ident_str(body.role))
        .bind(&expertise)
        .execute(&mut *tx)
        .await?;
    if body.team_id != old_team {
        set_membership(&mut tx, &user, id, body.team_id).await?;
    }
    if old_role != ident_str(body.role) || old_expertise != expertise {
        audit(&mut *tx, "account_updated", Some(user.account_id()), Some(id), json!({"from_role":old_role,"to_role":body.role,"from_expertise":old_expertise,"to_expertise":expertise})).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Clear assignment before removing eligibility, retaining tickets and attributable history.
pub(crate) async fn clear_assignments(
    db: &mut PgConnection,
    user: &CurrentUser,
    account: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query("WITH changed AS (
        UPDATE tickets SET assignee_account_id = NULL, updated_at = now() WHERE assignee_account_id = $1 RETURNING id
    ) INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, payload)
      SELECT id, $2, 'assignment_changed', jsonb_build_object('previous_assignee_account_id', $1::uuid, 'assignee_account_id', NULL, 'reason', $3::text) FROM changed")
        .bind(account).bind(user.account_id()).bind(reason).execute(db).await?;
    Ok(())
}

async fn set_membership(
    db: &mut PgConnection,
    user: &CurrentUser,
    account: Uuid,
    team: Option<Uuid>,
) -> Result<(), ApiError> {
    // Account, then membership and its team, before any ticket. Assignment and
    // lifecycle take those locks before the ticket row; clearing tickets first
    // deadlocks, and would miss an assignee committed while this waited.
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM accounts WHERE id = $1 FOR UPDATE")
        .bind(account)
        .fetch_optional(&mut *db)
        .await?;
    let old: Option<Uuid> = sqlx::query_scalar(
        "SELECT m.team_id FROM team_memberships m
         JOIN teams t ON t.id = m.team_id
         WHERE m.account_id = $1
         FOR UPDATE OF m, t",
    )
    .bind(account)
    .fetch_optional(&mut *db)
    .await?;
    if old == team {
        return Ok(());
    }
    let leads: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE lead_account_id = $1)")
            .bind(account)
            .fetch_one(&mut *db)
            .await?;
    if leads {
        return Err(ApiError::ActiveTeamLead);
    }
    if let Some(team) = team {
        let eligible: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM accounts a, teams t WHERE a.id = $1 AND a.is_active AND a.role NOT IN ('user','admin') AND t.id = $2 AND t.archived_at IS NULL)")
            .bind(account).bind(team).fetch_one(&mut *db).await?;
        if !eligible {
            return Err(invalid("team_id"));
        }
    }
    clear_assignments(db, user, account, "team_membership_changed").await?;
    sqlx::query("DELETE FROM team_memberships WHERE account_id = $1")
        .bind(account)
        .execute(&mut *db)
        .await?;
    if let Some(team) = team {
        sqlx::query("INSERT INTO team_memberships (account_id, role, team_id) SELECT id, role, $2 FROM accounts WHERE id = $1")
            .bind(account).bind(team).execute(&mut *db).await?;
    }
    audit(
        &mut *db,
        "team_membership_changed",
        Some(user.account_id()),
        Some(account),
        json!({"from_team_id":old,"to_team_id":team}),
    )
    .await?;
    Ok(())
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Team {
    pub id: Uuid,
    pub name: String,
    #[schema(required = true)]
    pub lead_account_id: Option<Uuid>,
    pub archived: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}
const TEAM_COLUMNS: &str =
    "id, name, lead_account_id, archived_at IS NOT NULL AS archived, created_at";
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTeamRequest {
    pub name: String,
    pub lead_account_id: Uuid,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateTeamRequest {
    pub name: String,
    /// Required for active teams; null when archiving. A lead must be active with team_lead role.
    pub lead_account_id: Option<Uuid>,
    /// Archiving clears leadership, memberships and assignments, retaining all history.
    pub archived: bool,
}
fn team_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(invalid("name"));
    }
    Ok(name)
}
async fn check_lead(db: &mut PgConnection, id: Uuid, team: Option<Uuid>) -> Result<(), ApiError> {
    let eligible: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM accounts a WHERE a.id = $1 AND a.role = 'team_lead' AND a.is_active AND NOT EXISTS (SELECT 1 FROM teams t WHERE t.lead_account_id = a.id AND ($2::uuid IS NULL OR t.id <> $2)))")
        .bind(id).bind(team).fetch_one(db).await?;
    if !eligible {
        return Err(invalid("lead_account_id"));
    }
    Ok(())
}
fn team_error(error: sqlx::Error) -> ApiError {
    if error.as_database_error().and_then(|e| e.constraint()) == Some("teams_name_key") {
        invalid("name")
    } else {
        error.into()
    }
}

#[utoipa::path(get, path = "/api/teams", tag = "teams", params(Pagination), responses((status = 200, body = Page<Team>), (status = 401, body = crate::api::ErrorEnvelope), (status = 403, body = crate::api::ErrorEnvelope)))]
pub async fn list_teams(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(page): Query<Pagination>,
) -> Result<Json<Page<Team>>, ApiError> {
    require_permission(&user, Permission::ManageTeams)?;
    let cursor = page.cursor()?;
    let rows = sqlx::query_as::<_, Team>(&format!("SELECT {TEAM_COLUMNS} FROM teams WHERE ($2::timestamptz IS NULL OR (created_at,id) < ($2,$3)) ORDER BY created_at DESC,id DESC LIMIT $1"))
        .bind(page.limit()+1).bind(cursor.map(|c|c.created_at)).bind(cursor.map(|c|c.id)).fetch_all(&state.pool).await?;
    Ok(Json(Page::from_rows(rows, page.limit(), |t| {
        crate::api::Cursor {
            created_at: t.created_at,
            id: t.id,
        }
    })))
}

#[utoipa::path(post, path = "/api/teams", tag = "teams", request_body = CreateTeamRequest, responses((status = 201, body = Team), (status = 401, body = crate::api::ErrorEnvelope), (status = 403, body = crate::api::ErrorEnvelope), (status = 422, body = crate::api::ErrorEnvelope)))]
pub async fn create_team(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<CreateTeamRequest>,
) -> Result<(StatusCode, Json<Team>), ApiError> {
    let mut tx = transaction(&state, &user).await?;
    let name = team_name(&body.name)?;
    check_lead(&mut tx, body.lead_account_id, None).await?;
    // Candidate may move from another team, but never leave another team leaderless.
    set_membership(&mut tx, &user, body.lead_account_id, None).await?;
    let team: Team = sqlx::query_as(&format!(
        "INSERT INTO teams (name,lead_account_id) VALUES ($1,$2) RETURNING {TEAM_COLUMNS}"
    ))
    .bind(name)
    .bind(body.lead_account_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(team_error)?;
    // The team's deferred FK permits inserting its first lead membership here.
    sqlx::query("INSERT INTO team_memberships (account_id,role,team_id) SELECT id,role,$2 FROM accounts WHERE id=$1")
        .bind(body.lead_account_id).bind(team.id).execute(&mut *tx).await?;
    audit(
        &mut *tx,
        "team_membership_changed",
        Some(user.account_id()),
        Some(body.lead_account_id),
        json!({"from_team_id":null,"to_team_id":team.id}),
    )
    .await?;
    audit(
        &mut *tx,
        "team_created",
        Some(user.account_id()),
        None,
        json!({"team":team}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(team)))
}

#[utoipa::path(put, path = "/api/teams/{id}", tag = "teams", params(("id" = Uuid, Path)), request_body = UpdateTeamRequest, responses((status = 200, body = Team), (status = 401, body = crate::api::ErrorEnvelope), (status = 403, body = crate::api::ErrorEnvelope), (status = 404, body = crate::api::ErrorEnvelope), (status = 409, body = crate::api::ErrorEnvelope), (status = 422, body = crate::api::ErrorEnvelope)))]
pub async fn update_team(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateTeamRequest>,
) -> Result<Json<Team>, ApiError> {
    let mut tx = transaction(&state, &user).await?;
    let old: Team = sqlx::query_as(&format!("SELECT {TEAM_COLUMNS} FROM teams WHERE id=$1"))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    let name = team_name(&body.name)?;
    if body.archived {
        if body.lead_account_id.is_some() {
            return Err(invalid("lead_account_id"));
        }
        let members: Vec<Uuid> = sqlx::query_scalar(
            "SELECT account_id FROM team_memberships WHERE team_id=$1 ORDER BY account_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        // Accounts before the team, before tickets: assignment share-locks a
        // member and then their team.
        for member in &members {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM accounts WHERE id = $1 FOR UPDATE")
                .bind(member)
                .fetch_optional(&mut *tx)
                .await?;
        }
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM teams WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("UPDATE teams SET name=$2, archived_at=coalesce(archived_at,now()), lead_account_id=NULL,lead_role=NULL,lead_active=NULL WHERE id=$1")
            .bind(id).bind(name).execute(&mut *tx).await.map_err(team_error)?;
        for member in members {
            set_membership(&mut tx, &user, member, None).await?;
        }
    } else {
        let lead = body
            .lead_account_id
            .ok_or_else(|| invalid("lead_account_id"))?;
        check_lead(&mut tx, lead, Some(id)).await?;
        // The lead's account, then this team, before set_membership touches tickets.
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM accounts WHERE id = $1 FOR UPDATE")
            .bind(lead)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| invalid("lead_account_id"))?;
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM teams WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if old.archived {
            // Restore with the same deferred-FK sequence used for initial creation.
            set_membership(&mut tx, &user, lead, None).await?;
            sqlx::query("INSERT INTO team_memberships (account_id,role,team_id) SELECT id,role,$2 FROM accounts WHERE id=$1")
                .bind(lead).bind(id).execute(&mut *tx).await?;
            audit(
                &mut *tx,
                "team_membership_changed",
                Some(user.account_id()),
                Some(lead),
                json!({"from_team_id":null,"to_team_id":id}),
            )
            .await?;
        } else if old.lead_account_id != Some(lead) {
            set_membership(&mut tx, &user, lead, Some(id)).await?;
        }
        sqlx::query("UPDATE teams SET name=$2,archived_at=NULL,lead_account_id=$3,lead_role='team_lead',lead_active=true WHERE id=$1")
            .bind(id).bind(name).bind(lead).execute(&mut *tx).await.map_err(team_error)?;
    }
    let team: Team = sqlx::query_as(&format!("SELECT {TEAM_COLUMNS} FROM teams WHERE id=$1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    audit(
        &mut *tx,
        "team_updated",
        Some(user.account_id()),
        None,
        json!({"from":old,"to":team}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(team))
}

/// When the owning requester may cancel a submitted open ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancellationPolicy {
    AnytimeWhileOpen,
    /// Until the first recorded human work-start (`in_progress`) event.
    BeforeWorkStarts,
    Disabled,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationSettings {
    pub name: String,
    /// UTC or an IANA area/timezone name supported by PostgreSQL; timestamps remain UTC.
    pub timezone: String,
    pub default_language: Language,
    /// Human-closed tickets only. AI-solved records remain permanently non-reopenable.
    pub allow_reopening: bool,
    /// Before work starts uses the first human work-start event, never current assignment. Every cancellation requires a reason.
    pub requester_cancellation_policy: CancellationPolicy,
    /// Positive elapsed hours, default 168. Future proposals snapshot this policy; timeout closure is not requester acceptance.
    pub resolution_confirmation_hours: i32,
    pub allow_requester_ai_skip: bool,
}
pub async fn load_settings(
    db: impl sqlx::PgExecutor<'_>,
) -> Result<OrganizationSettings, ApiError> {
    let r: (String,String,String,bool,String,i32,bool) = sqlx::query_as("SELECT name,timezone,default_language,allow_reopening,requester_cancellation_policy,resolution_confirmation_hours,allow_requester_ai_skip FROM organization WHERE id").fetch_one(db).await?;
    Ok(OrganizationSettings {
        name: r.0,
        timezone: r.1,
        default_language: ident(r.2)?,
        allow_reopening: r.3,
        requester_cancellation_policy: ident(r.4)?,
        resolution_confirmation_hours: r.5,
        allow_requester_ai_skip: r.6,
    })
}
#[utoipa::path(get, path = "/api/organization", tag = "organization", responses((status = 200, body = OrganizationSettings), (status = 401, body = crate::api::ErrorEnvelope)))]
pub async fn organization(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<OrganizationSettings>, ApiError> {
    Ok(Json(load_settings(&state.pool).await?))
}
#[utoipa::path(get, path = "/api/admin/settings", tag = "organization", responses((status = 200, body = OrganizationSettings), (status = 401, body = crate::api::ErrorEnvelope), (status = 403, body = crate::api::ErrorEnvelope)))]
pub async fn settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<OrganizationSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    Ok(Json(load_settings(&state.pool).await?))
}
#[utoipa::path(put, path = "/api/admin/settings", tag = "organization", request_body = OrganizationSettings, responses((status = 200, body = OrganizationSettings), (status = 401, body = crate::api::ErrorEnvelope), (status = 403, body = crate::api::ErrorEnvelope), (status = 422, body = crate::api::ErrorEnvelope)))]
pub async fn update_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut body): Json<OrganizationSettings>,
) -> Result<Json<OrganizationSettings>, ApiError> {
    let mut tx = transaction(&state, &user).await?;
    let mut errors = FieldErrors::new();
    body.name = body.name.trim().to_owned();
    if body.name.is_empty() || body.name.chars().count() > 120 {
        errors.insert("name".into(), vec!["invalid"]);
    }
    let timezone: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_timezone_names WHERE name=$1)")
            .bind(&body.timezone)
            .fetch_one(&mut *tx)
            .await?;
    let standard_timezone = body.timezone == "UTC"
        || body.timezone.split_once('/').is_some_and(|(area, _)| {
            matches!(
                area,
                "Africa"
                    | "America"
                    | "Antarctica"
                    | "Arctic"
                    | "Asia"
                    | "Atlantic"
                    | "Australia"
                    | "Europe"
                    | "Indian"
                    | "Pacific"
                    | "Etc"
            )
        });
    if !timezone || !standard_timezone || body.timezone.len() > 100 {
        errors.insert("timezone".into(), vec!["invalid"]);
    }
    if body.resolution_confirmation_hours <= 0 {
        errors.insert("resolution_confirmation_hours".into(), vec!["invalid"]);
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let old = load_settings(&mut *tx).await?;
    sqlx::query("UPDATE organization SET name=$1,timezone=$2,default_language=$3,allow_reopening=$4,requester_cancellation_policy=$5,resolution_confirmation_hours=$6,allow_requester_ai_skip=$7 WHERE id")
        .bind(&body.name).bind(&body.timezone).bind(ident_str(body.default_language)).bind(body.allow_reopening).bind(ident_str(body.requester_cancellation_policy)).bind(body.resolution_confirmation_hours).bind(body.allow_requester_ai_skip).execute(&mut *tx).await?;
    audit(
        &mut *tx,
        "organization_settings_updated",
        Some(user.account_id()),
        None,
        json!({"from":old,"to":body}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(body))
}
