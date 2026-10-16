//! Authentication, sessions and first setup (issue #9).

mod common;

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
};
use common::*;
use serde_json::{Value, json};
use tikkit_api::auth::{AuthSettings, RateLimits, RevokeReason, sessions};
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery staple";

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }

    fn set_cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().to_owned())
    }

    /// The token set by this response (`None` when no cookie or a clear).
    fn token(&self) -> Option<String> {
        let cookie = self.set_cookie()?;
        let value = cookie.split(';').next()?.split_once('=')?.1.to_owned();
        (!value.is_empty()).then_some(value)
    }

    fn clears_cookie(&self) -> bool {
        self.set_cookie()
            .is_some_and(|c| c.contains("Max-Age=0") && c.split(';').next().unwrap().ends_with('='))
    }

    fn cache_control(&self) -> Option<&str> {
        self.headers
            .get(header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap())
    }

    /// Error body without the per-request id.
    fn error_without_request_id(&self) -> Value {
        let mut body = self.body.clone();
        body["error"]
            .as_object_mut()
            .expect("error body")
            .remove("requestId");
        body
    }
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    Reply {
        status,
        headers,
        body,
    }
}

fn post(path: &str, body: Option<Value>, cookie: Option<&str>) -> Request<Body> {
    post_from(path, body, cookie, Some(DEV_ORIGIN))
}

fn post_from(
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    origin: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::post(path);
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(token) = cookie {
        builder = builder.header(header::COOKIE, format!("tikkit_session={token}"));
    }
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::get(path);
    if let Some(token) = cookie {
        builder = builder.header(header::COOKIE, format!("tikkit_session={token}"));
    }
    builder.body(Body::empty()).unwrap()
}

async fn login(app: &Router, username: &str, password: &str) -> Reply {
    send(
        app,
        post(
            "/api/v1/auth/login",
            Some(json!({ "username": username, "password": password })),
            None,
        ),
    )
    .await
}

/// Logs in and returns the session token.
async fn login_token(app: &Router, username: &str) -> String {
    let reply = login(app, username, PASSWORD).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.token().expect("login sets a session cookie")
}

async fn session(app: &Router, token: &str) -> Reply {
    send(app, get("/api/v1/auth/session", Some(token))).await
}

fn setup_body(username: &str) -> Value {
    json!({
        "username": username,
        "displayName": "Ada Admin",
        "email": "it@example.test",
        "password": PASSWORD,
        "language": "de",
    })
}

async fn audit_actions(test: &TestDb) -> Vec<(String, String, Value)> {
    sqlx::query_as("SELECT action, actor_kind, metadata FROM audit_events ORDER BY occurred_at, id")
        .fetch_all(&test.pool)
        .await
        .unwrap()
}

async fn session_revoke_reason(test: &TestDb, account: Uuid) -> Vec<Option<String>> {
    sqlx::query_scalar(
        "SELECT revoke_reason FROM sessions WHERE account_id = $1 ORDER BY created_at, id",
    )
    .bind(account)
    .fetch_all(&test.pool)
    .await
    .unwrap()
}

