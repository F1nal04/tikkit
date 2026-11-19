//! The web/API boundary over the real router: origin policy, cookies, security
//! headers, CORS, proxy trust, per-account budgets, no inbound channel, and
//! injection payloads at the edges (docs/security.md).
use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use tikkit_api::auth::Settings;
use tower::ServiceExt;
use uuid::Uuid;

mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app, test_app_with,
};

struct Sent {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Sent {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }
}

/// One request with explicit headers and socket peer.
async fn send(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
    peer: [u8; 4],
) -> Sent {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((peer, 4321))));
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    Sent {
        status: parts.status,
        headers: parts.headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

const LOCAL: [u8; 4] = [127, 0, 0, 1];
const CSRF: (&str, &str) = ("x-requested-with", "tikkit");

fn production() -> Settings {
    Settings {
        secure_cookie: true,
        hsts: true,
        public_origin: "https://help.example.com".into(),
        ..Settings::default()
    }
}

fn assert_hardened(headers: &HeaderMap, hsts: bool, what: &str) {
    let get = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_owned());
    assert_eq!(
        get("x-content-type-options").as_deref(),
        Some("nosniff"),
        "{what}"
    );
    assert_eq!(get("x-frame-options").as_deref(), Some("DENY"), "{what}");
    assert_eq!(
        get("referrer-policy").as_deref(),
        Some("no-referrer"),
        "{what}"
    );
    assert_eq!(
        get("cache-control").as_deref(),
        Some("private, no-store"),
        "{what}"
    );
    assert_eq!(
        get("cross-origin-resource-policy").as_deref(),
        Some("same-origin"),
        "{what}"
    );
    let csp = get("content-security-policy").unwrap_or_default();
    assert!(
        csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"),
        "{what}: {csp}"
    );
    assert_eq!(
        get("strict-transport-security").is_some(),
        hsts,
        "{what}: HSTS only when served over https"
    );
    for cors in [
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-methods",
        "access-control-allow-headers",
    ] {
        assert!(headers.get(cors).is_none(), "{what}: {cors}");
    }
}

#[tokio::test]
async fn every_api_response_carries_security_headers_and_no_cors_grant() {
    let Some(t) = TestDb::new().await else { return };
    fixtures::bootstrap_admin(&t.pool).await;
    for (settings, hsts) in [(Settings::default(), false), (production(), true)] {
        let app = test_app_with(&t.pool, settings);
        let evil = ("origin", "https://evil.example");
        let cases = [
            ("GET", "/api/health", vec![evil]),
            ("GET", "/api/auth/me", vec![evil]),
            ("GET", "/api/nope", vec![]),
            ("DELETE", "/api/health", vec![CSRF]),
            // A CORS preflight is never answered with a grant.
            (
                "OPTIONS",
                "/api/auth/login",
                vec![
                    evil,
                    ("access-control-request-method", "POST"),
                    (
                        "access-control-request-headers",
                        "x-requested-with, content-type",
                    ),
                ],
            ),
            ("POST", "/api/auth/login", vec![evil, CSRF]),
        ];
        for (method, path, headers) in cases {
            let r = send(&app, method, path, &headers, None, LOCAL).await;
            assert_hardened(&r.headers, hsts, &format!("{method} {path} {}", r.status));
        }
        let r = send(&app, "OPTIONS", "/api/auth/login", &[evil], None, LOCAL).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    }
}

