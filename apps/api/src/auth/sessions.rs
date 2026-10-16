//! Server-side session store (`sessions` table, migration 0002).
//!
//! - Token: 32 random bytes, base64url without padding (43 characters).
//!   Only `SHA-256(token)` is stored.
//! - Idle timeout 12 h (from `last_seen_at`), absolute lifetime 7 d (from
//!   creation). Both are checked against the database clock.
//! - Rotation: on the first authenticated request at least 15 min after the
//!   last rotation, a new token is issued; the previous token stays valid
//!   for 60 s so concurrent in-flight requests still succeed. Rotation is a
//!   conditional update, so concurrent requests rotate at most once.
//! - `last_seen_at` is written at most once a minute per session.
//! - Revocation is server-side and final.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgPool, Postgres};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::audit::{self, AuditActor, AuditEvent, actions};
use crate::{
    domain::{AccountStatus, Role},
    dto::Language,
};

pub const TOKEN_BYTES: usize = 32;

/// Session lifetime policy. [`SessionPolicy::default`] is the product policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionPolicy {
    pub idle_timeout: Duration,
    pub absolute_lifetime: Duration,
    pub rotation_interval: Duration,
    pub rotation_grace: Duration,
    /// Minimum interval between `last_seen_at` writes.
    pub touch_interval: Duration,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::hours(12),
            absolute_lifetime: Duration::days(7),
            rotation_interval: Duration::minutes(15),
            rotation_grace: Duration::seconds(60),
            touch_interval: Duration::seconds(60),
        }
    }
}

/// Why a session was revoked (stored as `sessions.revoke_reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevokeReason {
    /// The user logged out.
    Logout,
    /// A new login was made with this session's cookie present (session
    /// fixation defence).
    LoginReplaced,
    /// The account was found not active while using the session.
    AccountInactive,
    /// An Admin deactivated the account (#10).
    AccountDeactivated,
    /// The password was changed (#10; the current session is rotated instead).
    PasswordChanged,
    /// The password was reset through recovery (#10).
    PasswordReset,
    /// Revoked by an Admin.
    AdminRevoked,
}

impl RevokeReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Logout => "logout",
            Self::LoginReplaced => "login_replaced",
            Self::AccountInactive => "account_inactive",
            Self::AccountDeactivated => "account_deactivated",
            Self::PasswordChanged => "password_changed",
            Self::PasswordReset => "password_reset",
            Self::AdminRevoked => "admin_revoked",
        }
    }
}

/// A fresh random token (base64url, no padding).
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).expect("operating system RNG available");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 of a well-formed token; `None` for anything that cannot be one of
/// our tokens (so garbage never reaches the database).
pub fn token_hash(token: &str) -> Option<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
    (bytes.len() == TOKEN_BYTES).then(|| Sha256::digest(token.as_bytes()).to_vec())
}

/// A newly created session. `token` goes into the cookie and nowhere else.
#[derive(Clone, Debug)]
pub struct NewSession {
    pub id: Uuid,
    pub token: String,
    pub expires_at: OffsetDateTime,
    /// Cookie `Max-Age`: the full absolute lifetime.
    pub max_age_secs: i64,
}

/// Creates a session for `account_id`. Callers record the audit event.
pub async fn create<'e, E>(
    executor: E,
    account_id: Uuid,
    policy: &SessionPolicy,
) -> Result<NewSession, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let token = generate_token();
    let hash = token_hash(&token).expect("generated tokens are well-formed");
    let lifetime = policy.absolute_lifetime.whole_seconds();
    let expires_at: OffsetDateTime = sqlx::query_scalar(
        "INSERT INTO sessions (id, account_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, now() + make_interval(secs => $4)) RETURNING expires_at",
    )
    .bind(id)
    .bind(account_id)
    .bind(hash)
    .bind(lifetime as f64)
    .fetch_one(executor)
    .await?;
    Ok(NewSession {
        id,
        token,
        expires_at,
        max_age_secs: lifetime,
    })
}

/// The result of checking a presented token.
#[derive(Clone, Debug)]
pub struct ValidSession {
    pub session_id: Uuid,
    pub account_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub language: Language,
    pub expires_at: OffsetDateTime,
    /// Set when this request rotated the token: the new cookie value and
    /// its `Max-Age`.
    pub rotated: Option<(String, i64)>,
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    account_id: Uuid,
    matched_current: bool,
    last_seen_at: OffsetDateTime,
    rotated_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
    previous_token_valid_until: Option<OffsetDateTime>,
    now: OffsetDateTime,
    username: String,
    display_name: String,
    #[sqlx(try_from = "String")]
    role: Role,
    #[sqlx(try_from = "String")]
    language: Language,
    #[sqlx(try_from = "String")]
    status: AccountStatus,
}