// ---------------------------------------------------------------------------
// First setup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn first_setup_creates_admin_signs_in_and_closes() {
    let test = TestDb::new().await;
    let app = test.app();

    let status = send(&app, get("/api/v1/setup", None)).await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.body, json!({ "setupRequired": true }));

    let reply = send(
        &app,
        post("/api/v1/setup", Some(setup_body(" Ada.Admin ")), None),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.cache_control(), Some("no-store"));
    let account = &reply.body["account"];
    assert_eq!(account["username"], "Ada.Admin");
    assert_eq!(account["displayName"], "Ada Admin");
    assert_eq!(account["email"], "it@example.test");
    assert_eq!(account["role"], "admin");
    assert_eq!(account["language"], "de");
    assert_eq!(account["status"], "active");
    assert_eq!(account["team"], Value::Null);
    assert_eq!(account["specialization"], Value::Null);
    assert!(reply.body["expiresAt"].is_string());
    let token = reply.token().expect("setup signs the admin in");

    let me = session(&app, &token).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["account"]["id"], account["id"]);

    // Stored hash is argon2id; the normalized username is the login key.
    let (hash, normalized): (String, String) =
        sqlx::query_as("SELECT password_hash, username_normalized FROM accounts")
            .fetch_one(&test.pool)
            .await
            .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert_eq!(normalized, "ada.admin");
    assert_eq!(
        login(&app, "ADA.ADMIN", PASSWORD).await.status,
        StatusCode::OK
    );

    let status = send(&app, get("/api/v1/setup", None)).await;
    assert_eq!(status.body, json!({ "setupRequired": false }));
    let again = send(
        &app,
        post("/api/v1/setup", Some(setup_body("second.admin")), None),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "setup_completed");
    let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!(admins, 1);

    let audit = audit_actions(&test).await;
    assert_eq!(audit[0].0, "setup.completed");
    assert_eq!(audit[0].1, "account");
    assert!(
        !audit
            .iter()
            .any(|(_, _, m)| m.to_string().contains(PASSWORD))
    );
}

#[tokio::test]
async fn setup_validates_fields() {
    let test = TestDb::new().await;
    let app = test.app();
    let reply = send(
        &app,
        post(
            "/api/v1/setup",
            Some(json!({
                "username": "a b",
                "displayName": " ",
                "email": "not-an-email",
                "password": "short",
                "language": "en",
            })),
            None,
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        reply.body["error"]["fields"],
        json!([
            { "field": "username", "code": "invalid_format" },
            { "field": "displayName", "code": "required" },
            { "field": "email", "code": "invalid_format" },
            { "field": "password", "code": "too_short", "params": { "min": 12 } },
        ])
    );

    let mut body = setup_body("long.password.user");
    body["password"] = json!("x".repeat(257));
    let reply = send(&app, post("/api/v1/setup", Some(body), None)).await;
    assert_eq!(
        reply.body["error"]["fields"],
        json!([{ "field": "password", "code": "too_long", "params": { "max": 256 } }])
    );

    let mut body = setup_body("Same.As.Password");
    body["password"] = json!("same.as.password");
    let reply = send(&app, post("/api/v1/setup", Some(body), None)).await;
    assert_eq!(reply.body["error"]["fields"][0]["code"], "invalid_value");

    let mut body = setup_body("ok.user");
    body["role"] = json!("user");
    let reply = send(&app, post("/api/v1/setup", Some(body), None)).await;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "unknown fields are rejected"
    );

    // Nothing was created; setup is still open.
    let status = send(&app, get("/api/v1/setup", None)).await;
    assert_eq!(status.body, json!({ "setupRequired": true }));
}

#[tokio::test]
async fn setup_username_already_taken_is_a_field_error() {
    let test = TestDb::new().await;
    // An account created outside setup (e.g. by an operator script).
    create_account(&test.pool, Role::User, "Ada.Admin", "x@example.test").await;
    let reply = send(
        &test.app(),
        post("/api/v1/setup", Some(setup_body("ada.admin")), None),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        reply.body["error"]["fields"],
        json!([{ "field": "username", "code": "already_taken" }])
    );
    let status = send(&test.app(), get("/api/v1/setup", None)).await;
    assert_eq!(status.body, json!({ "setupRequired": true }), "rolled back");
}

