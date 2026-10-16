//! Teams, membership and leadership for Admins.
//!
//! Invariants (spec "Roles and team structure"; also enforced by schema
//! triggers in `0001_initial_schema.sql`):
//!
//! - Every active team has exactly one designated lead, who has the Team Lead
//!   role, is active, and belongs to that team. Never co-leads, never a
//!   leaderless active team: creation and lead replacement are single
//!   transactions, and the team row is locked so concurrent replacements
//!   serialize (the later one sees the earlier result).
//! - A non-admin account belongs to at most one team; Admin and User
//!   accounts belong to none.
//! - The sole lead cannot be removed, demoted or deactivated until a
//!   replacement is appointed or the team is archived.
//! - Archiving ends all memberships; it is refused while open tickets remain.
//! - Membership history is append-only: changes end rows and insert new ones.
//!   Periods use `clock_timestamp()`, not the transaction start time, so a
//!   transaction that waited on a lock never ends a row "before" a
//!   concurrently committed row started.
//!
//! When a membership ends, the account's open assignments in that team are
//! cleared (with a `ticket.unassigned` ticket event), so a former member never
//! keeps an individual assignment there.
//!
//! Lead replacement: by default the previous lead stays on the team as a
//! plain member ([`PreviousLead::KeepAsMember`]). It keeps the Team Lead role
//! but gains no lead authority (authorization uses the leadership flag), so
//! there is still exactly one lead; the Admin may later change its role.

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    accounts::current_membership,
    audit::{self, action, target},
    map_db_error, require_admin_actor, trimmed,
};
use crate::{
    domain::{AccountStatus, Role},
    error::{ApiError, ErrorCode, FieldError, FieldErrorCode},
};

/// Maximum team name length (characters), as in the schema.
pub const TEAM_NAME_MAX: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TeamStatus {
    Active,
    Archived,
}

/// Compact account reference (lead).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamPerson {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// A current member (including the lead).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    #[schema(required = true)]
    pub specialization: Option<String>,
    pub status: AccountStatus,
    pub is_lead: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub member_since: OffsetDateTime,
}

/// Admin view of a team with its designated lead and current members
/// (ordered by username). Archived teams have no lead and no members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub id: Uuid,
    pub name: String,
    pub status: TeamStatus,
    #[schema(required = true)]
    pub lead: Option<TeamPerson>,
    pub members: Vec<TeamMember>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub archived_at: Option<OffsetDateTime>,
}

/// What happens to the previous lead when leadership is handed over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviousLead {
    /// Stays on the team as a plain member.
    #[default]
    KeepAsMember,
    /// Leaves the team (its open assignments there are cleared).
    RemoveFromTeam,
}

impl PreviousLead {
    fn as_str(self) -> &'static str {
        match self {
            Self::KeepAsMember => "kept_as_member",
            Self::RemoveFromTeam => "removed",
        }
    }
}

#[derive(sqlx::FromRow)]
struct TeamRow {
    id: Uuid,
    name: String,
    status: String,
    created_at: OffsetDateTime,
    archived_at: Option<OffsetDateTime>,
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    team_id: Uuid,
    id: Uuid,
    username: String,
    display_name: String,
    role: String,
    specialization: Option<String>,
    status: String,
    is_lead: bool,
    started_at: OffsetDateTime,
}

fn assemble(teams: Vec<TeamRow>, members: Vec<MemberRow>) -> Result<Vec<Team>, ApiError> {
    let mut out = Vec::with_capacity(teams.len());
    for team in teams {
        let mut lead = None;
        let mut list = Vec::new();
        for m in members.iter().filter(|m| m.team_id == team.id) {
            if m.is_lead {
                lead = Some(TeamPerson {
                    id: m.id,
                    username: m.username.clone(),
                    display_name: m.display_name.clone(),
                });
            }
            list.push(TeamMember {
                id: m.id,
                username: m.username.clone(),
                display_name: m.display_name.clone(),
                role: Role::parse(&m.role).ok_or_else(|| ApiError::internal("invalid role"))?,
                specialization: m.specialization.clone(),
                status: AccountStatus::parse(&m.status)
                    .ok_or_else(|| ApiError::internal("invalid status"))?,
                is_lead: m.is_lead,
                member_since: m.started_at,
            });
        }
        out.push(Team {
            id: team.id,
            name: team.name,
            status: match team.status.as_str() {
                "active" => TeamStatus::Active,
                "archived" => TeamStatus::Archived,
                _ => return Err(ApiError::internal("invalid team status")),
            },
            lead,
            members: list,
            created_at: team.created_at,
            archived_at: team.archived_at,
        });
    }
    Ok(out)
}

