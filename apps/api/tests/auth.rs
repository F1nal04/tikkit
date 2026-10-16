//! Authentication integration tests against real PostgreSQL (skipped without TEST_DATABASE_URL).

use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tikkit_api::{AppState, app_with, auth::Settings};
use tower::ServiceExt;

use support::TestDb;

const PASSWORD: &str = "correct horse battery";

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    retry_after: Option<String>,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }
    /// `name=value` part of the session cookie.
    fn session(&self) -> String {
        let cookie = self.cookie.as_deref().expect("set-cookie");
        cookie.split(';').next().unwrap().to_owned()
    }
}

fn router(t: &TestDb, settings: Settings) -> Router {
    app_with(
        AppState {
            pool: t.pool.clone(),
        },
        settings,
    )
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Reply {
    call_from(app, method, path, body, cookie, [127, 0, 0, 1]).await
}

async fn call_from(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    ip: [u8; 4],
) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("x-requested-with", "tikkit");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let mut request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, 1234))));
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let header = |name| {
        parts
            .headers
            .get(name)
            .map(|v: &axum::http::HeaderValue| v.to_str().unwrap().to_owned())
    };
    let (cookie, retry_after) = (header("set-cookie"), header("retry-after"));
    let status = parts.status;
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    Reply {
        status,
        cookie,
        retry_after,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn setup_body(username: &str) -> Value {
    json!({ "username": username, "email": "admin@example.com", "password": PASSWORD, "language": "de" })
}

async fn login(app: &Router, username: &str, password: &str) -> Reply {
    let body = json!({ "username": username, "password": password });
    call(app, "POST", "/api/auth/login", Some(body), None).await
}

async fn account(t: &TestDb, username: &str, email: &str, role: &str) -> uuid::Uuid {
    sqlx::query_scalar(
        "INSERT INTO accounts (username, display_name, email, password_hash, role)
         VALUES ($1, $1, $2, $3, $4) RETURNING id",
    )
    .bind(username)
    .bind(email)
    .bind(tikkit_api::auth::hash_password(PASSWORD))
    .bind(role)
    .fetch_one(&t.pool)
    .await
    .unwrap()
}

async fn audit_types(t: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT event_type FROM audit_events ORDER BY id")
        .fetch_all(&t.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn setup_is_one_time_and_signs_in_the_first_admin() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    let r = call(&app, "GET", "/api/setup", None, None).await;
    assert_eq!(r.body, json!({ "setup_required": true }));

    let bad = json!({ "username": "a b", "email": "nope", "password": "short", "language": "en" });
    let r = call(&app, "POST", "/api/setup", Some(bad), None).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        r.body["error"]["details"]["fields"],
        json!({ "username": ["invalid"], "email": ["invalid"], "password": ["too_short"] })
    );

    let r = call(&app, "POST", "/api/setup", Some(setup_body(" Root ")), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["username"], "Root");
    assert_eq!(r.body["role"], "admin");
    assert_eq!(r.body["language"], "de");
    let me = call(&app, "GET", "/api/auth/me", None, Some(&r.session())).await;
    assert_eq!(me.body["id"], r.body["id"]);

    let r = call(&app, "GET", "/api/setup", None, None).await;
    assert_eq!(r.body, json!({ "setup_required": false }));
    let r = call(&app, "POST", "/api/setup", Some(setup_body("other")), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "setup_already_completed");
    assert_eq!(audit_types(&t).await, ["setup_completed"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_setup_creates_exactly_one_admin() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    let attempts = (0..20u8).map(|i| {
        let app = app.clone();
        tokio::spawn(async move {
            let body = setup_body(&format!("admin{i}"));
            call_from(&app, "POST", "/api/setup", Some(body), None, [10, 0, 0, i]).await
        })
    });
    let mut statuses = Vec::new();
    for attempt in attempts {
        statuses.push(attempt.await.unwrap().status);
    }
    assert_eq!(statuses.iter().filter(|s| **s == StatusCode::OK).count(), 1);
    assert!(
        statuses
            .iter()
            .all(|s| *s == StatusCode::OK || *s == StatusCode::CONFLICT)
    );
    let (admins, completed): (i64, bool) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM accounts WHERE role = 'admin'),
                (SELECT setup_completed_at IS NOT NULL FROM organization)",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!((admins, completed), (1, true));
    assert_eq!(audit_types(&t).await, ["setup_completed"]);
}

#[tokio::test]
async fn login_sets_a_hardened_cookie_and_normalizes_usernames() {
    let Some(t) = TestDb::new().await else { return };
    account(&t, "alice", "a@example.com", "operator").await;
    let r = login(&router(&t, Settings::default()), "  ALICE ", PASSWORD).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["username"], "alice");
    assert_eq!(r.body["role"], "operator");
    let cookie = r.cookie.unwrap();
    assert!(cookie.starts_with("tikkit_session="));
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=1209600"] {
        assert!(cookie.contains(attribute), "{cookie}");
    }
    assert!(!cookie.contains("Secure"), "{cookie}");

    let production = Settings {
        secure_cookie: true,
        ..Settings::default()
    };
    let r = login(&router(&t, production), "alice", PASSWORD).await;
    assert!(r.cookie.unwrap().ends_with("; Secure"));
    // Only 32-byte SHA-256 hashes are stored, never the token.
    let stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE length(token_hash) = 32")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(stored, 2);
}

