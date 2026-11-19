//! Restore mode (docs/backup-restore.md): a restored copy refuses the requests
//! that would reach a mail relay or an AI provider. `main` also does not start
//! the background workers in this mode; this covers the request paths.

mod support;

use std::time::Duration;

use serde_json::json;
use support::{
    TestDb, call, fixtures, login,
    smtp::{Fault, Listen, SECRET, USER, listen, wait_messages},
    test_app_with,
};
use tikkit_api::auth::Settings;

fn settings(ca_pem: &str, outbound_enabled: bool) -> Settings {
    Settings {
        smtp_trust_root_pem: Some(ca_pem.as_bytes().to_vec()),
        restore_mode: !outbound_enabled,
        ..Settings::default()
    }
}

#[tokio::test]
async fn restore_mode_sends_no_mail_and_refuses_connection_tests() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::None).await;
    let normal = test_app_with(&db.pool, settings(&server.ca_pem, true));
    let restored = test_app_with(&db.pool, settings(&server.ca_pem, false));
    let cookie = login(&normal, "admin", fixtures::PASSWORD).await.session();
    let saved = call(
        &normal,
        "PUT",
        "/api/settings/smtp",
        Some(json!({
            "host": "127.0.0.1", "port": server.port, "security": "starttls",
            "sender_name": "Tikkit Help", "sender_address": "help@example.com",
            "username": USER, "password": SECRET, "enabled": true,
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);

    let invite = json!({ "role": "user", "email": "new@example.com", "language": "en" });
    let sent = call(
        &normal,
        "POST",
        "/api/invitations",
        Some(invite.clone()),
        Some(&cookie),
    )
    .await;
    assert_eq!(sent.body["emailed"], true, "{}", sent.body);
    wait_messages(&server.messages, 1).await;

    // The same request on the restored copy still creates the link, mails nothing.
    let cookie = login(&restored, "admin", fixtures::PASSWORD)
        .await
        .session();
    let held = call(
        &restored,
        "POST",
        "/api/invitations",
        Some(invite),
        Some(&cookie),
    )
    .await;
    assert_eq!(held.status, 201, "{}", held.body);
    assert_eq!(held.body["emailed"], false, "{}", held.body);
    // Recovery must not mint a token or use the cooldown, and must not reveal
    // whether the username exists: the same refusal for both.
    for username in ["admin", "nobody-here"] {
        let recovery = call(
            &restored,
            "POST",
            "/api/auth/recovery",
            Some(json!({ "username": username })),
            None,
        )
        .await;
        assert_eq!(recovery.status, 409, "{}", recovery.body);
        assert_eq!(recovery.body["error"]["code"], "outbound_disabled");
    }
    let resets: i64 = sqlx::query_scalar("SELECT count(*) FROM password_resets")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(resets, 0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(server.messages.lock().unwrap().len(), 1);

    for (path, body) in [
        (
            "/api/settings/smtp/test",
            Some(json!({ "to": "admin@example.com" })),
        ),
        ("/api/settings/ai/test", None),
    ] {
        let r = call(&restored, "POST", path, body, Some(&cookie)).await;
        assert_eq!(r.status, 409, "{path}: {}", r.body);
        assert_eq!(r.body["error"]["code"], "outbound_disabled", "{path}");
    }
    assert_eq!(server.messages.lock().unwrap().len(), 1);

    // Reading the portal is unaffected.
    let me = call(&restored, "GET", "/api/auth/me", None, Some(&cookie)).await;
    assert_eq!(me.status, 200, "{}", me.body);
}

#[tokio::test]
async fn restore_mode_is_visible_in_readiness_and_metrics() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let normal = test_app_with(&db.pool, Settings::default());
    let restored = test_app_with(
        &db.pool,
        Settings {
            restore_mode: true,
            ..Settings::default()
        },
    );
    for (app, expected) in [(&normal, false), (&restored, true)] {
        let r = call(app, "GET", "/api/ready", None, None).await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["restore_mode"], expected);
    }
    let state = support::test_state(&db.pool);
    assert!(
        support::metrics_text(&state)
            .await
            .contains("tikkit_restore_mode 0\n")
    );
    state.metrics.set_restore_mode(true);
    assert!(
        support::metrics_text(&state)
            .await
            .contains("tikkit_restore_mode 1\n")
    );
}

#[tokio::test]
async fn cutover_suppresses_only_old_pending_email() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = fixtures::bootstrap_admin(&db.pool).await;
    let cutoff = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let row = |key: &str, channel: &str, status: &str, age_minutes: i64| {
        let (pool, key, channel, status) = (
            db.pool.clone(),
            key.to_owned(),
            channel.to_owned(),
            status.to_owned(),
        );
        async move {
            sqlx::query(
                "INSERT INTO notification_outbox (source_key, event_id, account_id, audience, channel, status, created_at)
                 VALUES ($1, 1, $2, 'staff', $3, $4, now() - make_interval(mins => $5::int))",
            )
            .bind(key).bind(account).bind(channel).bind(status).bind(age_minutes as i32)
            .execute(&pool).await.unwrap();
        }
    };
    row("old-email", "email", "pending", 120).await;
    row("old-email-sent", "email", "sent", 120).await;
    row("new-email", "email", "pending", 5).await;
    row("old-inbox", "in_app", "pending", 120).await;
    let done = tikkit_api::restore::suppress_pending_email(&db.pool, cutoff)
        .await
        .unwrap();
    assert_eq!(
        done,
        tikkit_api::restore::Suppressed {
            email_suppressed: 1,
            email_pending_after_cutoff: 1,
            in_app_pending: 1
        }
    );
    let state = |key: &str| {
        let (pool, key) = (db.pool.clone(), key.to_owned());
        async move {
            sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT status, last_error FROM notification_outbox WHERE source_key = $1",
            )
            .bind(key)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(
        state("old-email").await,
        ("suppressed".into(), Some("suppressed_on_restore".into()))
    );
    assert_eq!(state("old-email-sent").await.0, "sent");
    assert_eq!(state("new-email").await.0, "pending");
    assert_eq!(state("old-inbox").await.0, "pending");
    let audit: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'notifications_suppressed_on_restore'",
    )
    .fetch_all(&db.pool).await.unwrap();
    assert_eq!(audit.len(), 1);
    let actor: (Option<String>, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT actor_service, actor_account_id FROM audit_events WHERE event_type = 'notifications_suppressed_on_restore'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(actor, (Some("restore".into()), None));
    assert_eq!(audit[0]["email_suppressed"], 1);
    assert!(audit[0]["cutoff"].is_string());
    assert_eq!(
        audit[0].as_object().unwrap().len(),
        4,
        "counts and cutoff only: {}",
        audit[0]
    );
}

#[tokio::test]
async fn credentials_check_tells_a_wrong_key_from_the_right_one() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let app = test_app_with(&db.pool, Settings::default());
    let cookie = login(&app, "admin", fixtures::PASSWORD).await.session();
    let right = Settings::default().encryption_key;
    let wrong = [7u8; 32];
    let status = |key: [u8; 32]| {
        let pool = db.pool.clone();
        async move {
            tikkit_api::restore::credentials_status(&pool, &key)
                .await
                .unwrap()
        }
    };
    assert_eq!(status(right).await, ("none", "none"));

    let password = ["smtp", "pw"].join("-");
    let smtp = json!({
        "host": "smtp.invalid", "port": 587, "security": "starttls", "sender_name": "T",
        "sender_address": "help@example.invalid", "username": "mailer", "password": password,
        "enabled": false,
    });
    let r = call(&app, "PUT", "/api/settings/smtp", Some(smtp), Some(&cookie)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let key = ["sk", "test", "0123456789abcdefWXYZ"].join("-");
    let r = call(
        &app,
        "PUT",
        "/api/settings/ai/credentials/openai",
        Some(json!({ "api_key": key })),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(status(right).await, ("readable", "readable"));
    assert_eq!(status(wrong).await, ("unreadable", "unreadable"));
}

#[tokio::test]
async fn restore_mode_refuses_destructive_jobs_but_not_reads() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let admin = fixtures::bootstrap_admin(&db.pool).await;
    let normal = test_app_with(&db.pool, Settings::default());
    let restored = test_app_with(
        &db.pool,
        Settings {
            restore_mode: true,
            ..Settings::default()
        },
    );
    let normal_cookie = login(&normal, "admin", fixtures::PASSWORD).await.session();
    let cookie = login(&restored, "admin", fixtures::PASSWORD)
        .await
        .session();
    let run = json!({ "confirm": "DELETE", "preview_token": "x", "as_of": "2026-01-01T00:00:00Z" });
    let anonymize = json!({ "username": "admin" });
    let anonymize_path = format!("/api/admin/accounts/{admin}/anonymize");

    // Normal mode gets as far as its own validation.
    let r = call(
        &normal,
        "POST",
        "/api/admin/retention/runs",
        Some(run.clone()),
        Some(&normal_cookie),
    )
    .await;
    assert_ne!(
        r.body["error"]["code"], "restore_mode_blocked",
        "{}",
        r.body
    );
    let r = call(
        &normal,
        "POST",
        &anonymize_path,
        Some(anonymize.clone()),
        Some(&normal_cookie),
    )
    .await;
    assert_ne!(
        r.body["error"]["code"], "restore_mode_blocked",
        "{}",
        r.body
    );

    for (path, body) in [
        ("/api/admin/retention/runs".to_owned(), run),
        (anonymize_path, anonymize),
    ] {
        let r = call(&restored, "POST", &path, Some(body), Some(&cookie)).await;
        assert_eq!(r.status, 409, "{path}: {}", r.body);
        assert_eq!(r.body["error"]["code"], "restore_mode_blocked", "{path}");
    }
    // Looking is still allowed: the copy is there to be inspected.
    for path in [
        "/api/admin/retention",
        "/api/admin/reports?created_from=2026-01-01T00:00:00Z&created_before=2026-02-01T00:00:00Z",
    ] {
        let r = call(&restored, "GET", path, None, Some(&cookie)).await;
        assert_eq!(r.status, 200, "{path}: {}", r.body);
    }
}