#[tokio::test]
async fn concurrent_setup_attempts_have_exactly_one_winner() {
    let test = TestDb::new().await;
    let app = test.app();
    let attempts = (0..8).map(|i| {
        let app = app.clone();
        tokio::spawn(async move {
            send(
                &app,
                post(
                    "/api/v1/setup",
                    Some(setup_body(&format!("admin{i}"))),
                    None,
                ),
            )
            .await
        })
    });
    let mut created = 0;
    let mut conflicts = 0;
    for attempt in attempts {
        let reply = attempt.await.unwrap();
        match reply.status {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => {
                assert_eq!(reply.code(), "setup_completed");
                conflicts += 1;
            }
            other => panic!("unexpected {other}: {}", reply.body),
        }
    }
    assert_eq!((created, conflicts), (1, 7));
    let (accounts, admins): (i64, i64) =
        sqlx::query_as("SELECT count(*), count(*) FILTER (WHERE role = 'admin') FROM accounts")
            .fetch_one(&test.pool)
            .await
            .unwrap();
    assert_eq!((accounts, admins), (1, 1));
    let bootstrap_admin: Uuid =
        sqlx::query_scalar("SELECT bootstrap_admin_account_id FROM organization_settings")
            .fetch_one(&test.pool)
            .await
            .unwrap();
    let only: Uuid = sqlx::query_scalar("SELECT id FROM accounts")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!(bootstrap_admin, only);
}

