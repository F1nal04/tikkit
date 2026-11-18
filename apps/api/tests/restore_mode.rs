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
        outbound_enabled,
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
    let recovery = call(
        &restored,
        "POST",
        "/api/auth/recovery",
        Some(json!({ "username": "admin" })),
        None,
    )
    .await;
    assert_eq!(recovery.status, 202, "{}", recovery.body);
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
