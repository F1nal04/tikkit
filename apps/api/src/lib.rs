use std::time::Duration;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Serialize;
use sqlx::PgPool;

pub mod config;
pub mod db;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

#[derive(Serialize)]
struct Readiness {
    status: &'static str,
    checks: Checks,
}

#[derive(Serialize)]
struct Checks {
    database: &'static str,
    migrations: &'static str,
}

/// Readiness: the database answers and its schema matches this build.
/// Reports stable check identifiers only, never connection details.
async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let probe = async {
        sqlx::query("SELECT 1").execute(&state.pool).await?;
        db::migration_status(&state.pool).await
    };
    let checks = match tokio::time::timeout(Duration::from_secs(3), probe).await {
        Ok(Ok(migrations)) => Checks {
            database: "ok",
            migrations,
        },
        _ => Checks {
            database: "unavailable",
            migrations: "unknown",
        },
    };
    if checks.migrations == "ok" {
        (
            StatusCode::OK,
            Json(Readiness {
                status: "ready",
                checks,
            }),
        )
    } else {
        let body = Readiness {
            status: "unavailable",
            checks,
        };
        (StatusCode::SERVICE_UNAVAILABLE, Json(body))
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/health",
            get(|| async { Json(Health { status: "ok" }) }),
        )
        .route("/api/ready", get(ready))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    fn lazy_state() -> super::AppState {
        // Never connects unless a handler touches the database.
        let pool = PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(200))
            .connect_lazy("postgres://tikkit@127.0.0.1:1/tikkit")
            .unwrap();
        super::AppState { pool }
    }

    #[tokio::test]
    async fn health_returns_json_status() {
        let response = super::app(lazy_state())
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/json");
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload, serde_json::json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn ready_is_unavailable_without_database() {
        let response = super::app(lazy_state())
            .oneshot(Request::get("/api/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let expected = serde_json::json!({
            "status": "unavailable",
            "checks": { "database": "unavailable", "migrations": "unknown" }
        });
        assert_eq!(payload, expected);
    }
}