#[tokio::test]
async fn login_failures_are_indistinguishable() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    let id = account(&t, "bob", "b@example.com", "user").await;
    account(&t, "carol", "c@example.com", "user").await;
    sqlx::query("UPDATE accounts SET is_active = false WHERE username = 'carol'")
        .execute(&t.pool)
        .await
        .unwrap();
    let replies = [
        login(&app, "nobody", PASSWORD).await,
        login(&app, "bob", "wrong password!!").await,
        login(&app, "carol", PASSWORD).await,
    ];
    for r in &replies {
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.code(), "invalid_credentials");
        assert_eq!(
            r.body["error"]["message"],
            replies[0].body["error"]["message"]
        );
        assert!(r.cookie.is_none());
    }
    let failed: Vec<(Option<uuid::Uuid>, Value)> = sqlx::query_as(
        "SELECT subject_account_id, metadata FROM audit_events WHERE event_type = 'login_failed' ORDER BY id",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(failed.len(), 3);
    assert_eq!(failed[1].0, Some(id));
    assert_eq!(failed[1].1["reason"], "wrong_password");
    assert!(
        !failed
            .iter()
            .any(|f| f.1.to_string().contains("wrong password"))
    );
}

#[tokio::test]
async fn me_logout_and_deactivation() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    let r = call(&app, "GET", "/api/auth/me", None, None).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::UNAUTHORIZED, "unauthorized")
    );
    let r = call(
        &app,
        "GET",
        "/api/auth/me",
        None,
        Some("tikkit_session=forged"),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    account(&t, "dave", "d@example.com", "user").await;
    let first = login(&app, "dave", PASSWORD).await.session();
    let second = login(&app, "dave", PASSWORD).await.session();
    assert_ne!(first, second, "every login issues a new session");
    let r = call(&app, "GET", "/api/auth/me", None, Some(&first)).await;
    assert_eq!(r.body["username"], "dave");

    let r = call(&app, "POST", "/api/auth/logout", None, Some(&first)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(r.cookie.unwrap().contains("Max-Age=0"));
    let r = call(&app, "GET", "/api/auth/me", None, Some(&first)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Idempotent.
    let r = call(&app, "POST", "/api/auth/logout", None, Some(&first)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    // Deactivation kills the remaining session on its next request.
    assert_eq!(
        call(&app, "GET", "/api/auth/me", None, Some(&second))
            .await
            .status,
        StatusCode::OK
    );
    sqlx::query("UPDATE accounts SET is_active = false WHERE username = 'dave'")
        .execute(&t.pool)
        .await
        .unwrap();
    let r = call(&app, "GET", "/api/auth/me", None, Some(&second)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(audit_types(&t).await.contains(&"logout".to_owned()));
}

#[tokio::test]
async fn expired_sessions_are_rejected_and_old_sessions_rotate() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    account(&t, "erin", "e@example.com", "user").await;
    let expired = login(&app, "erin", PASSWORD).await.session();
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&t.pool)
        .await
        .unwrap();
    let r = call(&app, "GET", "/api/auth/me", None, Some(&expired)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let old = login(&app, "erin", PASSWORD).await.session();
    let r = call(&app, "GET", "/api/auth/me", None, Some(&old)).await;
    assert!(r.cookie.is_none(), "young sessions are not rotated");
    sqlx::query("UPDATE sessions SET created_at = now() - interval '25 hours' WHERE revoked_at IS NULL AND expires_at > now()")
        .execute(&t.pool)
        .await
        .unwrap();
    let r = call(&app, "GET", "/api/auth/me", None, Some(&old)).await;
    assert_eq!(r.status, StatusCode::OK);
    let new = r.session();
    assert_ne!(new, old);
    assert!(r.cookie.unwrap().contains("HttpOnly"));
    assert_eq!(
        call(&app, "GET", "/api/auth/me", None, Some(&old))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let r = call(&app, "GET", "/api/auth/me", None, Some(&new)).await;
    assert_eq!((r.status, r.cookie), (StatusCode::OK, None));
}

#[tokio::test]
async fn password_change_revokes_every_other_session() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    account(&t, "frank", "f@example.com", "specialist").await;
    let current = login(&app, "frank", PASSWORD).await.session();
    let other = login(&app, "frank", PASSWORD).await.session();

    let wrong =
        json!({ "current_password": "not my password", "new_password": "a brand new secret" });
    let r = call(
        &app,
        "POST",
        "/api/auth/password",
        Some(wrong),
        Some(&current),
    )
    .await;
    assert_eq!(
        r.body["error"]["details"]["fields"],
        json!({ "current_password": ["incorrect"] })
    );
    let weak = json!({ "current_password": PASSWORD, "new_password": "password1234" });
    let r = call(
        &app,
        "POST",
        "/api/auth/password",
        Some(weak),
        Some(&current),
    )
    .await;
    assert_eq!(
        r.body["error"]["details"]["fields"],
        json!({ "new_password": ["too_common"] })
    );

    let ok = json!({ "current_password": PASSWORD, "new_password": "a brand new secret" });
    let r = call(&app, "POST", "/api/auth/password", Some(ok), Some(&current)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let rotated = r.session();
    for old in [&current, &other] {
        assert_eq!(
            call(&app, "GET", "/api/auth/me", None, Some(old))
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        call(&app, "GET", "/api/auth/me", None, Some(&rotated))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, "frank", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "frank", "a brand new secret").await.status,
        StatusCode::OK
    );
    let types = audit_types(&t).await;
    assert!(
        types.contains(&"password_changed".to_owned())
            && types.contains(&"session_revoked".to_owned())
    );
}

#[tokio::test]
async fn state_changes_require_the_csrf_header() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"username":"x","password":"y"}"#))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "csrf_rejected");
    // Safe methods need no header.
    let response = app
        .oneshot(Request::get("/api/setup").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn login_is_rate_limited_per_username_and_ip() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    for _ in 0..5 {
        assert_eq!(
            login(&app, "grace", "nope nope nope").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let r = login(&app, "Grace", "nope nope nope").await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert!(r.retry_after.unwrap().parse::<u64>().unwrap() >= 1);

    // Per IP: 10 per minute across usernames (5 used above, the 6th was counted too).
    for i in 0..4 {
        let body = json!({ "username": format!("u{i}"), "password": "x" });
        let r = call(&app, "POST", "/api/auth/login", Some(body), None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let body = json!({ "username": "someone", "password": "x" });
    let r = call(&app, "POST", "/api/auth/login", Some(body.clone()), None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = call_from(
        &app,
        "POST",
        "/api/auth/login",
        Some(body),
        None,
        [10, 9, 9, 9],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn accounts_sharing_an_email_stay_separate() {
    let Some(t) = TestDb::new().await else { return };
    let app = router(&t, Settings::default());
    let ops = account(&t, "heidi", "shared@example.com", "operator").await;
    let admin = account(&t, "heidi.admin", "shared@example.com", "admin").await;
    let a = login(&app, "heidi", PASSWORD).await.session();
    let b = login(&app, "heidi.admin", PASSWORD).await.session();
    let me_a = call(&app, "GET", "/api/auth/me", None, Some(&a)).await.body;
    let me_b = call(&app, "GET", "/api/auth/me", None, Some(&b)).await.body;
    assert_eq!(
        (me_a["id"].as_str(), me_a["role"].as_str()),
        (Some(ops.to_string().as_str()), Some("operator"))
    );
    assert_eq!(
        (me_b["id"].as_str(), me_b["role"].as_str()),
        (Some(admin.to_string().as_str()), Some("admin"))
    );
    // Logging one out leaves the other signed in.
    call(&app, "POST", "/api/auth/logout", None, Some(&a)).await;
    assert_eq!(
        call(&app, "GET", "/api/auth/me", None, Some(&b))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn audit_events_are_append_only() {
    let Some(t) = TestDb::new().await else { return };
    login(&router(&t, Settings::default()), "nobody", PASSWORD).await;
    for statement in [
        "UPDATE audit_events SET event_type = 'x'",
        "DELETE FROM audit_events",
    ] {
        let err = sqlx::query(statement).execute(&t.pool).await.unwrap_err();
        assert_eq!(err.as_database_error().unwrap().code().unwrap(), "23001");
    }
}

// Copy of the disposable-database helper from tests/db.rs; deduplicate once tests/support exists.
mod support {
    use std::str::FromStr;

    use sqlx::{
        Connection, Executor, PgConnection, PgPool,
        postgres::{PgConnectOptions, PgPoolOptions},
    };
    use tikkit_api::db;
    use uuid::Uuid;

    pub struct TestDb {
        pub pool: PgPool,
        admin: PgConnectOptions,
        name: String,
    }

    impl TestDb {
        pub async fn new() -> Option<Self> {
            dotenvy::dotenv().ok();
            let Ok(url) =
                std::env::var("TEST_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
            else {
                eprintln!("skipping: set TEST_DATABASE_URL to run PostgreSQL integration tests");
                return None;
            };
            let admin = PgConnectOptions::from_str(&url).expect("valid TEST_DATABASE_URL");
            let name = format!("tikkit_test_{}", Uuid::new_v4().simple());
            let mut conn = PgConnection::connect_with(&admin).await.unwrap();
            conn.execute(format!("CREATE DATABASE {name}").as_str())
                .await
                .unwrap();
            let pool = PgPoolOptions::new()
                .max_connections(30)
                .connect_with(admin.clone().database(&name))
                .await
                .unwrap();
            db::migrate(&pool).await.unwrap();
            Some(Self { pool, admin, name })
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            let (admin, name) = (self.admin.clone(), self.name.clone());
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let mut conn = PgConnection::connect_with(&admin).await.unwrap();
                        conn.execute(
                            format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str(),
                        )
                        .await
                        .unwrap();
                    })
            })
            .join()
            .unwrap();
        }
    }
}
