//! Self-view account DTOs shared by auth, setup, invitations and `/me`.

use serde::Serialize;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::{AccountStatus, Role},
    dto::Language,
    error::{FieldError, FieldErrorCode},
    extract::FieldErrors,
};

/// The signed-in account's own view of itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: Uuid,
    /// As entered at creation (trimmed); login compares the normalized form.
    pub username: String,
    pub display_name: String,
    /// Contact address only; not unique and never used to identify accounts.
    pub email: String,
    pub role: Role,
    /// Specialist field designation; grants no access.
    #[schema(required = true)]
    pub specialization: Option<String>,
    pub language: Language,
    pub status: AccountStatus,
    /// The current team membership, `null` when the account has no team.
    #[schema(required = true)]
    pub team: Option<AccountTeam>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountTeam {
    pub id: Uuid,
    pub name: String,
    pub is_lead: bool,
}

/// The current session: who is signed in and until when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub account: Account,
    /// Absolute expiry of the session (UTC). The session also ends after 12
    /// hours without activity.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    username: String,
    display_name: String,
    email: String,
    #[sqlx(try_from = "String")]
    role: Role,
    specialization: Option<String>,
    #[sqlx(try_from = "String")]
    language: Language,
    #[sqlx(try_from = "String")]
    status: AccountStatus,
    created_at: OffsetDateTime,
    team_id: Option<Uuid>,
    team_name: Option<String>,
    team_is_lead: Option<bool>,
}

/// Loads the self view of `account_id` (with its current team, if any).
pub async fn load_account<'e, E>(
    executor: E,
    account_id: Uuid,
) -> Result<Option<Account>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let row: Option<AccountRow> = sqlx::query_as(
        "SELECT a.id, a.username, a.display_name, a.email, a.role, a.specialization, a.language, \
                a.status, a.created_at, t.id AS team_id, t.name AS team_name, m.is_lead AS team_is_lead \
         FROM accounts a \
         LEFT JOIN team_memberships m ON m.account_id = a.id AND m.ended_at IS NULL \
         LEFT JOIN teams t ON t.id = m.team_id \
         WHERE a.id = $1",
    )
    .bind(account_id)
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|row| Account {
        id: row.id,
        username: row.username,
        display_name: row.display_name,
        email: row.email,
        role: row.role,
        specialization: row.specialization,
        language: row.language,
        status: row.status,
        team: match (row.team_id, row.team_name) {
            (Some(id), Some(name)) => Some(AccountTeam {
                id,
                name,
                is_lead: row.team_is_lead.unwrap_or(false),
            }),
            _ => None,
        },
        created_at: row.created_at,
    }))
}

// ---------------------------------------------------------------------------
// Shared field validation for account creation and profile edits
// ---------------------------------------------------------------------------

pub const DISPLAY_NAME_MAX_CHARS: usize = 100;
pub const EMAIL_MAX_CHARS: usize = 254;

/// Display name: trimmed, 1–100 characters, no control characters.
pub fn check_display_name(errors: &mut FieldErrors, field: &str, value: &str) {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        errors.add(field, FieldErrorCode::Required);
    } else if len > DISPLAY_NAME_MAX_CHARS {
        errors.push(
            FieldError::new(field, FieldErrorCode::TooLong)
                .with_param("max", DISPLAY_NAME_MAX_CHARS),
        );
    } else if trimmed.chars().any(char::is_control) {
        errors.add(field, FieldErrorCode::InvalidFormat);
    }
}

/// Contact email: trimmed, at most 254 characters, exactly one `@` with a
/// non-empty local part and domain, no whitespace or control characters.
/// Not unique: accounts may share an address.
pub fn check_email(errors: &mut FieldErrors, field: &str, value: &str) {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        errors.add(field, FieldErrorCode::Required);
        return;
    }
    if len > EMAIL_MAX_CHARS {
        errors.push(
            FieldError::new(field, FieldErrorCode::TooLong).with_param("max", EMAIL_MAX_CHARS),
        );
        return;
    }
    let valid = match trimmed.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && !domain.contains('@')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        None => false,
    };
    if !valid {
        errors.add(field, FieldErrorCode::InvalidFormat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(value: &str) -> Option<FieldErrorCode> {
        let mut errors = FieldErrors::new();
        check_email(&mut errors, "email", value);
        errors.finish().err().map(|e| e.fields()[0].code)
    }

    #[test]
    fn email_rules() {
        assert_eq!(email(" it@example.test "), None);
        assert_eq!(email("it@localhost"), None);
        assert_eq!(email(""), Some(FieldErrorCode::Required));
        assert_eq!(email("no-at"), Some(FieldErrorCode::InvalidFormat));
        assert_eq!(email("a@b@c"), Some(FieldErrorCode::InvalidFormat));
        assert_eq!(email("@example.test"), Some(FieldErrorCode::InvalidFormat));
        assert_eq!(
            email("a b@example.test"),
            Some(FieldErrorCode::InvalidFormat)
        );
        assert_eq!(
            email(&format!("{}@example.test", "a".repeat(250))),
            Some(FieldErrorCode::TooLong)
        );
    }

    #[test]
    fn display_name_rules() {
        let check = |v: &str| {
            let mut errors = FieldErrors::new();
            check_display_name(&mut errors, "displayName", v);
            errors.finish().err().map(|e| e.fields()[0].code)
        };
        assert_eq!(check(" Ada "), None);
        assert_eq!(check("  "), Some(FieldErrorCode::Required));
        assert_eq!(check(&"a".repeat(101)), Some(FieldErrorCode::TooLong));
        assert_eq!(check("a\u{7}b"), Some(FieldErrorCode::InvalidFormat));
    }
}
