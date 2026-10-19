//! Durable auto-close of staff resolutions the requester never answered. See
//! docs/lifecycle.md.
//!
//! The only state is `tickets.confirmation_deadline`, snapshotted by the
//! proposal itself, so a restart or an outage simply finds the overdue rows
//! again. Closing goes through [`lifecycle::transition`] as the system actor,
//! which writes the one `confirmation_timeout` event (and, through the
//! `ticket_events` trigger, its notification rows) in the same transaction.

use sqlx::PgPool;

use crate::{
    api::ApiError,
    lifecycle::{self, By, Transition},
    notifications::poll_loop,
};

pub async fn worker_loop(pool: PgPool) {
    poll_loop("auto_close", || close_one(&pool)).await
}

/// Closes one overdue proposal and returns whether it did. `SKIP LOCKED` leaves
/// rows that a concurrent worker, a requester decision, or a staff revision holds
/// to their holder; the proposal version is read under the lock, so a replaced
/// proposal (new version, new deadline) is never closed by an old timer.
pub async fn close_one(pool: &PgPool) -> Result<bool, ApiError> {
    let mut tx = pool.begin().await?;
    let due: Option<(uuid::Uuid, i32)> = sqlx::query_as(
        "SELECT id, proposal_version FROM tickets
         WHERE status = 'awaiting_confirmation' AND confirmation_deadline <= clock_timestamp()
         ORDER BY confirmation_deadline, id LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, proposal_version)) = due else {
        return Ok(false);
    };
    let t = Transition::ConfirmationTimeout { proposal_version };
    lifecycle::transition(&mut tx, id, By::System, &t).await?;
    tx.commit().await?;
    Ok(true)
}
