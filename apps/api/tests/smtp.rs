//! Admin SMTP configuration, against an in-process TLS server. No external relay.

mod support;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::{
    TestDb, call, fixtures, login,
    smtp::{Fault, Listen, SECRET, USER, decode_mail, identity, listen, wait_messages},
    test_app_with,
};
use tikkit_api::{
    auth::Settings,
    mail::{self, DeliverOptions, Delivered, MailError, OutboundMessage},
};

fn router(pool: &sqlx::PgPool, trust: Option<&str>) -> axum::Router {
    let settings = Settings {
        smtp_trust_root_pem: trust.map(|pem| pem.as_bytes().to_vec()),
        ..Settings::default()
    };
    test_app_with(pool, settings)
}

fn write_body(port: u16, security: &str, password: Option<&str>, enabled: bool) -> Value {
    let mut body = json!({
        "host": "127.0.0.1",
        "port": port,
        "security": security,
        "sender_name": "Tikkit Help",
        "sender_address": "help@example.com",
        "username": USER,
        "enabled": enabled,
    });
    if let Some(password) = password {
        body["password"] = json!(password);
    }
    body
}

async fn save(app: &axum::Router, cookie: &str, body: Value) -> support::Reply {
    call(app, "PUT", "/api/settings/smtp", Some(body), Some(cookie)).await
}

fn assert_secret_absent(value: &impl std::fmt::Display) {
    let text = value.to_string();
    assert!(!text.contains(SECRET), "{text}");
}

async fn ciphertext(pool: &sqlx::PgPool) -> Vec<u8> {
    sqlx::query_scalar("SELECT password_ciphertext FROM smtp_connection")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn outsiders_cannot_touch_smtp_and_the_portal_works_without_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    for (role, name) in [
        (fixtures::Role::User, "ada"),
        (fixtures::Role::Operator, "op"),
        (fixtures::Role::Specialist, "spec"),
        (fixtures::Role::TeamLead, "lead"),
    ] {
        fixtures::account(&db.pool, role, name).await;
    }
    let app = router(&db.pool, None);
    let anon = call(&app, "GET", "/api/settings/smtp", None, None).await;
    assert_eq!(anon.code(), "unauthorized");

    let body = write_body(587, "starttls", Some(SECRET), true);
    for name in ["ada", "op", "spec", "lead"] {
        let cookie = login(&app, name, fixtures::PASSWORD).await.session();
        for (method, path) in [
            ("GET", "/api/settings/smtp"),
            ("PUT", "/api/settings/smtp"),
            ("DELETE", "/api/settings/smtp"),
            ("POST", "/api/settings/smtp/test"),
        ] {
            let payload = (method == "PUT" || method == "POST").then(|| {
                if method == "POST" {
                    json!({ "to": "shared@example.com" })
                } else {
                    body.clone()
                }
            });
            let reply = call(&app, method, path, payload, Some(&cookie)).await;
            assert_eq!(reply.status, 403, "{name} {method} {path} {}", reply.body);
            assert_eq!(reply.code(), "forbidden");
            assert_secret_absent(&reply.body);
        }
    }

    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    let empty = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(empty.status, 200, "{}", empty.body);
    assert!(empty.body["connection"].is_null());
    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com", "role": "user" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], false);
    assert!(
        invited.body["url"]
            .as_str()
            .unwrap()
            .contains("http://localhost:3000/invite/")
    );
}

