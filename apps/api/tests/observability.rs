//! Private exporter, request correlation and redacted operational diagnostics.
mod support;

use axum::{body::Body, http::Request};
use serde_json::Value;
use support::{Logs, TestDb, metrics_text, test_state};
use tikkit_api::{app, autoclose};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn public_routes_do_not_expose_metrics_and_scrape_reports_an_outage() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let state = test_state(&TestDb::pool_for(&db).await);
    let app = app(state.clone());
    for path in ["/metrics", "/api/metrics"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }
    let response = app
        .clone()
        .oneshot(Request::get("/api/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = metrics_text(&state).await;
    assert!(text.contains("tikkit_database_ready 1\n"), "{text}");
    assert!(text.contains("tikkit_worker_snapshot_up 1\n"), "{text}");
    assert!(text.contains("tikkit_http_request_duration_seconds_count{status_class=\"2xx\"} 1\n"));
    assert!(text.contains(
        "tikkit_http_request_duration_seconds_bucket{status_class=\"4xx\",le=\"+Inf\"} 2\n"
    ));

    // Simulate connection loss on the application's pool; the test DB stays
    // available to build a fresh pool as it would be after restart.
    state.pool.close().await;
    let response = app
        .oneshot(Request::get("/api/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert!(
        autoclose::close_due(&state.pool, &mut Vec::new(), &state.metrics)
            .await
            .is_err()
    );
    let text = metrics_text(&state).await;
    assert!(text.contains("tikkit_database_ready 0\n"));
    assert!(text.contains("tikkit_worker_snapshot_up 0\n"));
    assert!(!text.contains("tikkit_worker_due_jobs"));
    assert!(text.contains("tikkit_worker_failures_total{worker=\"auto_close\"} 1\n"));
    assert!(text.contains("tikkit_http_request_duration_seconds_count{status_class=\"5xx\"} 1\n"));
    let recovered = tikkit_api::AppState {
        pool: db.pool.clone(),
        ..state
    };
    assert!(
        metrics_text(&recovered)
            .await
            .contains("tikkit_database_ready 1\n")
    );
}

#[tokio::test]
async fn request_logs_use_route_templates_and_replace_header_content() {
    let logs = Logs::capture();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://tikkit@127.0.0.1:1/tikkit")
        .unwrap();
    let public = app(test_state(&pool));
    let id = Uuid::new_v4().to_string();
    let private = "private-request-value";
    let request = Request::get(format!("/api/tickets/{private}?token={private}"))
        .header("x-request-id", &id)
        .header("cookie", private)
        .body(Body::empty())
        .unwrap();
    let response = public.clone().oneshot(request).await.unwrap();
    assert_eq!(response.headers()["x-request-id"], id);
    let request = Request::get(format!("/{private}"))
        .header("x-request-id", private)
        .body(Body::empty())
        .unwrap();
    let response = public.oneshot(request).await.unwrap();
    assert!(Uuid::parse_str(response.headers()["x-request-id"].to_str().unwrap()).is_ok());
    let text = logs.text();
    assert!(!text.contains(private), "{text}");
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["span"]["route"] == "/api/tickets/{id}"
                && event["span"]["request_id"] == id)
    );
    assert!(
        events
            .iter()
            .any(|event| event["span"]["route"] == "unmatched"
                && event["span"]["request_id"].as_str()
                    == response.headers()["x-request-id"].to_str().ok())
    );
}

#[test]
fn production_logging_filters_dependencies_and_reports_safe_causes() {
    const CHILD: &str = "TIKKIT_LOGGING_TEST_CHILD";
    const PRIVATE: &str = "private-diagnostic-payload";
    if std::env::var_os(CHILD).is_some() {
        tikkit_api::telemetry::init_logging(true);
        tracing::error!(target: "sqlx::query", PRIVATE);
        let _ = tikkit_api::api::ApiError::from(sqlx::Error::PoolTimedOut);
        let _ = tikkit_api::api::ApiError::from(sqlx::Error::Protocol(PRIVATE.to_owned()));
        tikkit_api::db::log_migration_failure(&sqlx::migrate::MigrateError::ExecuteMigration(
            sqlx::Error::Io(std::io::Error::other(PRIVATE)),
            25,
        ));
        let _ = std::panic::catch_unwind(|| panic!("{PRIVATE}"));
        return;
    }

    // The child uses the real global subscriber and panic hook in isolation.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "production_logging_filters_dependencies_and_reports_safe_causes",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("RUST_LOG", "warn,sqlx=trace")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(!stdout.contains(PRIVATE) && !stderr.contains(PRIVATE));
    let events: Vec<Value> = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    for kind in ["pool_timed_out", "protocol"] {
        assert!(
            events
                .iter()
                .any(|event| event["fields"]["error_kind"] == kind),
            "{stdout}\n{stderr}"
        );
    }
    assert!(
        events.iter().any(|event| {
            let fields = &event["fields"];
            fields["message"] == "database migration failed"
                && fields["database_error_kind"] == "io"
                && fields["migration_version"] == 25
        }),
        "{stdout}\n{stderr}"
    );
    assert!(
        events.iter().any(|event| {
            let fields = &event["fields"];
            fields["message"] == "process task panicked"
                && fields["file"]
                    .as_str()
                    .is_some_and(|file| file.ends_with("observability.rs"))
                && fields["line"].as_u64().is_some_and(|line| line > 0)
        }),
        "{stdout}\n{stderr}"
    );
}
