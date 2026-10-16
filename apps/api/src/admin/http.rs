//! `/api/v1/admin` account directory and team endpoints (issue #11).
//!
//! Every handler requires a session (401 `unauthenticated`) and the Admin
//! role (403 `forbidden`). The [`Actor`] is reloaded from the database on
//! every request, so a demoted Admin loses access with its next request. The
//! services re-check the actor inside their transactions.
//!
//! Account deactivation/reactivation and invitations have their own
//! endpoints (#10).

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{
    accounts::{self, AccountCursor, AccountFilter, AdminAccount, RoleChange},
    teams::{self, PreviousLead, Team},
};
use crate::{
    AppState,
    auth::Actor,
    authz::{self, Principal},
    domain::{AccountStatus, Role},
    error::{ApiError, ErrorBody, FieldErrorCode},
    extract::{ApiJson, ApiPath, ApiQuery},
    pagination::{Page, PageParams},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/accounts", get(list_accounts))
        .route("/api/v1/admin/accounts/{id}", get(get_account))
        .route("/api/v1/admin/accounts/{id}/role", patch(change_role))
        .route("/api/v1/admin/teams", get(list_teams).post(create_team))
        .route("/api/v1/admin/teams/{id}", get(get_team).patch(rename_team))
        .route("/api/v1/admin/teams/{id}/lead", post(replace_lead))
        .route("/api/v1/admin/teams/{id}/members", post(add_member))
        .route(
            "/api/v1/admin/teams/{id}/members/{accountId}",
            axum::routing::delete(remove_member),
        )
        .route("/api/v1/admin/teams/{id}/archive", post(archive_team))
}

/// 403 unless the actor is an Admin (`authz::can_manage_org`).
fn require_admin(actor: &Actor) -> Result<(), ApiError> {
    let principal = Principal {
        account_id: actor.account_id,
        role: actor.role,
        // Admins never have a team; irrelevant for organization rights.
        team: None,
        // The extractor only yields active accounts.
        active: true,
    };
    authz::can_manage_org(&principal).map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ListAccountsQuery {
    /// Filter by role.
    #[param(value_type = Option<Role>)]
    pub role: Option<String>,
    /// Filter by status.
    #[param(value_type = Option<AccountStatus>)]
    pub status: Option<String>,
    /// Case-insensitive substring of username, display name or email (max 100).
    pub q: Option<String>,
    /// Page size (1–100, default 25).
    #[param(value_type = Option<u32>, minimum = 1, maximum = 100)]
    pub limit: Option<String>,
    /// Opaque cursor from a previous response's `nextCursor`.
    pub cursor: Option<String>,
}

/// Lists accounts ordered by normalized username.
#[utoipa::path(
    get,
    path = "/api/v1/admin/accounts",
    tag = "admin",
    operation_id = "listAdminAccounts",
    params(ListAccountsQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "One page of accounts", body = Page<AdminAccount>),
        (status = 400, description = "`invalid_request` (unknown parameter)", body = ErrorBody),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` (not an Admin)", body = ErrorBody),
        (status = 422, description = "`validation_failed` (role, status, q, limit, cursor)", body = ErrorBody),
    )
)]
pub async fn list_accounts(
    State(state): State<AppState>,
    actor: Actor,
    ApiQuery(query): ApiQuery<ListAccountsQuery>,
) -> Result<ApiJson<Page<AdminAccount>>, ApiError> {
    require_admin(&actor)?;
    let mut fields = Vec::new();
    let role = match query.role.as_deref() {
        None => None,
        Some(raw) => {
            let parsed = Role::parse(raw);
            if parsed.is_none() {
                fields.push(crate::error::FieldError::new(
                    "role",
                    FieldErrorCode::InvalidValue,
                ));
            }
            parsed
        }
    };
    let status = match query.status.as_deref() {
        None => None,
        Some(raw) => {
            let parsed = AccountStatus::parse(raw);
            if parsed.is_none() {
                fields.push(crate::error::FieldError::new(
                    "status",
                    FieldErrorCode::InvalidValue,
                ));
            }
            parsed
        }
    };
    let page = PageParams {
        limit: query.limit,
        cursor: query.cursor,
    }
    .validate::<AccountCursor>();
    let page = match page {
        Ok(page) if fields.is_empty() => page,
        Ok(_) => return Err(ApiError::validation(fields)),
        Err(error) => {
            fields.extend(error.fields().iter().cloned());
            return Err(ApiError::validation(fields));
        }
    };
    let filter = AccountFilter {
        role,
        status,
        q: query.q,
    };
    Ok(ApiJson(
        accounts::list_accounts(&state.db, &filter, &page).await?,
    ))
}

/// One account.
#[utoipa::path(
    get,
    path = "/api/v1/admin/accounts/{id}",
    tag = "admin",
    operation_id = "getAdminAccount",
    params(("id" = Uuid, Path, description = "Account id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The account", body = AdminAccount),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` (not an Admin)", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub async fn get_account(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<ApiJson<AdminAccount>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(accounts::get_account(&state.db, id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChangeRoleRequest {
    pub role: Role,
    /// Specialists only (1–100 characters); omit or `null` to clear.
    #[serde(default)]
    pub specialization: Option<String>,
    /// Required when the account currently leads a team and the new role is
    /// not `team_lead`: this Team Lead account takes over in the same change.
    #[serde(default)]
    pub replacement_lead_id: Option<Uuid>,
}