#[tokio::test]
async fn setup_stays_closed_after_restart() {
    let test = TestDb::new().await;
    let reply = send(
        &test.app(),
        post("/api/v1/setup", Some(setup_body("ada.admin")), None),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let token = reply.token().unwrap();

    // "Restart": new pool, new state, new in-memory limiters.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with(test.connect_options())
        .await
        .unwrap();
    let restarted = tikkit_api::app(
        tikkit_api::AppState::new(pool, test.config())
            .with_auth_settings(AuthSettings::insecure_fast_for_tests()),
    );
    let status = send(&restarted, get("/api/v1/setup", None)).await;
    assert_eq!(status.body, json!({ "setupRequired": false }));
    let again = send(
        &restarted,
        post("/api/v1/setup", Some(setup_body("other.admin")), None),
    )
    .await;
    assert_eq!(again.code(), "setup_completed");
    // Sessions survive the restart (they are server-side).
    assert_eq!(session(&restarted, &token).await.status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_succeeds_and_failures_are_indistinguishable() {
    let test = TestDb::new().await;
    let app = test.app();
    let ada = create_account_with_password(
        &test.pool,
        Role::Operator,
        "Ada",
        "ada@example.test",
        PASSWORD,
    )
    .await;
    let gone = create_account_with_password(
        &test.pool,
        Role::User,
        "gone",
        "gone@example.test",
        PASSWORD,
    )
    .await;
    sqlx::query("UPDATE accounts SET status = 'deactivated', deactivated_at = now() WHERE id = $1")
        .bind(gone.id)
        .execute(&test.pool)
        .await
        .unwrap();

    let ok = login(&app, "  ADA ", PASSWORD).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.cache_control(), Some("no-store"));
    assert_eq!(ok.body["account"]["id"], ada.id.to_string());
    assert_eq!(ok.body["account"]["role"], "operator");
    assert!(ok.token().is_some());

    let wrong_password = login(&app, "ada", "not the right password").await;
    let unknown = login(&app, "nobody", PASSWORD).await;
    let deactivated = login(&app, "gone", PASSWORD).await;
    let malformed = login(&app, "x", "").await;
    for reply in [&wrong_password, &unknown, &deactivated, &malformed] {
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(reply.code(), "invalid_credentials");
        assert_eq!(reply.set_cookie(), None);
        assert_eq!(
            reply.error_without_request_id(),
            wrong_password.error_without_request_id()
        );
    }

    let failures: Vec<(String, Value)> = sqlx::query_as(
        "SELECT actor_kind, metadata FROM audit_events WHERE action = 'auth.login_failed' \
         ORDER BY occurred_at, id",
    )
    .fetch_all(&test.pool)
    .await
    .unwrap();
    assert_eq!(failures.len(), 4);
    assert!(failures.iter().all(|(kind, _)| kind == "anonymous"));
    assert_eq!(failures[0].1["username"], "ada");
    assert_eq!(failures[0].1["reason"], "wrong_password");
    assert_eq!(failures[1].1["reason"], "unknown_username");
    assert_eq!(failures[2].1["reason"], "account_inactive");
    let all = audit_actions(&test).await;
    assert!(
        all.iter()
            .any(|(a, k, _)| a == "auth.login_succeeded" && k == "account")
    );
    for (_, _, metadata) in &all {
        let text = metadata.to_string();
        assert!(!text.contains(PASSWORD) && !text.contains("not the right password"));
    }
}

#[tokio::test]
async fn login_issues_new_session_and_revokes_presented_cookie() {
    let test = TestDb::new().await;
    let app = test.app();
    let ada =
        create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD)
            .await;
    let first = login_token(&app, "ada").await;

    // A second device keeps its own session.
    let other_device = login_token(&app, "ada").await;

    let reply = send(
        &app,
        post(
            "/api/v1/auth/login",
            Some(json!({ "username": "ada", "password": PASSWORD })),
            Some(&first),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let second = reply.token().unwrap();
    assert_ne!(first, second);

    let old = session(&app, &first).await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    assert!(old.clears_cookie());
    assert_eq!(session(&app, &second).await.status, StatusCode::OK);
    assert_eq!(session(&app, &other_device).await.status, StatusCode::OK);
    assert_eq!(
        session_revoke_reason(&test, ada.id).await,
        vec![Some("login_replaced".to_owned()), None, None]
    );

    // A planted, never-issued token value is not adopted either.
    let planted = sessions::generate_token();
    let reply = send(
        &app,
        post(
            "/api/v1/auth/login",
            Some(json!({ "username": "ada", "password": PASSWORD })),
            Some(&planted),
        ),
    )
    .await;
    assert_ne!(reply.token().unwrap(), planted);
    assert_eq!(
        session(&app, &planted).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn session_cookie_attributes_in_development_and_production() {
    let test = TestDb::new().await;
    create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD).await;

    let dev = login(&test.app(), "ada", PASSWORD).await;
    let cookie = dev.set_cookie().unwrap();
    let token = dev.token().unwrap();
    assert_eq!(token.len(), 43);
    assert_eq!(
        cookie,
        format!("tikkit_session={token}; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax")
    );

    let prod = tikkit_api::app(test.state_with(
        test.production_config(),
        AuthSettings::insecure_fast_for_tests(),
    ));
    let request = Request::post("/api/v1/auth/login")
        .header(header::ORIGIN, PRODUCTION_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "username": "ada", "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let reply = send(&prod, request).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let cookie = reply.set_cookie().unwrap();
    let token = cookie
        .strip_prefix("__Host-tikkit_session=")
        .and_then(|rest| rest.split(';').next())
        .unwrap()
        .to_owned();
    assert_eq!(
        cookie,
        format!(
            "__Host-tikkit_session={token}; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax; Secure"
        )
    );
    // Production only accepts the __Host- cookie.
    let with_host = Request::get("/api/v1/auth/session")
        .header(header::COOKIE, format!("__Host-tikkit_session={token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&prod, with_host).await.status, StatusCode::OK);
    let plain = Request::get("/api/v1/auth/session")
        .header(header::COOKIE, format!("tikkit_session={token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&prod, plain).await.status, StatusCode::UNAUTHORIZED);

    // Tokens are stored only as SHA-256 digests.
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_all(&test.pool)
        .await
        .unwrap();
    assert!(
        stored
            .iter()
            .all(|h| h.len() == 32 && *h != token.as_bytes())
    );
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn logout_revokes_the_session_and_clears_the_cookie() {
    let test = TestDb::new().await;
    let app = test.app();
    let ada =
        create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD)
            .await;
    let token = login_token(&app, "ada").await;

    let me = session(&app, &token).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.cache_control(), Some("no-store"));

    let out = send(&app, post("/api/v1/auth/logout", None, Some(&token))).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(out.clears_cookie());
    let after = session(&app, &token).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert_eq!(after.code(), "unauthenticated");
    assert_eq!(
        session_revoke_reason(&test, ada.id).await,
        vec![Some("logout".to_owned())]
    );

    // Idempotent: again, and without any cookie.
    assert_eq!(
        send(&app, post("/api/v1/auth/logout", None, Some(&token)))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let anonymous = send(&app, post("/api/v1/auth/logout", None, None)).await;
    assert_eq!(anonymous.status, StatusCode::NO_CONTENT);
    let logouts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'auth.logout'")
            .fetch_one(&test.pool)
            .await
            .unwrap();
    assert_eq!(logouts, 1);
}

#[tokio::test]
async fn missing_or_garbage_session_is_unauthenticated() {
    let test = TestDb::new().await;
    let app = test.app();
    let none = send(&app, get("/api/v1/auth/session", None)).await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    assert_eq!(none.code(), "unauthenticated");
    assert_eq!(none.set_cookie(), None, "nothing to clear");
    let garbage = session(&app, "garbage").await;
    assert_eq!(garbage.status, StatusCode::UNAUTHORIZED);
    assert!(garbage.clears_cookie());
}

#[tokio::test]
async fn idle_and_absolute_expiry() {
    let test = TestDb::new().await;
    let app = test.app();
    create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD).await;

    // Idle: 12 hours without activity.
    let idle = login_token(&app, "ada").await;
    sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '11 hours 59 minutes', rotated_at = now()")
        .execute(&test.pool)
        .await
        .unwrap();
    assert_eq!(
        session(&app, &idle).await.status,
        StatusCode::OK,
        "still within idle timeout"
    );
    sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '12 hours 1 minute'")
        .execute(&test.pool)
        .await
        .unwrap();
    let reply = session(&app, &idle).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.clears_cookie());

    // Absolute: 7 days after creation, however active.
    let absolute = login_token(&app, "ada").await;
    sqlx::query(
        "UPDATE sessions SET created_at = now() - interval '7 days 1 minute', \
                expires_at = now() - interval '1 minute', last_seen_at = now(), rotated_at = now() \
         WHERE revoked_at IS NULL AND expires_at > now()",
    )
    .execute(&test.pool)
    .await
    .unwrap();
    let reply = session(&app, &absolute).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.clears_cookie());
}

#[tokio::test]
async fn last_seen_updates_are_throttled() {
    let test = TestDb::new().await;
    let app = test.app();
    create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD).await;
    let token = login_token(&app, "ada").await;
    let last_seen = || async {
        sqlx::query_scalar::<_, time::OffsetDateTime>("SELECT last_seen_at FROM sessions")
            .fetch_one(&test.pool)
            .await
            .unwrap()
    };
    let before = last_seen().await;
    session(&app, &token).await;
    assert_eq!(last_seen().await, before, "no write within a minute");
    sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '2 minutes'")
        .execute(&test.pool)
        .await
        .unwrap();
    let stale = last_seen().await;
    session(&app, &token).await;
    assert!(last_seen().await > stale);
}

#[tokio::test]
async fn tokens_rotate_with_a_grace_period_for_the_previous_one() {
    let test = TestDb::new().await;
    let app = test.app();
    create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD).await;
    let first = login_token(&app, "ada").await;

    // Not due yet: no new cookie.
    assert_eq!(session(&app, &first).await.set_cookie(), None);

    sqlx::query("UPDATE sessions SET rotated_at = now() - interval '16 minutes'")
        .execute(&test.pool)
        .await
        .unwrap();
    let rotated = session(&app, &first).await;
    assert_eq!(rotated.status, StatusCode::OK);
    let second = rotated.token().expect("rotation sets a new cookie");
    assert_ne!(first, second);
    let max_age: i64 = rotated
        .set_cookie()
        .unwrap()
        .split("Max-Age=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (604_000..=604_800).contains(&max_age),
        "remaining absolute lifetime: {max_age}"
    );

    // In-flight requests with the old token still work during the grace
    // period, without rotating again.
    let grace = session(&app, &first).await;
    assert_eq!(grace.status, StatusCode::OK);
    assert_eq!(grace.set_cookie(), None);
    assert_eq!(session(&app, &second).await.status, StatusCode::OK);

    sqlx::query("UPDATE sessions SET previous_token_valid_until = now() - interval '1 second'")
        .execute(&test.pool)
        .await
        .unwrap();
    let expired = session(&app, &first).await;
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    assert!(expired.clears_cookie());
    assert_eq!(
        session(&app, &second).await.status,
        StatusCode::OK,
        "session itself lives on"
    );
    // A superseded token cannot log the session out either.
    send(&app, post("/api/v1/auth/logout", None, Some(&first))).await;
    assert_eq!(session(&app, &second).await.status, StatusCode::OK);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 1, "rotation keeps the same session");
}

#[tokio::test]
async fn concurrent_requests_rotate_only_once() {
    let test = TestDb::new().await;
    let app = test.app();
    create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD).await;
    let token = login_token(&app, "ada").await;
    sqlx::query("UPDATE sessions SET rotated_at = now() - interval '16 minutes'")
        .execute(&test.pool)
        .await
        .unwrap();
    let requests = (0..8).map(|_| {
        let app = app.clone();
        let token = token.clone();
        tokio::spawn(async move { session(&app, &token).await })
    });
    let mut new_tokens = Vec::new();
    for request in requests {
        let reply = request.await.unwrap();
        assert_eq!(reply.status, StatusCode::OK);
        assert!(!reply.clears_cookie());
        new_tokens.extend(reply.token());
    }
    assert_eq!(
        new_tokens.len(),
        1,
        "exactly one response carries the rotated token"
    );
    assert_eq!(session(&app, &new_tokens[0]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn deactivated_accounts_lose_existing_sessions() {
    let test = TestDb::new().await;
    let app = test.app();
    let ada = create_account_with_password(
        &test.pool,
        Role::Operator,
        "ada",
        "ada@example.test",
        PASSWORD,
    )
    .await;
    let token = login_token(&app, "ada").await;
    sqlx::query("UPDATE accounts SET status = 'deactivated', deactivated_at = now() WHERE id = $1")
        .bind(ada.id)
        .execute(&test.pool)
        .await
        .unwrap();
    let reply = session(&app, &token).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.clears_cookie());
    assert_eq!(
        session_revoke_reason(&test, ada.id).await,
        vec![Some("account_inactive".to_owned())]
    );
    // Reactivation does not revive the revoked session.
    sqlx::query("UPDATE accounts SET status = 'active', deactivated_at = NULL WHERE id = $1")
        .bind(ada.id)
        .execute(&test.pool)
        .await
        .unwrap();
    assert_eq!(session(&app, &token).await.status, StatusCode::UNAUTHORIZED);
    let revoked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'auth.session_revoked' AND actor_kind = 'system'",
    )
    .fetch_one(&test.pool)
    .await
    .unwrap();
    assert_eq!(revoked, 1);
}

#[tokio::test]
async fn role_and_team_changes_apply_on_the_next_request() {
    let test = TestDb::new().await;
    let app = test.app();
    let lead = create_account(&test.pool, Role::TeamLead, "lead", "lead@example.test").await;
    let team = create_team_with_lead(&test.pool, "Service Desk", &lead).await;
    let ada = create_account_with_password(
        &test.pool,
        Role::Operator,
        "ada",
        "ada@example.test",
        PASSWORD,
    )
    .await;
    let token = login_token(&app, "ada").await;
    let before = session(&app, &token).await;
    assert_eq!(before.body["account"]["role"], "operator");
    assert_eq!(before.body["account"]["team"], Value::Null);

    add_membership(&test.pool, team.id, ada.id, false)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE accounts SET role = 'specialist', specialization = 'Networking' WHERE id = $1",
    )
    .bind(ada.id)
    .execute(&test.pool)
    .await
    .unwrap();
    let after = session(&app, &token).await;
    assert_eq!(after.body["account"]["role"], "specialist");
    assert_eq!(after.body["account"]["specialization"], "Networking");
    assert_eq!(
        after.body["account"]["team"],
        json!({ "id": team.id, "name": "Service Desk", "isLead": false })
    );
}

#[tokio::test]
async fn revoke_all_for_account_keeps_the_excepted_session() {
    let test = TestDb::new().await;
    let app = test.app();
    let ada =
        create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD)
            .await;
    let other =
        create_account_with_password(&test.pool, Role::User, "bob", "bob@example.test", PASSWORD)
            .await;
    let keep = login_token(&app, "ada").await;
    let drop1 = login_token(&app, "ada").await;
    let drop2 = login_token(&app, "ada").await;
    let bob = login_token(&app, "bob").await;
    let keep_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM sessions WHERE account_id = $1 ORDER BY created_at, id LIMIT 1",
    )
    .bind(ada.id)
    .fetch_one(&test.pool)
    .await
    .unwrap();

    let revoked = sessions::revoke_all_for_account(
        &test.pool,
        ada.id,
        Some(keep_id),
        RevokeReason::PasswordChanged,
    )
    .await
    .unwrap();
    assert_eq!(revoked, 2);
    assert_eq!(session(&app, &keep).await.status, StatusCode::OK);
    assert_eq!(session(&app, &drop1).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(session(&app, &drop2).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        session(&app, &bob).await.status,
        StatusCode::OK,
        "other accounts unaffected"
    );

    // rotate_now: new token immediately, no grace for the old one.
    let (rotated, max_age) = sessions::rotate_now(&test.pool, keep_id)
        .await
        .unwrap()
        .expect("unrevoked session");
    assert!((604_000..=604_800).contains(&max_age), "{max_age}");
    assert_eq!(session(&app, &keep).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(session(&app, &rotated).await.status, StatusCode::OK);
    let keep = rotated;

    let all = sessions::revoke_all_for_account(
        &test.pool,
        ada.id,
        None,
        RevokeReason::AccountDeactivated,
    )
    .await
    .unwrap();
    assert_eq!(all, 1);
    assert_eq!(session(&app, &keep).await.status, StatusCode::UNAUTHORIZED);
    let _ = other;
}

// ---------------------------------------------------------------------------
// CSRF and abuse protection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unsafe_requests_require_the_public_origin() {
    let test = TestDb::new().await;
    let app = test.app();
    create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD).await;
    let body = || Some(json!({ "username": "ada", "password": PASSWORD }));

    for origin in [
        None,
        Some("https://evil.test"),
        Some("null"),
        Some("http://localhost:3001"),
    ] {
        let reply = send(&app, post_from("/api/v1/auth/login", body(), None, origin)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin:?}");
        assert_eq!(reply.code(), "csrf_rejected");
        assert!(reply.body["error"]["requestId"].is_string());
    }
    let setup = send(
        &app,
        post_from("/api/v1/setup", Some(setup_body("x.admin")), None, None),
    )
    .await;
    assert_eq!(setup.code(), "csrf_rejected");
    let token = login_token(&app, "ada").await;
    let logout = send(
        &app,
        post_from(
            "/api/v1/auth/logout",
            None,
            Some(&token),
            Some("https://evil.test"),
        ),
    )
    .await;
    assert_eq!(logout.code(), "csrf_rejected");
    assert_eq!(
        session(&app, &token).await.status,
        StatusCode::OK,
        "cross-site logout had no effect"
    );

    // Referer is the fallback when Origin is absent.
    let request = Request::post("/api/v1/auth/login")
        .header(header::REFERER, format!("{DEV_ORIGIN}/login?next=%2F"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body().unwrap().to_string()))
        .unwrap();
    assert_eq!(send(&app, request).await.status, StatusCode::OK);

    // Safe methods and probes are unaffected.
    assert_eq!(
        send(&app, get("/api/v1/setup", None)).await.status,
        StatusCode::OK
    );
    let health = Request::get("/api/health").body(Body::empty()).unwrap();
    assert_eq!(send(&app, health).await.status, StatusCode::OK);
}

