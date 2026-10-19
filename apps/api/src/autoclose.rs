//! Durable auto-close of staff resolutions the requester never answered. See
//! docs/lifecycle.md.
//!
//! The only state is `tickets.confirmation_deadline`, snapshotted by the
//! proposal itself, so a restart or an outage simply finds the overdue rows
//! again. Closing goes through [`lifecycle::transition`] as the system actor,
//! which writes the one `confirmation_timeout` event (and, through the
//! `ticket_events` trigger, its notification rows) in the same transaction.
//!
//! A transition that fails rolls back and is not retried until the current
//! drain cycle has nothing else due. Otherwise the oldest failing ticket would
//! stay at the head of `ORDER BY confirmation_deadline` and the ones behind it
//! would never run.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    api::ApiError,
    lifecycle::{self, By, Transition},
    notifications::poll_loop,
};

pub async fn worker_loop(pool: PgPool) {
    let skip = Arc::new(Mutex::new(Vec::<Uuid>::new()));
    poll_loop("auto_close", || {
        let pool = pool.clone();
        let skip = Arc::clone(&skip);
        async move {
            // Copy the ids out so the mutex is not held across the query.
            let mut skipped = skip.lock().await.clone();
            let result = close_due(&pool, &mut skipped).await;
            *skip.lock().await = skipped;
            result
        }
    })
    .await;
}

/// Closes one overdue proposal and returns whether it did. See [`close_due`].
pub async fn close_one(pool: &PgPool) -> Result<bool, ApiError> {
    try_close(pool, &[]).await.map_err(into_error)
}

/// One step of a drain cycle. `skipped` tickets are left out of this cycle
/// after their transition failed. `Ok(true)` means keep draining (a ticket was
/// closed, or one was skipped). `Ok(false)` means nothing else is due; `skipped`
/// is cleared so the next cycle, after the poll sleep, can retry them.
pub async fn close_due(pool: &PgPool, skipped: &mut Vec<Uuid>) -> Result<bool, ApiError> {
    match try_close(pool, skipped).await {
        Ok(true) => Ok(true),
        Ok(false) => {
            skipped.clear();
            Ok(false)
        }
        Err(CloseError::Ticket { id, error }) => {
            // The code only: database errors can carry statement text.
            tracing::error!(
                worker = "auto_close",
                ticket_id = %id,
                error_code = error_code(&error),
                "auto-close transition failed; skipping ticket for this cycle"
            );
            skipped.push(id);
            Ok(true)
        }
        Err(CloseError::Unavailable(error)) => Err(error),
    }
}

enum CloseError {
    /// The due ticket's transition failed and the transaction was rolled back.
    Ticket { id: Uuid, error: ApiError },
    /// Selecting or committing failed before a ticket could be set aside.
    Unavailable(ApiError),
}

fn into_error(error: CloseError) -> ApiError {
    match error {
        CloseError::Ticket { error, .. } | CloseError::Unavailable(error) => error,
    }
}

/// Stable machine code, matching the API envelope. Not the database message.
fn error_code(error: &ApiError) -> &'static str {
    match error {
        ApiError::Conflict => "conflict",
        ApiError::Forbidden => "forbidden",
        ApiError::NotFound => "not_found",
        ApiError::Validation(_) => "validation_failed",
        _ => "internal",
    }
}

async fn try_close(pool: &PgPool, skip: &[Uuid]) -> Result<bool, CloseError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| CloseError::Unavailable(error.into()))?;
    let due: Option<(Uuid, i32)> = sqlx::query_as(
        "SELECT id, proposal_version FROM tickets
         WHERE status = 'awaiting_confirmation'
           AND confirmation_deadline <= clock_timestamp()
           AND NOT (id = ANY($1::uuid[]))
         ORDER BY confirmation_deadline, id
         LIMIT 1
         FOR UPDATE SKIP LOCKED",
    )
    .bind(skip)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| CloseError::Unavailable(error.into()))?;
    let Some((id, proposal_version)) = due else {
        return Ok(false);
    };
    let t = Transition::ConfirmationTimeout { proposal_version };
    if let Err(error) = lifecycle::transition(&mut tx, id, By::System, &t).await {
        // Roll back before the caller records the skip, so this lock is gone
        // and the next ticket can be claimed.
        drop(tx);
        return Err(CloseError::Ticket { id, error });
    }
    tx.commit()
        .await
        .map_err(|error| CloseError::Unavailable(error.into()))?;
    Ok(true)
}