async fn load_teams(
    conn: &mut PgConnection,
    only: Option<Uuid>,
    include_archived: bool,
) -> Result<Vec<Team>, ApiError> {
    let teams: Vec<TeamRow> = sqlx::query_as(
        "SELECT id, name, status, created_at, archived_at FROM teams \
         WHERE ($1::uuid IS NULL OR id = $1) AND ($2 OR status = 'active') \
         ORDER BY lower(name), id",
    )
    .bind(only)
    .bind(include_archived)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let members: Vec<MemberRow> = sqlx::query_as(
        "SELECT m.team_id, a.id, a.username, a.display_name, a.role, a.specialization, a.status, \
                m.is_lead, m.started_at \
         FROM team_memberships m JOIN accounts a ON a.id = m.account_id \
         WHERE m.ended_at IS NULL AND ($1::uuid IS NULL OR m.team_id = $1) \
         ORDER BY a.username_normalized, a.id",
    )
    .bind(only)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    assemble(teams, members)
}

/// All teams (active ones, plus archived when asked), ordered by name.
/// Teams are few per organization, so this list is not paginated.
pub async fn list_teams(pool: &PgPool, include_archived: bool) -> Result<Vec<Team>, ApiError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load_teams(&mut conn, None, include_archived).await
}

/// One team (404 when unknown).
pub async fn get_team(pool: &PgPool, team_id: Uuid) -> Result<Team, ApiError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load_teams(&mut conn, Some(team_id), true)
        .await?
        .pop()
        .ok_or_else(ApiError::not_found)
}

fn validate_name(name: &str) -> Result<String, ApiError> {
    let Some(name) = trimmed(name) else {
        return Err(ApiError::field("name", FieldErrorCode::Required));
    };
    if name.chars().count() > TEAM_NAME_MAX {
        return Err(ApiError::validation(vec![
            FieldError::new("name", FieldErrorCode::TooLong).with_param("max", TEAM_NAME_MAX),
        ]));
    }
    Ok(name.to_owned())
}

/// Locks the team row (`FOR UPDATE`) and requires it to be active.
async fn lock_active_team(conn: &mut PgConnection, team_id: Uuid) -> Result<String, ApiError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT name, status FROM teams WHERE id = $1 FOR UPDATE")
            .bind(team_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
    match row {
        None => Err(ApiError::not_found()),
        Some((name, status)) if status == "active" => Ok(name),
        Some(_) => Err(ApiError::conflict().with_message("The team is archived.")),
    }
}

/// Share-locks an account and returns `(role, active)`; 422 on `field` when
/// it does not exist.
async fn load_candidate(
    conn: &mut PgConnection,
    account_id: Uuid,
    field: &'static str,
) -> Result<(Role, bool), ApiError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT role, status FROM accounts WHERE id = $1 FOR SHARE")
            .bind(account_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
    let (role, status) = row.ok_or_else(|| ApiError::field(field, FieldErrorCode::InvalidValue))?;
    let role = Role::parse(&role).ok_or_else(|| ApiError::internal("invalid role"))?;
    Ok((role, status == "active"))
}

/// A lead candidate must be an active Team Lead account.
async fn require_lead_candidate(
    conn: &mut PgConnection,
    account_id: Uuid,
    field: &'static str,
) -> Result<(), ApiError> {
    let (role, active) = load_candidate(conn, account_id, field).await?;
    if role != Role::TeamLead || !active {
        return Err(ApiError::field(field, FieldErrorCode::InvalidValue)
            .with_message("The team lead must be an active Team Lead account."));
    }
    Ok(())
}

async fn insert_membership(
    conn: &mut PgConnection,
    team_id: Uuid,
    account_id: Uuid,
    is_lead: bool,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO team_memberships (id, team_id, account_id, is_lead, started_at) \
         VALUES ($1, $2, $3, $4, clock_timestamp())",
    )
    .bind(Uuid::now_v7())
    .bind(team_id)
    .bind(account_id)
    .bind(is_lead)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

async fn end_membership(
    conn: &mut PgConnection,
    team_id: Uuid,
    account_id: Uuid,
) -> Result<u64, ApiError> {
    sqlx::query(
        "UPDATE team_memberships SET ended_at = greatest(clock_timestamp(), started_at) \
         WHERE team_id = $1 AND account_id = $2 AND ended_at IS NULL",
    )
    .bind(team_id)
    .bind(account_id)
    .execute(&mut *conn)
    .await
    .map(|r| r.rows_affected())
    .map_err(map_db_error)
}