#[tokio::test]
async fn admin_stores_the_password_encrypted_and_never_returns_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();

    let missing = save(
        &app,
        &admin,
        write_body(server.port, "starttls", None, true),
    )
    .await;
    assert_eq!(missing.status, 422, "{}", missing.body);
    assert_eq!(
        missing.body["error"]["details"]["fields"]["password"][0],
        "required"
    );

    let bad = save(
        &app,
        &admin,
        json!({
            "host": "not a host",
            "port": 0,
            "security": "none",
            "sender_name": "Bad\nName",
            "sender_address": "nope",
            "username": "mailer",
            "password": SECRET,
            "enabled": true,
        }),
    )
    .await;
    assert_eq!(bad.status, 422, "{}", bad.body);
    assert_secret_absent(&bad.body);

    let saved = save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), true),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.body["connection"]["password_set"], true);
    assert!(saved.body["connection"].get("password").is_none());
    assert_secret_absent(&saved.body);
    let stored = ciphertext(&db.pool).await;
    assert!(
        !stored.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
        "password stored in plaintext"
    );

    let got = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(got.body["connection"]["host"], "127.0.0.1");
    assert_eq!(got.body["connection"]["security"], "starttls");
    assert_secret_absent(&got.body);
    assert!(got.body["connection"].get("username").is_none());
    let stored_user: Vec<u8> =
        sqlx::query_scalar("SELECT username_ciphertext FROM smtp_connection")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        !stored_user
            .windows(USER.len())
            .any(|w| w == USER.as_bytes())
    );

    // Stored credentials never follow a new host without being entered again.
    let mut moved = write_body(server.port, "starttls", None, true);
    moved["host"] = json!("localhost");
    moved.as_object_mut().unwrap().remove("username");
    let moved = save(&app, &admin, moved).await;
    assert_eq!(moved.status, 422, "{}", moved.body);
    let fields = &moved.body["error"]["details"]["fields"];
    assert_eq!(fields["username"][0], "required");
    assert_eq!(fields["password"][0], "required");
    let mut kept = write_body(2525, "implicit_tls", None, false);
    kept.as_object_mut().unwrap().remove("username");
    assert_eq!(save(&app, &admin, kept).await.status, 200);
    assert_eq!(ciphertext(&db.pool).await, stored);

    let meta: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'smtp_configured'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0]["password_changed"], true);
    assert!(meta[0].get("password").is_none());
    assert!(meta[0].get("username").is_none());
    assert_secret_absent(&meta[0]);
}

#[tokio::test]
async fn starttls_delivers_test_invitation_and_recovery_mail() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    let saved = save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), true),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);

    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    assert_eq!(test.body["delivered"], true);
    assert_secret_absent(&test.body);
    wait_messages(&server.messages, 1).await;
    let body = server.messages.lock().unwrap()[0].clone();
    assert!(body.contains("http://localhost:3000"), "{body}");
    assert!(body.contains("Do not reply to this email"), "{body}");
    assert!(!body.contains(SECRET), "{body}");

    let sender = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "help@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(sender.status, 200, "{}", sender.body);
    wait_messages(&server.messages, 2).await;

    let elsewhere = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "other@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(elsewhere.status, 422, "{}", elsewhere.body);
    assert_eq!(
        elsewhere.body["error"]["details"]["fields"]["to"][0],
        "not_allowed"
    );
    assert_eq!(server.messages.lock().unwrap().len(), 2);

    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com", "language": "en" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], true);
    let url = invited.body["url"].as_str().unwrap().to_owned();
    wait_messages(&server.messages, 3).await;
    let mail = decode_mail(&server.messages.lock().unwrap()[2]);
    assert!(mail.contains(&url), "{mail}");
    assert!(
        mail.contains("Create and reply to tickets in the portal"),
        "{mail}"
    );
    assert!(!mail.contains(SECRET));

    fixtures::account(&db.pool, fixtures::Role::User, "recover-me").await;
    let recovery = call(
        &app,
        "POST",
        "/api/auth/recovery",
        Some(json!({ "username": "recover-me" })),
        None,
    )
    .await;
    assert_eq!(recovery.status, 202, "{}", recovery.body);
    wait_messages(&server.messages, 4).await;
    let mail = decode_mail(&server.messages.lock().unwrap()[3]);
    assert!(mail.contains("/reset-password/"), "{mail}");
    assert!(mail.contains("Do not reply to this email"), "{mail}");
    assert!(
        !server
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains(SECRET))
    );

    let tested: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'smtp_tested' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(tested.iter().any(|m| m["result"] == "ok"));
    assert!(tested.iter().all(|m| !m.to_string().contains(SECRET)));
    assert!(tested.iter().all(|m| m.get("to").is_none()));
}

#[tokio::test]
async fn implicit_tls_and_german_test_mail_work() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    sqlx::query("UPDATE accounts SET language = 'de' WHERE username = 'admin'")
        .execute(&db.pool)
        .await
        .unwrap();
    let server = listen(Listen::Implicit, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    let saved = save(
        &app,
        &admin,
        write_body(server.port, "implicit_tls", Some(SECRET), true),
    )
    .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    wait_messages(&server.messages, 1).await;
    let body = &server.messages.lock().unwrap()[0];
    assert!(
        body.contains("Antworten Sie nicht auf diese E-Mail"),
        "{body}"
    );
    assert!(body.contains("http://localhost:3000"), "{body}");
}