/// Validates `token`, reloading the account. Returns `None` for unknown,
/// revoked, expired or superseded tokens and for accounts that are not
/// active (whose session is revoked here). May rotate the token or refresh
/// `last_seen_at`.
pub async fn authenticate(
    pool: &PgPool,
    token: &str,
    policy: &SessionPolicy,
) -> Result<Option<ValidSession>, sqlx::Error> {
    let Some(hash) = token_hash(token) else {
        return Ok(None);
    };
    let row: Option<SessionRow> = sqlx::query_as(
        "SELECT s.id, s.account_id, s.token_hash = $1 AS matched_current, s.last_seen_at, \
                s.rotated_at, s.expires_at, s.revoked_at, s.previous_token_valid_until, \
                now() AS now, a.username, a.display_name, a.role, a.language, a.status \
         FROM sessions s JOIN accounts a ON a.id = s.account_id \
         WHERE s.token_hash = $1 OR s.previous_token_hash = $1",
    )
    .bind(&hash)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let now = row.now;
    if row.revoked_at.is_some()
        || now >= row.expires_at
        || now >= row.last_seen_at + policy.idle_timeout
        || (!row.matched_current
            && row
                .previous_token_valid_until
                .is_none_or(|until| now >= until))
    {
        return Ok(None);
    }
    if row.status != AccountStatus::Active {
        let mut tx = pool.begin().await?;
        let revoked = revoke_by_id(&mut *tx, row.id, RevokeReason::AccountInactive).await?;
        if revoked {
            audit::record(
                &mut *tx,
                AuditEvent::new(AuditActor::System, actions::SESSION_REVOKED)
                    .target("account", row.account_id)
                    .metadata(serde_json::json!({
                        "sessionId": row.id,
                        "reason": RevokeReason::AccountInactive.as_str(),
                    })),
            )
            .await?;
        }
        tx.commit().await?;
        return Ok(None);
    }

    let mut rotated = None;
    if row.matched_current && now - row.rotated_at >= policy.rotation_interval {
        let new_token = generate_token();
        let new_hash = token_hash(&new_token).expect("generated tokens are well-formed");
        let result = sqlx::query(
            "UPDATE sessions SET previous_token_hash = token_hash, \
                    previous_token_valid_until = now() + make_interval(secs => $3), \
                    token_hash = $2, rotated_at = now(), last_seen_at = now() \
             WHERE id = $4 AND token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(&hash)
        .bind(new_hash)
        .bind(policy.rotation_grace.whole_seconds() as f64)
        .bind(row.id)
        .execute(pool)
        .await?;
        // Zero rows: a concurrent request rotated first; the presented token
        // is now the previous one and stays valid for the grace period.
        if result.rows_affected() == 1 {
            let max_age = (row.expires_at - now).whole_seconds().max(0);
            rotated = Some((new_token, max_age));
        }
    } else if now - row.last_seen_at >= policy.touch_interval {
        sqlx::query(
            "UPDATE sessions SET last_seen_at = now() \
             WHERE id = $1 AND revoked_at IS NULL AND last_seen_at = $2",
        )
        .bind(row.id)
        .bind(row.last_seen_at)
        .execute(pool)
        .await?;
    }

    Ok(Some(ValidSession {
        session_id: row.id,
        account_id: row.account_id,
        username: row.username,
        display_name: row.display_name,
        role: row.role,
        language: row.language,
        expires_at: row.expires_at,
        rotated,
    }))
}

/// Revokes one session. Returns whether it was still unrevoked.
pub async fn revoke_by_id<'e, E>(
    executor: E,
    session_id: Uuid,
    reason: RevokeReason,
) -> Result<bool, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query(
        "UPDATE sessions SET revoked_at = now(), revoke_reason = $2 \
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(session_id)
    .bind(reason.as_str())
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Revokes the session a presented token belongs to: its current token
/// (regardless of idle/absolute expiry) or its previous token within the
/// rotation grace period. Superseded tokens cannot revoke the session.
/// Returns `(session_id, account_id)` when an
/// unrevoked session was revoked.
pub async fn revoke_by_token<'e, E>(
    executor: E,
    token: &str,
    reason: RevokeReason,
) -> Result<Option<(Uuid, Uuid)>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let Some(hash) = token_hash(token) else {
        return Ok(None);
    };
    sqlx::query_as(
        "UPDATE sessions SET revoked_at = now(), revoke_reason = $2 \
         WHERE (token_hash = $1 \
                OR (previous_token_hash = $1 AND previous_token_valid_until > now())) \
           AND revoked_at IS NULL \
         RETURNING id, account_id",
    )
    .bind(hash)
    .bind(reason.as_str())
    .fetch_optional(executor)
    .await
}

/// Revokes every unrevoked session of `account_id` except `except` (e.g. the
/// current session on password change). Returns the number revoked. Use it
/// in the same transaction as the change that requires it and record the
/// audit event there.
pub async fn revoke_all_for_account<'e, E>(
    executor: E,
    account_id: Uuid,
    except: Option<Uuid>,
    reason: RevokeReason,
) -> Result<u64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query(
        "UPDATE sessions SET revoked_at = now(), revoke_reason = $3 \
         WHERE account_id = $1 AND revoked_at IS NULL AND ($2::uuid IS NULL OR id <> $2)",
    )
    .bind(account_id)
    .bind(except)
    .bind(reason.as_str())
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}

/// Issues a new token for an existing session (e.g. after a password change)
/// without a grace period for the old one. Returns the new token and its
/// cookie `Max-Age`, or `None` if the session is gone or revoked.
pub async fn rotate_now<'e, E>(
    executor: E,
    session_id: Uuid,
) -> Result<Option<(String, i64)>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let token = generate_token();
    let hash = token_hash(&token).expect("generated tokens are well-formed");
    let max_age: Option<f64> = sqlx::query_scalar(
        "UPDATE sessions SET token_hash = $2, previous_token_hash = NULL, \
                previous_token_valid_until = NULL, rotated_at = now(), last_seen_at = now() \
         WHERE id = $1 AND revoked_at IS NULL \
         RETURNING extract(epoch FROM expires_at - now())::float8",
    )
    .bind(session_id)
    .bind(hash)
    .fetch_optional(executor)
    .await?;
    Ok(max_age.map(|secs| (token, secs.max(0.0) as i64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_32_random_bytes_base64url() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert_eq!(token_hash(&a).unwrap().len(), 32);
        assert_ne!(token_hash(&a), token_hash(&b));
        assert_eq!(token_hash("short"), None);
        assert_eq!(token_hash(&format!("{a}x")), None);
    }
}
