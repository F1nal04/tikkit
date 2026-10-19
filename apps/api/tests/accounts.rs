//! Invitations, recovery, profile, and account lifecycle against real PostgreSQL
//! (skipped without TEST_DATABASE_URL).

use axum::{Router, http::StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

mod support;
use support::{
    Reply, TestDb, call, call_from,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

const NEW_PASSWORD: &str = "a brand new secret";

async fn admin_session(t: &TestDb, app: &Router) -> String {
    fixtures::bootstrap_admin(&t.pool).await;
    login(app, "admin", PASSWORD).await.session()
}

async fn audit_types(t: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT event_type FROM audit_events ORDER BY id")
        .fetch_all(&t.pool)
        .await
        .unwrap()
}

/// Creates an invitation and returns its token (the last path segment of the link).
async fn invite(app: &Router, admin: &str, body: Value) -> (Reply, String) {
    let r = call(app, "POST", "/api/invitations", Some(body), Some(admin)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let url = r.body["url"].as_str().unwrap();
    let token = url.rsplit('/').next().unwrap().split('?').next().unwrap();
    let token = token.to_owned();
    (r, token)
}

fn accept_body(token: &str, username: &str) -> Value {
    json!({ "token": token, "username": username, "email": "shared@example.com",
            "password": PASSWORD, "language": "de" })
}

async fn accept(app: &Router, body: Value) -> Reply {
    call(app, "POST", "/api/invitations/accept", Some(body), None).await
}

#[tokio::test]
async fn invitation_creates_the_account_with_the_server_side_role() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let existing = fixtures::account(&t.pool, Role::User, "taken").await;

    // Only Admins invite; the default role is User.
    let user = login(&app, "taken", PASSWORD).await.session();
    let r = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({})),
        Some(&user),
    )
    .await;
    assert_eq!((r.status, r.code()), (StatusCode::FORBIDDEN, "forbidden"));
    let (r, _) = invite(&app, &admin, json!({})).await;
    assert_eq!(r.body["invitation"]["role"], "user");
    assert_eq!(r.body["emailed"], false, "no SMTP in tests");

    let (r, token) = invite(&app, &admin, json!({ "role": "operator" })).await;
    assert!(
        r.body["url"]
            .as_str()
            .unwrap()
            .starts_with("http://localhost:3000/invite/")
    );
    let preview = call(
        &app,
        "POST",
        "/api/invitations/lookup",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(preview.body["role"], "operator");

    // Nothing below consumes the invitation: a forged role, a taken username,
    // a weak password.
    let mut forged = accept_body(&token, "newbie");
    forged["role"] = json!("admin");
    let r = accept(&app, forged).await;
    assert_eq!(r.code(), "unknown_field");
    let r = accept(&app, accept_body(&token, "TAKEN")).await;
    assert_eq!(
        r.body["error"]["details"]["fields"],
        json!({ "username": ["taken"] })
    );
    let mut weak = accept_body(&token, "newbie");
    weak["password"] = json!("short");
    assert_eq!(accept(&app, weak).await.code(), "validation_failed");

    let r = accept(&app, accept_body(&token, "Newbie")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (&r.body["username"], &r.body["role"], &r.body["language"]),
        (&json!("Newbie"), &json!("operator"), &json!("de"))
    );
    // Same email as an existing account, still a separate account.
    assert_ne!(r.body["id"], json!(existing.to_string()));
    let me = call(&app, "GET", "/api/auth/me", None, Some(&r.session())).await;
    assert_eq!(me.body["username"], "Newbie");

    // Used: replay and lookup fail the same way.
    let r = accept(&app, accept_body(&token, "another")).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::GONE, "invitation_invalid")
    );
    let r = call(
        &app,
        "POST",
        "/api/invitations/lookup",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(r.code(), "invitation_invalid");
    let types = audit_types(&t).await;
    assert!(types.contains(&"invitation_created".into()));
    assert!(types.contains(&"invitation_accepted".into()));
}