#[tokio::test]
async fn auth_tls_and_delivery_failures_are_distinct_and_safe() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let admin_app = router(&db.pool, None);
    let admin = login(&admin_app, "admin", fixtures::PASSWORD)
        .await
        .session();

    let auth_server = listen(Listen::StartTls, Fault::RejectAuth).await;
    let app = router(&db.pool, Some(&auth_server.ca_pem));
    save(
        &app,
        &admin,
        write_body(auth_server.port, "starttls", Some(SECRET), true),
    )
    .await;
    let failed = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(failed.status, 502, "{}", failed.body);
    assert_eq!(failed.code(), "smtp_auth_failed");
    assert_secret_absent(&failed.body);
    assert!(auth_server.messages.lock().unwrap().is_empty());

    let plain = listen(Listen::StartTls, Fault::NoStartTls).await;
    let app = router(&db.pool, Some(&plain.ca_pem));
    save(
        &app,
        &admin,
        write_body(plain.port, "starttls", Some(SECRET), true),
    )
    .await;
    let tls = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(tls.code(), "smtp_tls_failed", "{}", tls.body);
    assert_secret_absent(&tls.body);
    assert!(!plain.commands.lock().unwrap().iter().any(|c| c == "AUTH"));

    let untrusted = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, None);
    save(
        &app,
        &admin,
        write_body(untrusted.port, "starttls", Some(SECRET), true),
    )
    .await;
    let tls = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(tls.code(), "smtp_tls_failed", "{}", tls.body);
    assert!(untrusted.messages.lock().unwrap().is_empty());

    let other = identity();
    let trusted_wrong = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&other.ca_pem));
    save(
        &app,
        &admin,
        write_body(trusted_wrong.port, "starttls", Some(SECRET), true),
    )
    .await;
    let tls = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(tls.code(), "smtp_tls_failed", "{}", tls.body);

    let reject = listen(Listen::StartTls, Fault::RejectRecipient).await;
    let app = router(&db.pool, Some(&reject.ca_pem));
    save(
        &app,
        &admin,
        write_body(reject.port, "starttls", Some(SECRET), true),
    )
    .await;
    let delivery = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(delivery.code(), "smtp_delivery_failed", "{}", delivery.body);
    let mails = reject
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c == "MAIL")
        .count();
    assert_eq!(mails, 1, "permanent refusal must not be retried");

    let closed = listen(Listen::StartTls, Fault::None).await;
    let port = closed.port;
    drop(closed);
    let app = router(&db.pool, Some(&other.ca_pem));
    save(
        &app,
        &admin,
        write_body(port, "starttls", Some(SECRET), true),
    )
    .await;
    let started = Instant::now();
    let network = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(network.code(), "smtp_network_failed", "{}", network.body);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "retries must stay bounded"
    );
    assert_secret_absent(&network.body);
    let status = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(
        status.body["connection"]["last_test_error"],
        "smtp_network_failed"
    );
    assert_secret_absent(&status.body);

    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], true);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
        if status.body["connection"]["last_delivery_error"] == "smtp_network_failed" {
            assert_secret_absent(&status.body);
            break;
        }
        assert!(Instant::now() < deadline, "{}", status.body);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn transient_failures_are_retried_and_duplicate_keys_are_not() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::TransientOnce).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), true),
    )
    .await;
    // Background delivery (the default options) retries a 450.
    let key = [0u8; 32];
    let opts = DeliverOptions {
        allow_disabled: true,
        ..DeliverOptions::default()
    };
    let retried = OutboundMessage {
        to: "shared@example.com".into(),
        subject: "Retry".into(),
        body: "portal only".into(),
        idempotency_key: None,
    };
    let sent = mail::deliver(
        &db.pool,
        &key,
        Some(server.ca_pem.as_bytes()),
        &retried,
        opts,
    )
    .await;
    assert_eq!(sent, Ok(Delivered::Sent));
    assert_eq!(server.messages.lock().unwrap().len(), 1);
    let mails = server
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c == "MAIL")
        .count();
    assert_eq!(mails, 2, "the 450 must be tried again");

    let message = OutboundMessage {
        to: "shared@example.com".into(),
        subject: "Again".into(),
        body: "portal only".into(),
        idempotency_key: Some("invitation:once".into()),
    };
    let first = mail::deliver(
        &db.pool,
        &key,
        Some(server.ca_pem.as_bytes()),
        &message,
        opts,
    )
    .await
    .unwrap();
    assert_eq!(first, Delivered::Sent);
    let second = mail::deliver(
        &db.pool,
        &key,
        Some(server.ca_pem.as_bytes()),
        &message,
        opts,
    )
    .await
    .unwrap();
    assert_eq!(second, Delivered::Duplicate);
    assert_eq!(server.messages.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn disabling_or_removing_smtp_keeps_the_portal_usable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let server = listen(Listen::StartTls, Fault::None).await;
    let app = router(&db.pool, Some(&server.ca_pem));
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    save(
        &app,
        &admin,
        write_body(server.port, "starttls", Some(SECRET), false),
    )
    .await;
    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.body["emailed"], false, "{}", invited.body);
    assert!(
        invited.body["url"]
            .as_str()
            .unwrap()
            .starts_with("http://localhost:3000/invite/")
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(server.messages.lock().unwrap().is_empty());

    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    wait_messages(&server.messages, 1).await;

    let kept = save(
        &app,
        &admin,
        write_body(server.port, "starttls", None, true),
    )
    .await;
    assert_eq!(kept.status, 200, "{}", kept.body);
    assert_eq!(kept.body["connection"]["enabled"], true);
    let test = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "help@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(test.status, 200, "{}", test.body);
    wait_messages(&server.messages, 2).await;

    let removed = call(&app, "DELETE", "/api/settings/smtp", None, Some(&admin)).await;
    assert_eq!(removed.status, 204, "{}", removed.body);
    let gone = call(&app, "GET", "/api/settings/smtp", None, Some(&admin)).await;
    assert!(gone.body["connection"].is_null());
    let invited = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "person@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(invited.status, 201, "{}", invited.body);
    assert_eq!(invited.body["emailed"], false);
    let missing = call(
        &app,
        "POST",
        "/api/settings/smtp/test",
        Some(json!({ "to": "shared@example.com" })),
        Some(&admin),
    )
    .await;
    assert_eq!(missing.code(), "smtp_not_configured");
}

