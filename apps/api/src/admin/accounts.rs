//! Account directory for Admins: listing, role changes, deactivation.
//!
//! Invariants enforced here (and again by schema triggers):
//!
//! - The last active Admin can never be demoted or deactivated
//!   (`last_admin`). All active Admin rows are locked first, so two
//!   concurrent demotions of the only two Admins cannot both succeed.
//! - Admin and User roles cannot hold a team membership: promoting a current
//!   member to Admin (or demoting one to User) is rejected
//!   (`team_membership_conflict`) until the membership is ended explicitly.
//! - A current team lead keeps the Team Lead role and stays active until a
//!   replacement is appointed (`team_lead_required`). [`change_role`] can do
//!   both atomically when given `replacement_lead_id`.
//! - Shared email addresses never matter: every check uses account IDs.

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    audit::{self, action, target},
    lock_active_admins, map_db_error, teams, trimmed,
};
use crate::{
    auth::account::AccountTeam,
    domain::{AccountStatus, Role},
    dto::Language,
    error::{ApiError, ErrorCode, FieldError, FieldErrorCode},
    pagination::{Page, PageRequest},
};

/// Maximum specialization length (characters), as in the schema.
pub const SPECIALIZATION_MAX: usize = 100;
/// Maximum search query length (characters).
pub const SEARCH_MAX: usize = 100;

/// Admin view of an account. Accounts are identified by `id` and `username`;
/// `email` is contact information only and may be shared by several accounts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccount {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub role: Role,
    /// Display designation (Specialists). Grants no access.
    #[schema(required = true)]
    pub specialization: Option<String>,
    pub language: Language,
    pub status: AccountStatus,
    #[schema(required = true)]
    pub team: Option<AccountTeam>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub deactivated_at: Option<OffsetDateTime>,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    username: String,
    username_normalized: String,
    display_name: String,
    email: String,
    role: String,
    specialization: Option<String>,
    language: String,
    status: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    deactivated_at: Option<OffsetDateTime>,
    team_id: Option<Uuid>,
    team_name: Option<String>,
    is_lead: Option<bool>,
}

const ACCOUNT_SELECT: &str = "SELECT a.id, a.username, a.username_normalized, a.display_name, \
         a.email, a.role, a.specialization, a.language, a.status, a.created_at, a.updated_at, \
         a.deactivated_at, m.team_id, t.name AS team_name, m.is_lead \
     FROM accounts a \
     LEFT JOIN team_memberships m ON m.account_id = a.id AND m.ended_at IS NULL \
     LEFT JOIN teams t ON t.id = m.team_id";

impl TryFrom<AccountRow> for AdminAccount {
    type Error = ApiError;

    fn try_from(row: AccountRow) -> Result<Self, ApiError> {
        let invalid =
            |what: &str| ApiError::internal(format!("account {}: invalid {what}", row.id));
        Ok(Self {
            role: Role::parse(&row.role).ok_or_else(|| invalid("role"))?,
            status: AccountStatus::parse(&row.status).ok_or_else(|| invalid("status"))?,
            language: Language::parse(&row.language).ok_or_else(|| invalid("language"))?,
            team: match (row.team_id, row.team_name) {
                (Some(id), Some(name)) => Some(AccountTeam {
                    id,
                    name,
                    is_lead: row.is_lead.unwrap_or(false),
                }),
                _ => None,
            },
            id: row.id,
            username: row.username,
            display_name: row.display_name,
            email: row.email,
            specialization: row.specialization,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deactivated_at: row.deactivated_at,
        })
    }
}

/// Filters for [`list_accounts`].
#[derive(Clone, Debug, Default)]
pub struct AccountFilter {
    pub role: Option<Role>,
    pub status: Option<AccountStatus>,
    /// Case-insensitive substring of username, display name or email.
    pub q: Option<String>,
}

/// Ordering key of [`list_accounts`]: `(username_normalized, id)` ascending.
pub type AccountCursor = (String, Uuid);

fn like_pattern(q: &str) -> String {
    let mut escaped = String::with_capacity(q.len() + 2);
    escaped.push('%');
    for c in q.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped.push('%');
    escaped
}

