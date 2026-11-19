//! Restore-time commands (docs/backup-restore.md). They run as `tikkit-api
//! restore-check` and `tikkit-api suppress-restored-email --before <time>`, only
//! while `RESTORE_MODE` is on.

use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::auth::audit;

/// `last_error` of rows suppressed by [`suppress_pending_email`].
pub const SUPPRESSED_ON_RESTORE: &str = "suppressed_on_restore";

/// `none`, `readable` or `unreadable` for each kind of credential encrypted with
/// `ENCRYPTION_KEY`, for the line `tikkit-api credentials-check` prints.
pub async fn credentials_status(
    pool: &PgPool,
    key: &[u8; 32],
) -> Result<(&'static str, &'static str), sqlx::Error> {
    let (stored, readable) = crate::ai_config::stored_keys_readable(pool, key).await?;
    let ai = match (stored, readable) {
        (0, _) => "none",
        (n, r) if n == r => "readable",
        _ => "unreadable",
    };
    let smtp = match crate::mail::load(pool, key).await {
        Err(crate::mail::MailError::NotConfigured) => "none",
        Err(crate::mail::MailError::Unreadable) => "unreadable",
        Err(crate::mail::MailError::Store) => return Err(sqlx::Error::PoolClosed),
        _ => "readable",
    };
    Ok((ai, smtp))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Suppressed {
    /// Pending email rows created at or before the cutoff, now `suppressed`.
    pub email_suppressed: i64,
    /// Pending email rows created after the cutoff, left to be delivered.
    pub email_pending_after_cutoff: i64,
    /// Pending in-portal rows, which are never touched.
    pub in_app_pending: i64,
}

/// A relay may have accepted a message after the backup snapshot that the restored
/// database still shows as pending, and a worker would send it again. This marks
/// every pending **email** row created at or before `cutoff` (the backup time) as
/// `suppressed`: never sent, kept for history, counted. In-portal rows (the inbox)
/// are unaffected. One audit event records the cutoff and the counts, no content.
pub async fn suppress_pending_email(
    pool: &PgPool,
    cutoff: OffsetDateTime,
) -> Result<Suppressed, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let email_suppressed = sqlx::query(
        "UPDATE notification_outbox
         SET status = 'suppressed', last_error = $2, completed_at = clock_timestamp()
         WHERE status = 'pending' AND channel = 'email' AND created_at <= $1",
    )
    .bind(cutoff)
    .bind(SUPPRESSED_ON_RESTORE)
    .execute(&mut *tx)
    .await?
    .rows_affected() as i64;
    let (email_pending_after_cutoff, in_app_pending): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE channel = 'email'), count(*) FILTER (WHERE channel = 'in_app')
         FROM notification_outbox WHERE status = 'pending'",
    )
    .fetch_one(&mut *tx)
    .await?;
    audit(
        &mut *tx,
        "notifications_suppressed_on_restore",
        None,
        None,
        json!({
            "cutoff": cutoff.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
            "email_suppressed": email_suppressed,
            "email_pending_after_cutoff": email_pending_after_cutoff,
            "in_app_pending": in_app_pending,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(Suppressed {
        email_suppressed,
        email_pending_after_cutoff,
        in_app_pending,
    })
}
