//! Liveness and readiness probes (issue #8).
//!
//! Liveness (`/api/health`, `/api/health/live`) never touches dependencies,
//! so a slow database cannot get a healthy process restarted. Readiness
//! (`/api/health/ready`) checks PostgreSQL and schema compatibility with a
//! short timeout and answers 503 when traffic should not be routed here.
//! Optional integrations (AI, SMTP) are not readiness dependencies.

use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::{
    db::{self, SchemaProblem},
    state::AppState,
};

/// Upper bound for each readiness check.
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
}

pub async fn live() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseCheck {
    Ok,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaCheck {
    Ok,
    PendingMigrations,
    Incompatible,
    /// Not checked because the database is unavailable.
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessChecks {
    pub database: DatabaseCheck,
    pub schema: SchemaCheck,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Readiness {
    /// `ready` or `not_ready`.
    pub status: &'static str,
    pub checks: ReadinessChecks,
}

pub async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let checks = check(&state).await;
    let is_ready = checks.database == DatabaseCheck::Ok && checks.schema == SchemaCheck::Ok;
    let (code, status) = if is_ready {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not_ready")
    };
    (code, Json(Readiness { status, checks }))
}

async fn check(state: &AppState) -> ReadinessChecks {
    let ping = tokio::time::timeout(
        READINESS_TIMEOUT,
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.db),
    )
    .await;
    match ping {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            let detail = state.config.database_url.scrub(&error.to_string());
            tracing::warn!(error = %detail, "readiness: database unavailable");
            return unavailable();
        }
        Err(_) => {
            tracing::warn!("readiness: database check timed out");
            return unavailable();
        }
    }

    let schema = match tokio::time::timeout(READINESS_TIMEOUT, db::schema_status(&state.db)).await {
        Ok(Ok(status)) => match status.problem() {
            None => SchemaCheck::Ok,
            Some(problem @ SchemaProblem::Pending(_)) => {
                tracing::warn!(%problem, "readiness: schema not ready");
                SchemaCheck::PendingMigrations
            }
            Some(problem @ SchemaProblem::Incompatible(_)) => {
                tracing::error!(%problem, "readiness: schema not ready");
                SchemaCheck::Incompatible
            }
        },
        Ok(Err(error)) => {
            let detail = state.config.database_url.scrub(&error.to_string());
            tracing::warn!(error = %detail, "readiness: schema check failed");
            return unavailable();
        }
        Err(_) => {
            tracing::warn!("readiness: schema check timed out");
            return unavailable();
        }
    };
    ReadinessChecks {
        database: DatabaseCheck::Ok,
        schema,
    }
}

fn unavailable() -> ReadinessChecks {
    ReadinessChecks {
        database: DatabaseCheck::Unavailable,
        schema: SchemaCheck::Unknown,
    }
}