/// Lists accounts ordered by normalized username (then id).
pub async fn list_accounts(
    pool: &PgPool,
    filter: &AccountFilter,
    page: &PageRequest<AccountCursor>,
) -> Result<Page<AdminAccount>, ApiError> {
    let q = filter.q.as_deref().and_then(trimmed);
    if q.is_some_and(|q| q.chars().count() > SEARCH_MAX) {
        return Err(ApiError::validation(vec![
            FieldError::new("q", FieldErrorCode::TooLong).with_param("max", SEARCH_MAX),
        ]));
    }
    let (after_name, after_id) = page.after.clone().unzip();
    let sql = format!(
        "{ACCOUNT_SELECT} \
         WHERE ($1::text IS NULL OR a.role = $1) \
           AND ($2::text IS NULL OR a.status = $2) \
           AND ($3::text IS NULL OR a.username_normalized ILIKE $3 OR a.display_name ILIKE $3 \
                OR a.email ILIKE $3) \
           AND ($4::text IS NULL OR (a.username_normalized, a.id) > ($4, $5)) \
         ORDER BY a.username_normalized, a.id \
         LIMIT $6"
    );
    let rows: Vec<AccountRow> = sqlx::query_as(&sql)
        .bind(filter.role.map(Role::as_str))
        .bind(filter.status.map(AccountStatus::as_str))
        .bind(q.map(like_pattern))
        .bind(after_name)
        .bind(after_id)
        .bind(page.fetch_limit())
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?;
    let keys: Vec<AccountCursor> = rows
        .iter()
        .map(|r| (r.username_normalized.clone(), r.id))
        .collect();
    let items = rows
        .into_iter()
        .map(AdminAccount::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    // Pair items with their keys for the cursor.
    let paired: Vec<(AdminAccount, AccountCursor)> = items.into_iter().zip(keys).collect();
    let page = Page::from_overfetched(paired, page, |(_, key)| key.clone());
    Ok(Page {
        items: page.items.into_iter().map(|(item, _)| item).collect(),
        next_cursor: page.next_cursor,
    })
}

/// One account (404 when unknown).
pub async fn get_account(pool: &PgPool, account_id: Uuid) -> Result<AdminAccount, ApiError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    fetch_account(&mut conn, account_id).await
}

async fn fetch_account(
    conn: &mut PgConnection,
    account_id: Uuid,
) -> Result<AdminAccount, ApiError> {
    let row: Option<AccountRow> = sqlx::query_as(&format!("{ACCOUNT_SELECT} WHERE a.id = $1"))
        .bind(account_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    row.ok_or_else(ApiError::not_found)?.try_into()
}

/// A role change requested by an Admin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleChange {
    pub role: Role,
    /// Only for Specialists (trimmed, 1–100 characters); `None` clears it.
    pub specialization: Option<String>,
    /// Required when the account currently leads a team and the new role is
    /// not Team Lead: this Team Lead account takes over leadership in the
    /// same transaction. The previous lead stays a plain member when the new
    /// role is Operator/Specialist and leaves the team for User/Admin.
    pub replacement_lead_id: Option<Uuid>,
}

fn validate_specialization(change: &RoleChange) -> Result<Option<String>, ApiError> {
    let value = change.specialization.as_deref().and_then(trimmed);
    match value {
        None => Ok(None),
        Some(_) if change.role != Role::Specialist => Err(ApiError::field(
            "specialization",
            FieldErrorCode::InvalidValue,
        )),
        Some(v) if v.chars().count() > SPECIALIZATION_MAX => Err(ApiError::validation(vec![
            FieldError::new("specialization", FieldErrorCode::TooLong)
                .with_param("max", SPECIALIZATION_MAX),
        ])),
        Some(v) => Ok(Some(v.to_owned())),
    }
}

struct LockedAccount {
    role: Role,
    status: AccountStatus,
    specialization: Option<String>,
}

async fn lock_account(conn: &mut PgConnection, id: Uuid) -> Result<LockedAccount, ApiError> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT role, status, specialization FROM accounts WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (role, status, specialization) = row.ok_or_else(ApiError::not_found)?;
    Ok(LockedAccount {
        role: Role::parse(&role).ok_or_else(|| ApiError::internal("invalid role"))?,
        status: AccountStatus::parse(&status)
            .ok_or_else(|| ApiError::internal("invalid status"))?,
        specialization,
    })
}

