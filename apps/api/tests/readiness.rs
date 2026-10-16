//! Liveness/readiness probes and startup schema checks (issue #8).

mod common;

use std::time::{Duration, Instant};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use common::*;
use serde_json::{Value, json};
use tikkit_api::{
    AppState,
    db::{self, DbError, SchemaProblem},
};
use tower::ServiceExt;

async fn get(app: axum::Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

const UNREACHABLE: &str = "postgres://tikkit:sup3r-secret@127.0.0.1:1/tikkit";

fn unreachable_state() -> AppState {
    let config = config_with_database_url(UNREACHABLE);
    AppState::new(db::connect_lazy(&config), config)
}

#[tokio::test]
async fn ready_with_migrated_database() {
    let test = TestDb::new().await;
    let (status, body) = get(test.app(), "/api/health/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "status": "ready", "checks": { "database": "ok", "schema": "ok" } })
    );
    let (status, body) = get(test.app(), "/api/health").await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "status": "ok" })));
}

#[tokio::test]
async fn not_ready_when_database_unreachable_but_still_live() {
    let state = unreachable_state();
    let started = Instant::now();
    let (status, body) = get(tikkit_api::app(state.clone()), "/api/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({ "status": "not_ready", "checks": { "database": "unavailable", "schema": "unknown" } })
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "readiness is bounded"
    );
    assert!(!body.to_string().contains("sup3r-secret"));

    for path in ["/api/health", "/api/health/live"] {
        let (status, _) = get(tikkit_api::app(state.clone()), path).await;
        assert_eq!(status, StatusCode::OK, "liveness never needs the database");
    }
}

#[tokio::test]
async fn not_ready_with_pending_migrations() {
    let test = TestDb::empty().await;
    let (status, body) = get(test.app(), "/api/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["checks"]["schema"], "pending_migrations");

    // Without auto-migrate, startup refuses with instructions.
    let error = db::prepare_schema(&test.pool, false).await.unwrap_err();
    assert!(matches!(error, DbError::Schema(SchemaProblem::Pending(_))));
    assert!(error.to_string().contains("tikkit-api migrate"));

    db::prepare_schema(&test.pool, true).await.unwrap();
    let (status, _) = get(test.app(), "/api/health/ready").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn newer_or_modified_schema_is_incompatible() {
    let test = TestDb::new().await;
    // Simulate a database migrated by a newer release.
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'from the future', true, '\\x00', 0)",
    )
    .execute(&test.pool)
    .await
    .unwrap();
    let (status, body) = get(test.app(), "/api/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["checks"]["schema"], "incompatible");

    for auto_migrate in [true, false] {
        let error = db::prepare_schema(&test.pool, auto_migrate)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("9999"), "{error}");
        assert!(error.to_string().contains("newer"), "{error}");
    }
    assert!(matches!(
        db::migrate(&test.pool).await,
        Err(DbError::Schema(_))
    ));

    // An edited migration (checksum mismatch) is refused as well.
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 9999")
        .execute(&test.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x01' WHERE version = 1")
        .execute(&test.pool)
        .await
        .unwrap();
    let error = db::prepare_schema(&test.pool, true).await.unwrap_err();
    assert!(error.to_string().contains("never be edited"), "{error}");
}

#[tokio::test]
async fn startup_connection_to_unreachable_database_fails_with_redacted_message() {
    let config = tikkit_api::Config::from_source(|name| match name {
        "DATABASE_URL" => Some(UNREACHABLE.to_owned()),
        "DATABASE_STARTUP_TIMEOUT_SECS" => Some("1".to_owned()),
        _ => None,
    })
    .unwrap();
    let started = Instant::now();
    let error = db::connect_with_retry(&config).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    let message = error.to_string();
    assert!(
        matches!(error, DbError::Unavailable { attempts, .. } if attempts >= 2),
        "{message}"
    );
    assert!(message.contains("127.0.0.1:1/tikkit"), "{message}");
    assert!(!message.contains("sup3r-secret"), "{message}");
}