#[tokio::test]
async fn a_changed_connection_is_what_the_next_delivery_uses() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let first = listen(Listen::StartTls, Fault::None).await;
    let second = listen(Listen::StartTls, Fault::None).await;
    let app_first = router(&db.pool, Some(&first.ca_pem));
    let admin = login(&app_first, "admin", fixtures::PASSWORD)
        .await
        .session();
    save(
        &app_first,
        &admin,
        write_body(first.port, "starttls", Some(SECRET), true),
    )
    .await;
    call(
        &app_first,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "a@example.com" })),
        Some(&admin),
    )
    .await;
    wait_messages(&first.messages, 1).await;

    let mut body = write_body(second.port, "starttls", Some(SECRET), true);
    body["sender_name"] = json!("Updated Desk");
    let app_second = router(&db.pool, Some(&second.ca_pem));
    let updated = save(&app_second, &admin, body).await;
    assert_eq!(updated.status, 200, "{}", updated.body);
    assert_eq!(updated.body["connection"]["sender_name"], "Updated Desk");
    call(
        &app_second,
        "POST",
        "/api/invitations",
        Some(json!({ "email": "b@example.com" })),
        Some(&admin),
    )
    .await;
    wait_messages(&second.messages, 1).await;
    assert_eq!(first.messages.lock().unwrap().len(), 1);
    assert!(second.messages.lock().unwrap()[0].contains("Updated Desk"));

    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM audit_events WHERE event_type LIKE 'smtp_%' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(events, ["smtp_configured", "smtp_updated"]);
}

#[tokio::test]
async fn dns_failure_is_a_bounded_network_error() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let app = router(&db.pool, None);
    let admin = login(&app, "admin", fixtures::PASSWORD).await.session();
    save(
        &app,
        &admin,
        json!({
            "host": "no-such-host.invalid",
            "port": 587,
            "security": "starttls",
            "sender_name": "Tikkit",
            "sender_address": "help@example.com",
            "username": USER,
            "password": SECRET,
            "enabled": true,
        }),
    )
    .await;
    let started = Instant::now();
    let error = mail::deliver(
        &db.pool,
        &[0u8; 32],
        None,
        &OutboundMessage {
            to: "shared@example.com".into(),
            subject: "dns".into(),
            body: "portal".into(),
            idempotency_key: None,
        },
        DeliverOptions {
            timeout: Duration::from_secs(2),
            max_attempts: 1,
            allow_disabled: true,
        },
    )
    .await;
    assert_eq!(error, Err(MailError::Network));
    assert!(started.elapsed() < Duration::from_secs(5));
}