/// Current membership of an account: `(team_id, is_lead)`.
pub(crate) async fn current_membership(
    conn: &mut PgConnection,
    account_id: Uuid,
) -> Result<Option<(Uuid, bool)>, ApiError> {
    sqlx::query_as(
        "SELECT team_id, is_lead FROM team_memberships WHERE account_id = $1 AND ended_at IS NULL",
    )
    .bind(account_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Changes an account's role (and specialization). See the module docs for
/// the invariants. No-op (no audit row) when nothing changes.
pub async fn change_role(
    pool: &PgPool,
    actor_id: Uuid,
    target_id: Uuid,
    change: RoleChange,
) -> Result<AdminAccount, ApiError> {
    let specialization = validate_specialization(&change)?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;

    let admins = lock_active_admins(&mut tx).await?;
    if !admins.contains(&actor_id) {
        return Err(ApiError::forbidden());
    }
    let current = lock_account(&mut tx, target_id).await?;
    let membership = current_membership(&mut tx, target_id).await?;
    let leads_team = membership
        .filter(|(_, is_lead)| *is_lead)
        .map(|(team, _)| team);

    if change.replacement_lead_id.is_some()
        && (leads_team.is_none() || change.role == Role::TeamLead)
    {
        return Err(ApiError::field(
            "replacementLeadId",
            FieldErrorCode::InvalidValue,
        ));
    }
    if current.role == change.role && current.specialization == specialization {
        drop(tx);
        return get_account(pool, target_id).await;
    }

    // Last active Admin protection.
    if current.role == Role::Admin
        && change.role != Role::Admin
        && current.status == AccountStatus::Active
        && admins.len() <= 1
    {
        return Err(ApiError::new(ErrorCode::LastAdmin));
    }
    // Sole lead protection: leadership must be handed over explicitly.
    if let Some(team_id) = leads_team.filter(|_| change.role != Role::TeamLead) {
        let Some(replacement) = change.replacement_lead_id else {
            return Err(ApiError::new(ErrorCode::TeamLeadRequired));
        };
        let previous = if change.role.is_team_eligible() {
            teams::PreviousLead::KeepAsMember
        } else {
            teams::PreviousLead::RemoveFromTeam
        };
        teams::replace_lead_in_tx(&mut tx, actor_id, team_id, replacement, previous).await?;
    } else if membership.is_some() && !change.role.is_team_eligible() {
        // Admin is mutually exclusive with team ties; Users do no team work.
        return Err(ApiError::new(ErrorCode::TeamMembershipConflict));
    }

    sqlx::query("UPDATE accounts SET role = $2, specialization = $3 WHERE id = $1")
        .bind(target_id)
        .bind(change.role.as_str())
        .bind(&specialization)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit::record(
        &mut tx,
        actor_id,
        action::ACCOUNT_ROLE_CHANGED,
        target::ACCOUNT,
        target_id,
        json!({
            "fromRole": current.role.as_str(),
            "toRole": change.role.as_str(),
            "specializationChanged": current.specialization != specialization,
            "replacementLeadAccountId": change.replacement_lead_id,
        }),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_account(pool, target_id).await
}

/// Checks that deactivating `target_id` keeps the organization valid: not
/// the last active Admin, not the designated lead of an active team. The
/// caller must hold [`lock_active_admins`] and the target row lock.
pub(crate) async fn guard_deactivation(
    conn: &mut PgConnection,
    admins: &[Uuid],
    target_id: Uuid,
    target_role: Role,
) -> Result<(), ApiError> {
    if target_role == Role::Admin && admins.contains(&target_id) && admins.len() <= 1 {
        return Err(ApiError::new(ErrorCode::LastAdmin));
    }
    if current_membership(conn, target_id)
        .await?
        .is_some_and(|(_, is_lead)| is_lead)
    {
        return Err(ApiError::new(ErrorCode::TeamLeadRequired));
    }
    Ok(())
}

/// Deactivates an account with last-Admin and sole-lead protection.
/// Idempotent. Session revocation belongs to the session layer (#9/#10) and
/// must run in the same request.
pub async fn deactivate_account(
    pool: &PgPool,
    actor_id: Uuid,
    target_id: Uuid,
) -> Result<AdminAccount, ApiError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let admins = lock_active_admins(&mut tx).await?;
    if !admins.contains(&actor_id) {
        return Err(ApiError::forbidden());
    }
    let current = lock_account(&mut tx, target_id).await?;
    if current.status == AccountStatus::Deactivated {
        drop(tx);
        return get_account(pool, target_id).await;
    }
    guard_deactivation(&mut tx, &admins, target_id, current.role).await?;
    sqlx::query("UPDATE accounts SET status = 'deactivated', deactivated_at = now() WHERE id = $1")
        .bind(target_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit::record(
        &mut tx,
        actor_id,
        action::ACCOUNT_DEACTIVATED,
        target::ACCOUNT,
        target_id,
        json!({ "role": current.role.as_str() }),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_account(pool, target_id).await
}

/// Reactivates a deactivated account. Idempotent.
pub async fn reactivate_account(
    pool: &PgPool,
    actor_id: Uuid,
    target_id: Uuid,
) -> Result<AdminAccount, ApiError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let admins = lock_active_admins(&mut tx).await?;
    if !admins.contains(&actor_id) {
        return Err(ApiError::forbidden());
    }
    let current = lock_account(&mut tx, target_id).await?;
    if current.status == AccountStatus::Active {
        drop(tx);
        return get_account(pool, target_id).await;
    }
    sqlx::query("UPDATE accounts SET status = 'active', deactivated_at = NULL WHERE id = $1")
        .bind(target_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit::record(
        &mut tx,
        actor_id,
        action::ACCOUNT_REACTIVATED,
        target::ACCOUNT,
        target_id,
        json!({ "role": current.role.as_str() }),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_account(pool, target_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("a_b%c\\"), "%a\\_b\\%c\\\\%");
    }

    #[test]
    fn specialization_only_for_specialists() {
        let change = |role, spec: Option<&str>| RoleChange {
            role,
            specialization: spec.map(str::to_owned),
            replacement_lead_id: None,
        };
        assert_eq!(
            validate_specialization(&change(Role::Specialist, Some("  Network "))).unwrap(),
            Some("Network".into())
        );
        assert_eq!(
            validate_specialization(&change(Role::Operator, Some("  "))).unwrap(),
            None
        );
        assert!(validate_specialization(&change(Role::Operator, Some("Network"))).is_err());
        assert!(
            validate_specialization(&change(Role::Specialist, Some(&"x".repeat(101)))).is_err()
        );
    }
}