/// Clears `account_id`'s open assignments in `team_id` after its membership
/// ended, recording a ticket event per ticket. Returns the number cleared.
async fn clear_assignments(
    conn: &mut PgConnection,
    actor_id: Uuid,
    team_id: Uuid,
    account_id: Uuid,
) -> Result<u64, ApiError> {
    let cleared: Vec<(Uuid, String)> = sqlx::query_as(
        "UPDATE tickets SET assignee_account_id = NULL \
         WHERE team_id = $1 AND assignee_account_id = $2 \
           AND status IN ('queued', 'in_progress', 'waiting_for_requester', 'awaiting_confirmation') \
         RETURNING id, status",
    )
    .bind(team_id)
    .bind(account_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (ticket_id, status) in &cleared {
        sqlx::query(
            "INSERT INTO ticket_events (id, ticket_id, actor_account_id, actor_kind, event_type, \
                                        from_status, to_status, data) \
             VALUES ($1, $2, $3, 'staff', 'ticket.unassigned', $4, $4, $5)",
        )
        .bind(Uuid::now_v7())
        .bind(ticket_id)
        .bind(actor_id)
        .bind(status)
        .bind(json!({ "reason": "membership_ended", "previousAssigneeAccountId": account_id }))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(cleared.len() as u64)
}

/// Creates an active team and appoints its lead atomically. The lead must be
/// an active Team Lead account without a current team.
pub async fn create_team(
    pool: &PgPool,
    actor_id: Uuid,
    name: &str,
    lead_account_id: Uuid,
) -> Result<Team, ApiError> {
    let name = validate_name(name)?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    require_admin_actor(&mut tx, actor_id).await?;
    require_lead_candidate(&mut tx, lead_account_id, "leadAccountId").await?;
    if current_membership(&mut tx, lead_account_id)
        .await?
        .is_some()
    {
        return Err(ApiError::new(ErrorCode::TeamMembershipConflict));
    }
    let team_id = Uuid::now_v7();
    sqlx::query("INSERT INTO teams (id, name) VALUES ($1, $2)")
        .bind(team_id)
        .bind(&name)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    insert_membership(&mut tx, team_id, lead_account_id, true).await?;
    audit::record(
        &mut tx,
        actor_id,
        action::TEAM_CREATED,
        target::TEAM,
        team_id,
        json!({ "name": name, "leadAccountId": lead_account_id }),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_team(pool, team_id).await
}

/// Renames an active team (names are unique among active teams,
/// case-insensitively).
pub async fn rename_team(
    pool: &PgPool,
    actor_id: Uuid,
    team_id: Uuid,
    name: &str,
) -> Result<Team, ApiError> {
    let name = validate_name(name)?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    require_admin_actor(&mut tx, actor_id).await?;
    let previous = lock_active_team(&mut tx, team_id).await?;
    if previous != name {
        sqlx::query("UPDATE teams SET name = $2 WHERE id = $1")
            .bind(team_id)
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        audit::record(
            &mut tx,
            actor_id,
            action::TEAM_RENAMED,
            target::TEAM,
            team_id,
            json!({ "previousName": previous, "name": name }),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get_team(pool, team_id).await
}

/// Replaces the team's lead in the caller's transaction. Returns the previous
/// lead, or `None` when `new_lead_id` already leads (no-op).
pub(crate) async fn replace_lead_in_tx(
    conn: &mut PgConnection,
    actor_id: Uuid,
    team_id: Uuid,
    new_lead_id: Uuid,
    previous: PreviousLead,
) -> Result<Option<Uuid>, ApiError> {
    lock_active_team(conn, team_id).await?;
    let old_lead: Uuid = sqlx::query_scalar(
        "SELECT account_id FROM team_memberships \
         WHERE team_id = $1 AND is_lead AND ended_at IS NULL FOR UPDATE",
    )
    .bind(team_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if old_lead == new_lead_id {
        return Ok(None);
    }
    require_lead_candidate(conn, new_lead_id, "accountId").await?;
    match current_membership(conn, new_lead_id).await? {
        Some((team, _)) if team != team_id => {
            return Err(ApiError::new(ErrorCode::TeamMembershipConflict));
        }
        Some(_) => {
            end_membership(conn, team_id, new_lead_id).await?;
        }
        None => {}
    }
    end_membership(conn, team_id, old_lead).await?;
    insert_membership(conn, team_id, new_lead_id, true).await?;
    let cleared = match previous {
        PreviousLead::KeepAsMember => {
            insert_membership(conn, team_id, old_lead, false).await?;
            0
        }
        PreviousLead::RemoveFromTeam => {
            clear_assignments(conn, actor_id, team_id, old_lead).await?
        }
    };
    audit::record(
        conn,
        actor_id,
        action::TEAM_LEAD_REPLACED,
        target::TEAM,
        team_id,
        json!({
            "previousLeadAccountId": old_lead,
            "leadAccountId": new_lead_id,
            "previousLead": previous.as_str(),
            "clearedAssignments": cleared,
        }),
    )
    .await?;
    Ok(Some(old_lead))
}

/// Appoints `new_lead_id` (an active Team Lead account that is in no team or
/// already a member of this team) as the team's sole lead, atomically. See
/// [`PreviousLead`] for what happens to the previous lead.
pub async fn replace_lead(
    pool: &PgPool,
    actor_id: Uuid,
    team_id: Uuid,
    new_lead_id: Uuid,
    previous: PreviousLead,
) -> Result<Team, ApiError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    require_admin_actor(&mut tx, actor_id).await?;
    replace_lead_in_tx(&mut tx, actor_id, team_id, new_lead_id, previous).await?;
    tx.commit().await.map_err(map_db_error)?;
    get_team(pool, team_id).await
}

/// Adds an active Operator, Specialist or Team Lead account without a
/// current team as a plain member.
pub async fn add_member(
    pool: &PgPool,
    actor_id: Uuid,
    team_id: Uuid,
    account_id: Uuid,
) -> Result<Team, ApiError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    require_admin_actor(&mut tx, actor_id).await?;
    lock_active_team(&mut tx, team_id).await?;
    let (role, active) = load_candidate(&mut tx, account_id, "accountId").await?;
    if !active {
        return Err(ApiError::field("accountId", FieldErrorCode::InvalidValue));
    }
    if !role.is_team_eligible() || current_membership(&mut tx, account_id).await?.is_some() {
        return Err(ApiError::new(ErrorCode::TeamMembershipConflict));
    }
    insert_membership(&mut tx, team_id, account_id, false).await?;
    audit::record(
        &mut tx,
        actor_id,
        action::TEAM_MEMBER_ADDED,
        target::TEAM,
        team_id,
        json!({ "accountId": account_id }),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_team(pool, team_id).await
}

/// Ends a plain membership (404 when the account is not a current member).
/// The designated lead cannot be removed (`team_lead_required`).
pub async fn remove_member(
    pool: &PgPool,
    actor_id: Uuid,
    team_id: Uuid,
    account_id: Uuid,
) -> Result<Team, ApiError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    require_admin_actor(&mut tx, actor_id).await?;
    lock_active_team(&mut tx, team_id).await?;
    match current_membership(&mut tx, account_id).await? {
        Some((team, false)) if team == team_id => {}
        Some((team, true)) if team == team_id => {
            return Err(ApiError::new(ErrorCode::TeamLeadRequired));
        }
        _ => return Err(ApiError::not_found()),
    }
    end_membership(&mut tx, team_id, account_id).await?;
    let cleared = clear_assignments(&mut tx, actor_id, team_id, account_id).await?;
    audit::record(
        &mut tx,
        actor_id,
        action::TEAM_MEMBER_REMOVED,
        target::TEAM,
        team_id,
        json!({ "accountId": account_id, "clearedAssignments": cleared }),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_team(pool, team_id).await
}

/// Archives a team: refused while it has open tickets; ends every current
/// membership (including the lead's) in the same transaction.
pub async fn archive_team(pool: &PgPool, actor_id: Uuid, team_id: Uuid) -> Result<Team, ApiError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    require_admin_actor(&mut tx, actor_id).await?;
    lock_active_team(&mut tx, team_id).await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets WHERE team_id = $1 AND status NOT IN ('closed', 'cancelled')",
    )
    .bind(team_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open > 0 {
        return Err(ApiError::new(ErrorCode::TeamHasOpenTickets));
    }
    let ended = sqlx::query(
        "UPDATE team_memberships SET ended_at = greatest(clock_timestamp(), started_at) \
         WHERE team_id = $1 AND ended_at IS NULL",
    )
    .bind(team_id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    sqlx::query("UPDATE teams SET status = 'archived', archived_at = now() WHERE id = $1")
        .bind(team_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit::record(
        &mut tx,
        actor_id,
        action::TEAM_ARCHIVED,
        target::TEAM,
        team_id,
        json!({ "endedMemberships": ended }),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_team(pool, team_id).await
}