#[tokio::test]
async fn expired_and_revoked_invitations_are_rejected() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let (_, expired) = invite(&app, &admin, json!({ "expires_in_hours": 1 })).await;
    sqlx::query("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .execute(&t.pool)
        .await
        .unwrap();
    assert_eq!(
        accept(&app, accept_body(&expired, "late")).await.code(),
        "invitation_invalid"
    );
    let r = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "expires_in_hours": 0, "email": "nope" })),
        Some(&admin),
    )
    .await;
    assert_eq!(
        r.body["error"]["details"]["fields"],
        json!({ "email": ["invalid"], "expires_in_hours": ["invalid"] })
    );

    let (r, token) = invite(&app, &admin, json!({ "email": "x@example.com" })).await;
    let id = r.body["invitation"]["id"].as_str().unwrap().to_owned();
    let list = call(&app, "GET", "/api/invitations", None, Some(&admin)).await;
    let items = list.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "expired invitations are not pending");
    assert_eq!(items[0]["email"], "x@example.com");

    let revoke = format!("/api/invitations/{id}/revoke");
    let r = call(&app, "POST", &revoke, None, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        accept(&app, accept_body(&token, "revoked")).await.code(),
        "invitation_invalid"
    );
    let r = call(&app, "POST", &revoke, None, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let list = call(&app, "GET", "/api/invitations", None, Some(&admin)).await;
    assert_eq!(list.body["items"], json!([]));
    assert!(audit_types(&t).await.contains(&"invitation_revoked".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_acceptance_has_one_winner() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let (_, token) = invite(&app, &admin, json!({})).await;
    let attempts: Vec<_> = (0..8u8)
        .map(|i| {
            let (app, body) = (app.clone(), accept_body(&token, &format!("racer{i}")));
            tokio::spawn(async move {
                let path = "/api/invitations/accept";
                call_from(&app, "POST", path, Some(body), None, [10, 0, 0, i]).await
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for attempt in attempts {
        statuses.push(attempt.await.unwrap().status);
    }
    assert_eq!(statuses.iter().filter(|s| **s == StatusCode::OK).count(), 1);
    assert!(statuses.iter().all(|s| matches!(s.as_u16(), 200 | 410)));
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'user'")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 1);
}

/// Inserts a recovery token for `account` directly; the API only ever emails it.
async fn reset_token(t: &TestDb, account: Uuid, expires: &str) -> String {
    let token = Uuid::new_v4().to_string();
    sqlx::query(&format!(
        "INSERT INTO password_resets (account_id, token_hash, expires_at)
         VALUES ($1, sha256(convert_to($2, 'UTF8')), now() + interval '{expires}')"
    ))
    .bind(account)
    .bind(&token)
    .execute(&t.pool)
    .await
    .unwrap();
    token
}

async fn reset(app: &Router, token: &str, password: &str) -> Reply {
    let body = json!({ "token": token, "new_password": password });
    call(app, "POST", "/api/auth/recovery/reset", Some(body), None).await
}

#[tokio::test]
async fn recovery_is_neutral_and_bound_to_one_account() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    // Both share an email; recovery for one must never touch the other.
    let alice = fixtures::account(&t.pool, Role::User, "alice").await;
    fixtures::account(&t.pool, Role::User, "bob").await;
    let alice_session = login(&app, "alice", PASSWORD).await.session();
    let bob_session = login(&app, "bob", PASSWORD).await.session();

    let unknown = call(
        &app,
        "POST",
        "/api/auth/recovery",
        Some(json!({ "username": "nobody" })),
        None,
    )
    .await;
    let known = call(
        &app,
        "POST",
        "/api/auth/recovery",
        Some(json!({ "username": " Alice " })),
        None,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::ACCEPTED);
    assert_eq!(
        (known.status, &known.body, &known.cookie),
        (unknown.status, &unknown.body, &unknown.cookie)
    );
    let owners: Vec<Uuid> = sqlx::query_scalar("SELECT account_id FROM password_resets")
        .fetch_all(&t.pool)
        .await
        .unwrap();
    assert_eq!(owners, [alice]);

    let token = reset_token(&t, alice, "1 hour").await;
    let r = reset(&app, &token, "password1234").await;
    assert_eq!(
        r.body["error"]["details"]["fields"],
        json!({ "new_password": ["too_common"] })
    );
    let r = reset(&app, &token, NEW_PASSWORD).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let me = |s: String| {
        let app = app.clone();
        async move {
            call(&app, "GET", "/api/auth/me", None, Some(&s))
                .await
                .status
        }
    };
    assert_eq!(me(alice_session).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me(bob_session).await, StatusCode::OK);
    assert_eq!(
        login(&app, "alice", NEW_PASSWORD).await.status,
        StatusCode::OK
    );
    assert_eq!(login(&app, "bob", PASSWORD).await.status, StatusCode::OK);

    // Single use; the token requested earlier died with this reset too.
    let r = reset(&app, &token, "yet another secret").await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::GONE, "reset_token_invalid")
    );
    let unused: i64 =
        sqlx::query_scalar("SELECT count(*) FROM password_resets WHERE used_at IS NULL")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(unused, 0);
    let expired = reset_token(&t, alice, "-1 second").await;
    assert_eq!(
        reset(&app, &expired, "yet another secret").await.code(),
        "reset_token_invalid"
    );
    let types = audit_types(&t).await;
    assert!(types.contains(&"recovery_requested".into()));
    assert!(types.contains(&"recovery_completed".into()));
}

#[tokio::test]
async fn profile_updates_email_and_language() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    fixtures::account(&t.pool, Role::Operator, "olga").await;
    let s = login(&app, "olga", PASSWORD).await.session();
    let body = json!({ "email": "nope" });
    let r = call(&app, "PATCH", "/api/auth/me", Some(body), Some(&s)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = json!({ "language": "de" });
    let r = call(&app, "PATCH", "/api/auth/me", Some(body), Some(&s)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (&r.body["language"], &r.body["email"]),
        (&json!("de"), &json!("shared@example.com"))
    );
    let body = json!({ "email": " olga@example.com " });
    call(&app, "PATCH", "/api/auth/me", Some(body), Some(&s)).await;
    let me = call(&app, "GET", "/api/auth/me", None, Some(&s)).await;
    assert_eq!(
        (&me.body["language"], &me.body["email"]),
        (&json!("de"), &json!("olga@example.com"))
    );
    let body = json!({ "role": "admin" });
    let r = call(&app, "PATCH", "/api/auth/me", Some(body), Some(&s)).await;
    assert_eq!(r.code(), "unknown_field");
    assert!(audit_types(&t).await.contains(&"profile_updated".into()));
}

#[tokio::test]
async fn deactivation_ends_sessions_and_keeps_history() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let dave = fixtures::account(&t.pool, Role::User, "dave").await;
    let ticket: Uuid = sqlx::query_scalar(
        "WITH c AS (INSERT INTO categories (name) VALUES ('Hardware') RETURNING id),
              p AS (INSERT INTO priorities (name) VALUES ('Normal') RETURNING id)
         INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description)
         SELECT $1, c.id, p.id, 'Printer', 'Jammed' FROM c, p RETURNING id",
    )
    .bind(dave)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let session = login(&app, "dave", PASSWORD).await.session();
    let reset_link = reset_token(&t, dave, "1 hour").await;
    // Invitations issued by an Admin who is later deactivated stop working.
    fixtures::account(&t.pool, Role::Admin, "issuer").await;
    let issuer = login(&app, "issuer", PASSWORD).await;
    let issuer_id = issuer.body["id"].as_str().unwrap().to_owned();
    let (_, orphan) = invite(&app, &issuer.session(), json!({})).await;
    let path = format!("/api/accounts/{issuer_id}/deactivate");
    let r = call(&app, "POST", &path, None, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        accept(&app, accept_body(&orphan, "orphan")).await.code(),
        "invitation_invalid"
    );

    let deactivate = format!("/api/accounts/{dave}/deactivate");
    let r = call(&app, "POST", &deactivate, None, Some(&session)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(&app, "GET", "/api/accounts", None, Some(&session)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let missing = format!("/api/accounts/{}/deactivate", Uuid::new_v4());
    let r = call(&app, "POST", &missing, None, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = call(&app, "POST", &deactivate, None, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let me = call(&app, "GET", "/api/auth/me", None, Some(&session)).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&app, "dave", PASSWORD).await.code(),
        "invalid_credentials"
    );
    let r = reset(&app, &reset_link, NEW_PASSWORD).await;
    assert_eq!(r.code(), "reset_token_invalid");
    let list = call(&app, "GET", "/api/accounts", None, Some(&admin)).await;
    let row = list.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["username"] == "dave")
        .unwrap()
        .clone();
    assert_eq!(row["is_active"], false);
    let requester: Uuid =
        sqlx::query_scalar("SELECT requester_account_id FROM tickets WHERE id = $1")
            .bind(ticket)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(requester, dave);

    let reactivate = format!("/api/accounts/{dave}/reactivate");
    let r = call(&app, "POST", &reactivate, None, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        reset(&app, &reset_link, NEW_PASSWORD).await.code(),
        "reset_token_invalid",
        "reactivation does not revive recovery links"
    );
    assert_eq!(login(&app, "dave", PASSWORD).await.status, StatusCode::OK);
    let types = audit_types(&t).await;
    for kind in [
        "account_deactivated",
        "account_reactivated",
        "session_revoked",
        "invitation_revoked",
    ] {
        assert!(types.contains(&kind.into()), "{kind}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_active_admin_cannot_be_deactivated_even_concurrently() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let me: Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE username = 'admin'")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let r = call(
        &app,
        "POST",
        &format!("/api/accounts/{me}/deactivate"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "last_active_admin")
    );

    // Two Admins deactivate themselves at the same time: one must survive.
    let other = fixtures::account(&t.pool, Role::Admin, "admin2").await;
    for round in 0..10 {
        let app = test_app(&t.pool); // fresh login rate limiter per round
        sqlx::query("UPDATE accounts SET is_active = true")
            .execute(&t.pool)
            .await
            .unwrap();
        let a = login(&app, "admin", PASSWORD).await.session();
        let b = login(&app, "admin2", PASSWORD).await.session();
        let race = [(a, me), (b, other)].map(|(session, target)| {
            let app = app.clone();
            tokio::spawn(async move {
                let path = format!("/api/accounts/{target}/deactivate");
                call(&app, "POST", &path, None, Some(&session)).await.status
            })
        });
        let mut statuses = Vec::new();
        for task in race {
            statuses.push(task.await.unwrap());
        }
        let active: i64 =
            sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin' AND is_active")
                .fetch_one(&t.pool)
                .await
                .unwrap();
        assert_eq!(active, 1, "round {round}: {statuses:?}");
        statuses.sort();
        assert_eq!(
            statuses,
            [StatusCode::NO_CONTENT, StatusCode::CONFLICT],
            "round {round}"
        );
    }
}

#[tokio::test]
async fn recovery_sends_at_most_one_link_per_cooldown() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    fixtures::account(&t.pool, Role::User, "victim").await;
    let request = || {
        let app = app.clone();
        async move {
            let body = json!({ "username": "victim" });
            call(&app, "POST", "/api/auth/recovery", Some(body), None)
                .await
                .status
        }
    };
    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM password_resets")
            .fetch_one(&t.pool)
            .await
            .unwrap()
    };
    assert_eq!(request().await, StatusCode::ACCEPTED);
    assert_eq!(
        request().await,
        StatusCode::ACCEPTED,
        "same answer while cooling down"
    );
    assert_eq!(count().await, 1);
    sqlx::query("UPDATE password_resets SET created_at = now() - interval '16 minutes'")
        .execute(&t.pool)
        .await
        .unwrap();
    request().await;
    assert_eq!(count().await, 2);
}

#[tokio::test]
async fn token_endpoints_are_rate_limited_per_ip() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    for (path, body) in [
        ("/api/invitations/lookup", json!({ "token": "guess" })),
        (
            "/api/auth/recovery/reset",
            json!({ "token": "guess", "new_password": NEW_PASSWORD }),
        ),
    ] {
        for _ in 0..10 {
            let r = call(&app, "POST", path, Some(body.clone()), None).await;
            assert_eq!(r.status, StatusCode::GONE, "{path}");
        }
        let r = call(&app, "POST", path, Some(body.clone()), None).await;
        assert_eq!(r.code(), "rate_limited", "{path}");
        let r = call_from(&app, "POST", path, Some(body), None, [10, 1, 1, 1]).await;
        assert_eq!(r.status, StatusCode::GONE, "other clients are unaffected");
    }
}