#[tokio::test]
async fn authenticated_mutations_need_the_header_and_the_portal_origin() {
    let Some(t) = TestDb::new().await else { return };
    fixtures::bootstrap_admin(&t.pool).await;
    fixtures::account(&t.pool, Role::User, "rita").await;
    let app = test_app_with(&t.pool, production());
    let session = login(&app, "rita", PASSWORD).await.session();
    let cookie = ("cookie", session.as_str());
    let rejected = [
        vec![],
        vec![("x-requested-with", "XMLHttpRequest")],
        vec![CSRF, ("origin", "https://evil.example")],
        vec![CSRF, ("origin", "null")],
        vec![CSRF, ("origin", "http://help.example.com")],
        vec![CSRF, ("origin", "https://help.example.com.evil.example")],
        vec![CSRF, ("sec-fetch-site", "cross-site")],
        vec![CSRF, ("sec-fetch-site", "same-site")],
        vec![
            CSRF,
            ("origin", "https://help.example.com"),
            ("sec-fetch-site", "cross-site"),
        ],
    ];
    for headers in rejected {
        let r = patch_me(&app, &session, &headers).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{headers:?}");
        assert_eq!(r.code(), "csrf_rejected", "{headers:?}");
    }
    let me = send(&app, "GET", "/api/auth/me", &[cookie], None, LOCAL).await;
    assert_eq!(
        me.body["language"], "en",
        "rejected requests changed nothing"
    );
    // Same-origin browsers and non-browser callers (SSR, scripts) pass.
    for headers in [
        vec![
            CSRF,
            ("origin", "https://help.example.com"),
            ("sec-fetch-site", "same-origin"),
        ],
        vec![CSRF, ("origin", "HTTPS://HELP.EXAMPLE.COM")],
        vec![CSRF],
    ] {
        let r = patch_me(&app, &session, &headers).await;
        assert_eq!(r.status, StatusCode::OK, "{headers:?}: {}", r.body);
    }
    // Sign-in itself is a mutation: a cross-site login form cannot plant a session.
    let r = send(
        &app,
        "POST",
        "/api/auth/login",
        &[CSRF, ("origin", "https://evil.example")],
        Some(json!({"username": "rita", "password": PASSWORD})),
        LOCAL,
    )
    .await;
    assert_eq!(r.code(), "csrf_rejected");
    assert!(r.headers.get("set-cookie").is_none());
    // Reads stay possible cross-site only as far as the browser lets a page
    // read them: no CORS grant (see the headers test), and nothing changes.
    let r = send(
        &app,
        "GET",
        "/api/auth/me",
        &[cookie, ("sec-fetch-site", "cross-site")],
        None,
        LOCAL,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn every_session_cookie_is_hardened_in_production() {
    let Some(t) = TestDb::new().await else { return };
    fixtures::bootstrap_admin(&t.pool).await;
    fixtures::account(&t.pool, Role::User, "sam").await;
    let app = test_app_with(&t.pool, production());
    let check = |cookie: Option<&str>, what: &str| {
        let cookie = cookie.unwrap_or_else(|| panic!("{what}: no cookie"));
        for attribute in [
            "tikkit_session=",
            "HttpOnly",
            "SameSite=Lax",
            "Path=/",
            "Secure",
        ] {
            assert!(cookie.contains(attribute), "{what}: {cookie}");
        }
        assert!(!cookie.contains("Domain="), "{what}: host-only cookie");
    };
    let r = login(&app, "sam", PASSWORD).await;
    check(r.cookie.as_deref(), "login");
    let session = r.session();
    let body = json!({"current_password": PASSWORD, "new_password": "a brand new passphrase"});
    let r = call(
        &app,
        "POST",
        "/api/auth/password",
        Some(body),
        Some(&session),
    )
    .await;
    check(r.cookie.as_deref(), "password change");
    let session = r.session();
    let r = call(&app, "POST", "/api/auth/logout", None, Some(&session)).await;
    check(r.cookie.as_deref(), "logout");
    assert!(r.cookie.unwrap().contains("Max-Age=0"));
}

/// `PATCH /api/auth/me` as the session's account with extra `headers`.
async fn patch_me(app: &Router, session: &str, headers: &[(&str, &str)]) -> Sent {
    let mut headers = headers.to_vec();
    headers.push(("cookie", session));
    let body = json!({"language": "de"});
    send(app, "PATCH", "/api/auth/me", &headers, Some(body), LOCAL).await
}

/// Login failures from one client over a socket peer that is not a trusted proxy.
async fn fail_login(app: &Router, peer: [u8; 4], forwarded: &str, username: &str) -> Sent {
    let mut headers = vec![CSRF];
    if !forwarded.is_empty() {
        headers.push(("x-forwarded-for", forwarded));
    }
    let body = json!({"username": username, "password": "not the password"});
    send(app, "POST", "/api/auth/login", &headers, Some(body), peer).await
}

async fn audited_ips(t: &TestDb) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT metadata->>'ip' FROM audit_events WHERE event_type = 'login_failed' ORDER BY id",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn forwarding_headers_count_only_from_trusted_proxies() {
    let Some(t) = TestDb::new().await else { return };
    fixtures::bootstrap_admin(&t.pool).await;

    // Without TRUSTED_PROXIES a rotating X-Forwarded-For buys nothing.
    let app = test_app(&t.pool);
    for i in 0..10 {
        let forged = format!("198.51.100.{i}");
        let r = fail_login(&app, [203, 0, 113, 5], &forged, &format!("u{i}")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = fail_login(&app, [203, 0, 113, 5], "198.51.100.99", "fresh").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
    assert!(audited_ips(&t).await.iter().all(|ip| ip == "203.0.113.5"));

    // Behind a trusted proxy each real client has its own budget and audit IP,
    // and whatever the client itself put in the header is skipped.
    let settings = Settings {
        trusted_proxies: "10.0.0.0/8".parse().unwrap(),
        ..Settings::default()
    };
    let app = test_app_with(&t.pool, settings);
    sqlx::query("DELETE FROM audit_events WHERE event_type = 'login_failed'")
        .execute(&t.pool)
        .await
        .ok();
    let caddy = [10, 0, 0, 2];
    for i in 0..10 {
        let header = format!("192.0.2.{i}, 198.51.100.1");
        let r = fail_login(&app, caddy, &header, &format!("a{i}")).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = fail_login(&app, caddy, "192.0.2.77, 198.51.100.1", "a-next").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // Another client through the same proxy, and through proxy + SSR hop.
    let r = fail_login(&app, caddy, "198.51.100.2", "b").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = fail_login(&app, [10, 0, 0, 3], "198.51.100.3, 10.0.0.2", "c").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // An untrusted peer's header is still ignored with proxies configured.
    let r = fail_login(&app, [203, 0, 113, 6], "10.0.0.9", "d").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Garbage in the header never becomes an address.
    let r = fail_login(&app, caddy, "<script>alert(1)</script>", "e").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let ips = audited_ips(&t).await;
    // The audit deletion above may be refused by the append-only guard; read the tail.
    let tail = &ips[ips.len() - 14..];
    assert!(tail[..10].iter().all(|ip| ip == "198.51.100.1"), "{tail:?}");
    assert_eq!(
        &tail[10..],
        ["198.51.100.2", "198.51.100.3", "203.0.113.6", "10.0.0.2"]
    );
}

#[tokio::test]
async fn costly_routes_have_per_account_budgets() {
    let Some(t) = TestDb::new().await else { return };
    fixtures::bootstrap_admin(&t.pool).await;
    fixtures::account(&t.pool, Role::Admin, "admin2").await;
    let any = Uuid::new_v4();
    // (method, path, budget per minute). Bodies are deliberately invalid: the
    // budget is spent before the body is read, so each call stays cheap.
    let routes = [
        ("POST", "/api/tickets".to_owned(), 10, Some(json!({}))),
        (
            "POST",
            format!("/api/ticket-drafts/{any}/submit"),
            10,
            Some(json!({})),
        ),
        (
            "POST",
            "/api/ticket-drafts".to_owned(),
            120,
            Some(json!({"subject": 1})),
        ),
        (
            "PATCH",
            format!("/api/ticket-drafts/{any}"),
            120,
            Some(json!({})),
        ),
        (
            "POST",
            format!("/api/tickets/{any}/replies"),
            30,
            Some(json!({})),
        ),
        (
            "POST",
            format!("/api/tickets/{any}/notes"),
            30,
            Some(json!({})),
        ),
        (
            "POST",
            "/api/screenshots?filename=x.png".to_owned(),
            30,
            None,
        ),
        ("GET", "/api/queue?q=printer".to_owned(), 120, None),
        ("POST", "/api/admin/reports/export".to_owned(), 6, None),
        (
            "POST",
            "/api/admin/audit/export?action=nothing_matches".to_owned(),
            6,
            None,
        ),
        ("POST", "/api/settings/ai/test".to_owned(), 5, None),
        (
            "POST",
            "/api/settings/smtp/test".to_owned(),
            5,
            Some(json!({})),
        ),
        (
            "POST",
            "/api/admin/retention/preview".to_owned(),
            10,
            Some(json!({})),
        ),
        (
            "POST",
            "/api/admin/retention/runs".to_owned(),
            10,
            Some(json!({})),
        ),
        (
            "POST",
            format!("/api/admin/accounts/{any}/personal-data/export"),
            6,
            Some(json!({})),
        ),
        (
            "POST",
            format!("/api/admin/accounts/{any}/anonymize"),
            10,
            Some(json!({})),
        ),
    ];
    let sign_in = test_app(&t.pool);
    let admin = login(&sign_in, "admin", PASSWORD).await.session();
    let admin2 = login(&sign_in, "admin2", PASSWORD).await.session();
    for (method, path, budget, body) in routes {
        let app = test_app(&t.pool);
        // Signed-out calls never spend anyone's budget.
        for _ in 0..budget + 2 {
            let r = call(&app, method, &path, body.clone(), None).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
        for i in 0..budget {
            let r = call(&app, method, &path, body.clone(), Some(&admin)).await;
            assert_ne!(
                r.status,
                StatusCode::TOO_MANY_REQUESTS,
                "{method} {path} #{i}"
            );
            assert_ne!(
                r.status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{method} {path}: {}",
                r.body
            );
        }
        let r = call(&app, method, &path, body.clone(), Some(&admin)).await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{method} {path}");
        assert_eq!(r.code(), "rate_limited");
        let wait: u64 = r.retry_after.expect("Retry-After").parse().unwrap();
        assert!((1..=60).contains(&wait));
        // Budgets are per account.
        let r = call(&app, method, &path, body.clone(), Some(&admin2)).await;
        assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS, "{method} {path}");
    }
}

#[tokio::test]
async fn there_is_no_inbound_email_or_webhook_channel() {
    let Some(t) = TestDb::new().await else { return };
    fixtures::bootstrap_admin(&t.pool).await;
    let app = test_app(&t.pool);
    let doc = serde_json::to_value(tikkit_api::openapi()).unwrap();
    for path in doc["paths"].as_object().unwrap().keys() {
        for word in [
            "inbound", "webhook", "hook", "mailgun", "postmark", "sendgrid", "imap", "pop3",
            "incoming", "email",
        ] {
            assert!(!path.contains(word), "{path} looks like an inbound channel");
        }
    }
    let message = json!({
        "from": "Requester <someone@example.com>",
        "to": "help@example.com",
        "subject": "Re: [#1] Printer",
        "text": "please reopen",
        "headers": {"In-Reply-To": "<ticket-1@tikkit>"},
    });
    for path in [
        "/api/inbound",
        "/api/inbound/email",
        "/api/email",
        "/api/mail",
        "/api/webhooks/email",
        "/api/webhook",
        "/api/hooks/mail",
        "/api/tickets/email",
        "/api/tickets/inbound",
        "/api/tickets/1/email",
        "/api/smtp",
        "/api/settings/smtp/inbound",
    ] {
        let r = send(&app, "POST", path, &[CSRF], Some(message.clone()), LOCAL).await;
        assert!(
            matches!(
                r.status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{path}: {}",
            r.status
        );
    }
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(tickets, 0);
}

#[tokio::test]
async fn smtp_settings_refuse_metadata_destinations_and_header_injection() {
    let Some(t) = TestDb::new().await else { return };
    fixtures::bootstrap_admin(&t.pool).await;
    let app = test_app(&t.pool);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let secret = ["smtp", "matrix", "only"].join("-");
    let settings = |host: &str, name: &str, sender: &str| {
        json!({
            "host": host, "port": 465, "security": "implicit_tls", "sender_name": name,
            "sender_address": sender, "enabled": true, "username": "desk", "password": secret,
        })
    };
    for (host, name, sender, field) in [
        ("169.254.169.254", "Desk", "desk@example.com", "host"),
        ("[fe80::1]", "Desk", "desk@example.com", "host"),
        ("0.0.0.0", "Desk", "desk@example.com", "host"),
        // 169.254.169.254 in decimal, NAT64, and the AWS IPv6 metadata address.
        ("2852039166", "Desk", "desk@example.com", "host"),
        ("64:ff9b::a9fe:a9fe", "Desk", "desk@example.com", "host"),
        ("fd00:ec2::254", "Desk", "desk@example.com", "host"),
        (
            "smtp.example.com\r\nRCPT TO:<x@evil.example>",
            "Desk",
            "desk@example.com",
            "host",
        ),
        (
            "smtp.example.com",
            "Desk\r\nBcc: x@evil.example",
            "desk@example.com",
            "sender_name",
        ),
        (
            "smtp.example.com",
            "Desk",
            "desk@example.com\r\nBcc: x@evil.example",
            "sender_address",
        ),
        (
            "smtp.example.com",
            "Desk",
            "desk@example.com>, <x@evil.example",
            "sender_address",
        ),
    ] {
        let r = call(
            &app,
            "PUT",
            "/api/settings/smtp",
            Some(settings(host, name, sender)),
            Some(&admin),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{host:?} {name:?} {sender:?}"
        );
        assert!(
            r.body["error"]["details"]["fields"][field].is_array(),
            "{field}: {}",
            r.body
        );
        assert!(!r.body.to_string().contains(&secret));
    }
    // The test send only goes to the Admin's own or the sender address, and a
    // recipient cannot smuggle extra headers or recipients.
    let ok = settings("127.0.0.1", "Desk", "desk@example.com");
    let r = call(&app, "PUT", "/api/settings/smtp", Some(ok), Some(&admin)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    for to in [
        "desk@example.com\r\nBcc: x@evil.example",
        "desk@example.com, x@evil.example",
        "x@evil.example",
    ] {
        let r = call(
            &app,
            "POST",
            "/api/settings/smtp/test",
            Some(json!({"to": to})),
            Some(&admin),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{to:?}");
    }
}

#[tokio::test]
async fn requester_cannot_rewrite_submitted_content() {
    let Some(t) = TestDb::new().await else { return };
    let p = &t.pool;
    fixtures::bootstrap_admin(p).await;
    let req = fixtures::account(p, Role::User, "req").await;
    let category: Uuid =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    let priority: Uuid =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    let app = test_app(p);
    let cookie = login(&app, "req", PASSWORD).await.session();
    let intake = json!({"category_id": category, "priority_id": priority, "subject": "Printer", "description": "It jams"});
    let key = Uuid::new_v4();
    let r = call(
        &app,
        "POST",
        "/api/tickets",
        Some(json!({"idempotency_key": key, "intake": intake})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let ticket = r.body["ticket_id"]
        .as_str()
        .or(r.body["id"].as_str())
        .unwrap()
        .to_owned();
    // No route edits or deletes a ticket or a message; replaying the key with
    // other content does not rewrite the frozen intake.
    for (method, path) in [
        ("PUT", format!("/api/tickets/{ticket}")),
        ("PATCH", format!("/api/tickets/{ticket}")),
        ("DELETE", format!("/api/tickets/{ticket}")),
        ("PUT", format!("/api/tickets/{ticket}/messages")),
        ("PATCH", format!("/api/tickets/{ticket}/messages")),
        ("DELETE", format!("/api/tickets/{ticket}/messages")),
        ("PATCH", format!("/api/tickets/{ticket}/replies")),
        ("DELETE", format!("/api/tickets/{ticket}/replies")),
    ] {
        let r = call(
            &app,
            method,
            &path,
            Some(json!({"subject": "Changed"})),
            Some(&cookie),
        )
        .await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
    }
    let changed = json!({"category_id": category, "priority_id": priority, "subject": "Changed", "description": "Rewritten"});
    let r = call(
        &app,
        "POST",
        "/api/tickets",
        Some(json!({"idempotency_key": key, "intake": changed})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    let (subject, description): (String, String) =
        sqlx::query_as("SELECT subject, description FROM tickets WHERE requester_account_id = $1")
            .bind(req)
            .fetch_one(p)
            .await
            .unwrap();
    assert_eq!(
        (subject.as_str(), description.as_str()),
        ("Printer", "It jams")
    );
}

#[tokio::test]
async fn role_payloads_cannot_carry_extra_authority() {
    let Some(t) = TestDb::new().await else { return };
    let p = &t.pool;
    fixtures::bootstrap_admin(p).await;
    let lead = fixtures::account(p, Role::TeamLead, "lead").await;
    let op = fixtures::account(p, Role::Operator, "op").await;
    fixtures::team(p, "Desk", lead).await.unwrap();
    let app = test_app(p);
    let admin = login(&app, "admin", PASSWORD).await.session();
    // Forged flags are unknown fields, never silently ignored or applied.
    for extra in [
        "is_team_lead",
        "is_admin",
        "team_lead_of",
        "is_active",
        "email",
    ] {
        let mut body = json!({"role": "operator", "expertise": null, "team_id": null});
        body[extra] = json!(true);
        let r = call(
            &app,
            "PUT",
            &format!("/api/accounts/{op}"),
            Some(body),
            Some(&admin),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{extra}");
        assert_eq!(r.code(), "unknown_field", "{extra}");
    }
    let role: String = sqlx::query_scalar("SELECT role::text FROM accounts WHERE id = $1")
        .bind(op)
        .fetch_one(p)
        .await
        .unwrap();
    assert_eq!(role, "operator");
}

#[tokio::test]
async fn open_drafts_are_capped_per_account() {
    let Some(t) = TestDb::new().await else { return };
    let p = &t.pool;
    fixtures::bootstrap_admin(p).await;
    let req = fixtures::account(p, Role::User, "req").await;
    fixtures::account(p, Role::User, "other").await;
    sqlx::query(
        "INSERT INTO ticket_drafts (requester_account_id, intake)
         SELECT $1, '{}'::jsonb FROM generate_series(1, $2)",
    )
    .bind(req)
    .bind(tikkit_api::submission::MAX_OPEN_DRAFTS as i32)
    .execute(p)
    .await
    .unwrap();
    let app = test_app(p);
    let cookie = login(&app, "req", PASSWORD).await.session();
    let r = call(
        &app,
        "POST",
        "/api/ticket-drafts",
        Some(json!({})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    assert_eq!(
        r.body["error"]["details"]["fields"]["draft"],
        json!(["too_many"])
    );
    // Another account is unaffected; freeing one slot allows one more.
    let other = login(&app, "other", PASSWORD).await.session();
    let r = call(
        &app,
        "POST",
        "/api/ticket-drafts",
        Some(json!({})),
        Some(&other),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    sqlx::query("DELETE FROM ticket_drafts WHERE id = (SELECT id FROM ticket_drafts WHERE requester_account_id = $1 LIMIT 1)")
        .bind(req)
        .execute(p)
        .await
        .unwrap();
    let r = call(
        &app,
        "POST",
        "/api/ticket-drafts",
        Some(json!({})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
}

#[tokio::test]
async fn a_stuck_request_ends_at_the_deadline_and_commits_nothing() {
    let Some(t) = TestDb::new().await else { return };
    let p = &t.pool;
    fixtures::bootstrap_admin(p).await;
    let settings = Settings {
        request_timeout: std::time::Duration::from_millis(300),
        ..Settings::default()
    };
    let app = test_app_with(p, settings);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let current = call(&app, "GET", "/api/admin/settings", None, Some(&admin))
        .await
        .body;
    let mut changed = current.clone();
    changed["name"] = json!("Changed while stuck");
    // Another transaction holds the organization row, so the update blocks.
    let mut holder = p.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM organization FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let sent = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        call(
            &app,
            "PUT",
            "/api/admin/settings",
            Some(changed),
            Some(&admin),
        ),
    )
    .await
    .expect("the deadline must answer while the request is still blocked");
    assert_eq!(
        sent.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        sent.body
    );
    assert_eq!(sent.code(), "request_timeout");
    assert!(sent.body["request_id"].is_string());
    holder.rollback().await.unwrap();
    // The abandoned transaction is rolled back, never committed.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let name: String = sqlx::query_scalar("SELECT name FROM organization")
        .fetch_one(p)
        .await
        .unwrap();
    assert_eq!(json!(name), current["name"]);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event_type = 'organization_settings_updated'",
    )
    .fetch_one(p)
    .await
    .unwrap();
    assert_eq!(audited, 0);
}