/// Changes an account's role. Takes effect on the account's next request.
#[utoipa::path(
    patch,
    path = "/api/v1/admin/accounts/{id}/role",
    tag = "admin",
    operation_id = "changeAccountRole",
    params(("id" = Uuid, Path, description = "Account id")),
    request_body = ChangeRoleRequest,
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The updated account", body = AdminAccount),
        (status = 400, description = "`invalid_request` (unknown role or field)", body = ErrorBody),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` / `csrf_rejected`", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`last_admin`, `team_membership_conflict`, `team_lead_required`, `conflict`", body = ErrorBody),
        (status = 422, description = "`validation_failed` (specialization, replacementLeadId, accountId)", body = ErrorBody),
    )
)]
pub async fn change_role(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<ChangeRoleRequest>,
) -> Result<ApiJson<AdminAccount>, ApiError> {
    require_admin(&actor)?;
    let change = RoleChange {
        role: body.role,
        specialization: body.specialization,
        replacement_lead_id: body.replacement_lead_id,
    };
    Ok(ApiJson(
        accounts::change_role(&state.db, actor.account_id, id, change).await?,
    ))
}

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListTeamsQuery {
    /// Include archived teams (default `false`).
    pub include_archived: Option<bool>,
}

/// Lists teams ordered by name (not paginated; teams are few).
#[utoipa::path(
    get,
    path = "/api/v1/admin/teams",
    tag = "admin",
    operation_id = "listTeams",
    params(ListTeamsQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "Teams with lead and current members", body = Vec<Team>),
        (status = 400, description = "`invalid_request`", body = ErrorBody),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` (not an Admin)", body = ErrorBody),
    )
)]
pub async fn list_teams(
    State(state): State<AppState>,
    actor: Actor,
    ApiQuery(query): ApiQuery<ListTeamsQuery>,
) -> Result<ApiJson<Vec<Team>>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(
        teams::list_teams(&state.db, query.include_archived.unwrap_or(false)).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateTeamRequest {
    /// 1–100 characters after trimming; unique among active teams.
    pub name: String,
    /// An active Team Lead account without a current team.
    pub lead_account_id: Uuid,
}

/// Creates a team and appoints its lead atomically.
#[utoipa::path(
    post,
    path = "/api/v1/admin/teams",
    tag = "admin",
    operation_id = "createTeam",
    request_body = CreateTeamRequest,
    security(("session_cookie" = [])),
    responses(
        (status = 201, description = "The new team", body = Team),
        (status = 400, description = "`invalid_request`", body = ErrorBody),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` / `csrf_rejected`", body = ErrorBody),
        (status = 409, description = "`team_membership_conflict` (the lead already belongs to a team)", body = ErrorBody),
        (status = 422, description = "`validation_failed` (name, leadAccountId)", body = ErrorBody),
    )
)]
pub async fn create_team(
    State(state): State<AppState>,
    actor: Actor,
    ApiJson(body): ApiJson<CreateTeamRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&actor)?;
    let team = teams::create_team(
        &state.db,
        actor.account_id,
        &body.name,
        body.lead_account_id,
    )
    .await?;
    Ok((StatusCode::CREATED, ApiJson(team)))
}

/// One team (active or archived).
#[utoipa::path(
    get,
    path = "/api/v1/admin/teams/{id}",
    tag = "admin",
    operation_id = "getTeam",
    params(("id" = Uuid, Path, description = "Team id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The team", body = Team),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` (not an Admin)", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub async fn get_team(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<ApiJson<Team>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(teams::get_team(&state.db, id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RenameTeamRequest {
    pub name: String,
}

/// Renames an active team.
#[utoipa::path(
    patch,
    path = "/api/v1/admin/teams/{id}",
    tag = "admin",
    operation_id = "renameTeam",
    params(("id" = Uuid, Path, description = "Team id")),
    request_body = RenameTeamRequest,
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The team", body = Team),
        (status = 400, description = "`invalid_request`", body = ErrorBody),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` / `csrf_rejected`", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`conflict` (archived)", body = ErrorBody),
        (status = 422, description = "`validation_failed` (name)", body = ErrorBody),
    )
)]
pub async fn rename_team(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<RenameTeamRequest>,
) -> Result<ApiJson<Team>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(
        teams::rename_team(&state.db, actor.account_id, id, &body.name).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplaceLeadRequest {
    /// An active Team Lead account in no team or already in this team.
    pub account_id: Uuid,
    /// What happens to the previous lead (default `keep_as_member`).
    #[serde(default)]
    pub previous_lead: Option<PreviousLead>,
}

/// Replaces the team's lead atomically (never co-leads, never leaderless).
#[utoipa::path(
    post,
    path = "/api/v1/admin/teams/{id}/lead",
    tag = "admin",
    operation_id = "replaceTeamLead",
    params(("id" = Uuid, Path, description = "Team id")),
    request_body = ReplaceLeadRequest,
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The team", body = Team),
        (status = 400, description = "`invalid_request`", body = ErrorBody),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` / `csrf_rejected`", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`team_membership_conflict`, `conflict`", body = ErrorBody),
        (status = 422, description = "`validation_failed` (accountId)", body = ErrorBody),
    )
)]
pub async fn replace_lead(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<ReplaceLeadRequest>,
) -> Result<ApiJson<Team>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(
        teams::replace_lead(
            &state.db,
            actor.account_id,
            id,
            body.account_id,
            body.previous_lead.unwrap_or_default(),
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AddMemberRequest {
    /// An active Operator, Specialist or Team Lead account without a team.
    pub account_id: Uuid,
}

/// Adds a plain member.
#[utoipa::path(
    post,
    path = "/api/v1/admin/teams/{id}/members",
    tag = "admin",
    operation_id = "addTeamMember",
    params(("id" = Uuid, Path, description = "Team id")),
    request_body = AddMemberRequest,
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The team", body = Team),
        (status = 400, description = "`invalid_request`", body = ErrorBody),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` / `csrf_rejected`", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`team_membership_conflict`, `conflict` (archived)", body = ErrorBody),
        (status = 422, description = "`validation_failed` (accountId)", body = ErrorBody),
    )
)]
pub async fn add_member(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<AddMemberRequest>,
) -> Result<ApiJson<Team>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(
        teams::add_member(&state.db, actor.account_id, id, body.account_id).await?,
    ))
}

/// Removes a plain member (the lead must be replaced first). Clears the
/// member's open assignments in this team.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/teams/{id}/members/{accountId}",
    tag = "admin",
    operation_id = "removeTeamMember",
    params(
        ("id" = Uuid, Path, description = "Team id"),
        ("accountId" = Uuid, Path, description = "Account id of the member"),
    ),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The team", body = Team),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` / `csrf_rejected`", body = ErrorBody),
        (status = 404, description = "`not_found` (team or membership)", body = ErrorBody),
        (status = 409, description = "`team_lead_required`", body = ErrorBody),
    )
)]
pub async fn remove_member(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath((id, account_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<ApiJson<Team>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(
        teams::remove_member(&state.db, actor.account_id, id, account_id).await?,
    ))
}

/// Archives a team and ends all memberships. Refused while open tickets remain.
#[utoipa::path(
    post,
    path = "/api/v1/admin/teams/{id}/archive",
    tag = "admin",
    operation_id = "archiveTeam",
    params(("id" = Uuid, Path, description = "Team id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The archived team", body = Team),
        (status = 401, description = "`unauthenticated`", body = ErrorBody),
        (status = 403, description = "`forbidden` / `csrf_rejected`", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
        (status = 409, description = "`team_has_open_tickets`, `conflict` (already archived)", body = ErrorBody),
    )
)]
pub async fn archive_team(
    State(state): State<AppState>,
    actor: Actor,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<ApiJson<Team>, ApiError> {
    require_admin(&actor)?;
    Ok(ApiJson(
        teams::archive_team(&state.db, actor.account_id, id).await?,
    ))
}