#[tokio::test]
async fn login_and_setup_are_rate_limited() {
    let test = TestDb::new().await;
    let settings = AuthSettings {
        rate_limits: RateLimits {
            period: Duration::from_secs(60),
            login_per_ip: 6,
            login_per_ip_and_username: 3,
            setup_per_ip: 2,
        },
        ..AuthSettings::insecure_fast_for_tests()
    };
    let app = tikkit_api::app(test.state_with(test.config(), settings));
    create_account_with_password(&test.pool, Role::User, "ada", "ada@example.test", PASSWORD).await;

    for _ in 0..3 {
        assert_eq!(
            login(&app, "ada", "wrong password!!").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Per username (normalized): even the right password is refused now.
    let limited = login(&app, " ADA ", PASSWORD).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.code(), "rate_limited");
    let retry: u64 = limited.headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry));

    // Per address: other usernames have their own budget until the IP cap.
    assert_eq!(
        login(&app, "bob", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "carol", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "dave", PASSWORD).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    for _ in 0..2 {
        let reply = send(&app, post("/api/v1/setup", Some(setup_body("a")), None)).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let reply = send(
        &app,
        post("/api/v1/setup", Some(setup_body("ada.admin")), None),
    )
    .await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(reply.headers.contains_key(header::RETRY_AFTER));
}

// ---------------------------------------------------------------------------
// Shared email
// ---------------------------------------------------------------------------

#[tokio::test]
async fn accounts_sharing_an_email_stay_independent() {
    let test = TestDb::new().await;
    let app = test.app();
    let setup = send(
        &app,
        post("/api/v1/setup", Some(setup_body("ada.admin")), None),
    )
    .await;
    assert_eq!(setup.status, StatusCode::CREATED);
    let admin_id = setup.body["account"]["id"].as_str().unwrap().to_owned();
    // The same person's operational account, same contact email.
    let operator = create_account_with_password(
        &test.pool,
        Role::Operator,
        "ada.ops",
        "it@example.test",
        "a different operator passphrase",
    )
    .await;

    let admin_token = login_token(&app, "ada.admin").await;
    let ops = login(&app, "ada.ops", "a different operator passphrase").await;
    assert_eq!(ops.status, StatusCode::OK);
    let ops_token = ops.token().unwrap();
    assert_ne!(admin_token, ops_token);

    let admin_session = session(&app, &admin_token).await;
    let ops_session = session(&app, &ops_token).await;
    assert_eq!(admin_session.body["account"]["id"], admin_id);
    assert_eq!(admin_session.body["account"]["role"], "admin");
    assert_eq!(ops_session.body["account"]["id"], operator.id.to_string());
    assert_eq!(ops_session.body["account"]["role"], "operator");
    assert_eq!(
        admin_session.body["account"]["email"],
        ops_session.body["account"]["email"]
    );

    // Credentials are not interchangeable.
    assert_eq!(
        login(&app, "ada.ops", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "ada.admin", "a different operator passphrase")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Logging out of one leaves the other signed in.
    send(&app, post("/api/v1/auth/logout", None, Some(&ops_token))).await;
    assert_eq!(
        session(&app, &ops_token).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(session(&app, &admin_token).await.status, StatusCode::OK);
}
