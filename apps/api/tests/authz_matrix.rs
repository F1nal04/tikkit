//! Authorization regression matrix: every API operation × signed out and the
//! five roles, against one fixed scenario (docs/security.md#authorization-matrix).
//!
//! The operation list comes from the OpenAPI document and is cross-checked
//! against the `.route(...)` calls in `src/lib.rs` and against the router itself
//! (an undocumented method on a known path must be `405`). A new route without a
//! row in [`MATRIX`] fails `every_route_has_a_matrix_row`: add one line.
//!
//! Scenario: `req` (User) submitted ticket T, dispatched to team Desk and
//! claimed by `op` (Operator). `spec` (Specialist) is a Desk member without
//! the claim, `lead` leads Desk, `admin` is the Admin. Expected outcome per
//! caller, in the order signed out, req, op, spec, lead, admin:
//! `A` reached the handler's own logic (any status but the denials below),
//! `U` 401 `unauthorized`, `F` 403 `forbidden`, `N` 404 `not_found`.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    net::SocketAddr,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use image::{DynamicImage, ImageFormat, RgbaImage};
use serde_json::{Value, json};
use tikkit_api::{auth::Settings, metrics::Metrics, notifications::process_one};
use tower::ServiceExt;
use uuid::Uuid;

mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

const CALLERS: [&str; 6] = ["out", "req", "op", "spec", "lead", "admin"];
const STALE: &str = "2000-01-01T00:00:00Z";
const RANGE: &str = "created_from=2026-01-01T00:00:00Z&created_before=2026-02-01T00:00:00Z";

/// What a row's path parameters and body refer to.
#[derive(Clone, Copy)]
enum Body_ {
    None,
    Json(fn(&World) -> Value),
    /// A raw screenshot upload.
    Png,
}

struct Row {
    method: &'static str,
    /// The OpenAPI path template.
    path: &'static str,
    /// The concrete request target (placeholders are World resources).
    target: &'static str,
    body: Body_,
    expect: &'static str,
}

const fn row(
    method: &'static str,
    path: &'static str,
    target: &'static str,
    body: Body_,
    expect: &'static str,
) -> Row {
    Row {
        method,
        path,
        target,
        body,
        expect,
    }
}

use Body_::{Json as J, None as E, Png};

/// Rows run top to bottom, callers left to right; denials change nothing, so
/// only allowed calls are written to avoid disturbing later rows (stale
/// versions, no-op updates, throwaway resources). Logout is last.
const MATRIX: &[Row] = &[
    // Operations: public probes and the contract.
    row("GET", "/api/health", "/api/health", E, "AAAAAA"),
    row("GET", "/api/ready", "/api/ready", E, "AAAAAA"),
    row("GET", "/api/openapi.json", "/api/openapi.json", E, "AAAAAA"),
    // Authentication: open to everyone by design, answers never depend on the caller.
    row("GET", "/api/setup", "/api/setup", E, "AAAAAA"),
    row(
        "POST",
        "/api/setup",
        "/api/setup",
        J(
            |_| json!({"username": "late", "email": "late@example.com", "password": "another long passphrase", "language": "en"}),
        ),
        "AAAAAA",
    ),
    row(
        "POST",
        "/api/auth/login",
        "/api/auth/login",
        J(|_| json!({"username": "nobody", "password": "not the password"})),
        "AAAAAA",
    ),
    row(
        "POST",
        "/api/auth/recovery",
        "/api/auth/recovery",
        J(|_| json!({"username": "nobody"})),
        "AAAAAA",
    ),
    row(
        "POST",
        "/api/auth/recovery/reset",
        "/api/auth/recovery/reset",
        J(|_| json!({"token": "unknown", "new_password": "another long passphrase"})),
        "AAAAAA",
    ),
    row(
        "POST",
        "/api/invitations/lookup",
        "/api/invitations/lookup",
        J(|_| json!({"token": "unknown"})),
        "AAAAAA",
    ),
    row(
        "POST",
        "/api/invitations/accept",
        "/api/invitations/accept",
        J(
            |_| json!({"token": "unknown", "username": "newbie", "email": "n@example.com", "password": "another long passphrase", "language": "en"}),
        ),
        "AAAAAA",
    ),
    // Own account.
    row("GET", "/api/auth/me", "/api/auth/me", E, "UAAAAA"),
    row(
        "PATCH",
        "/api/auth/me",
        "/api/auth/me",
        J(|_| json!({})),
        "UAAAAA",
    ),
    row(
        "POST",
        "/api/auth/password",
        "/api/auth/password",
        J(
            |_| json!({"current_password": "not the password", "new_password": "another long passphrase"}),
        ),
        "UAAAAA",
    ),
    row("GET", "/api/organization", "/api/organization", E, "UAAAAA"),
    row("GET", "/api/ai/policy", "/api/ai/policy", E, "UAAAAA"),
    row("GET", "/api/catalog", "/api/catalog", E, "UAAAAA"),
    row(
        "GET",
        "/api/notification-preferences",
        "/api/notification-preferences",
        E,
        "UAAAAA",
    ),
    row(
        "PUT",
        "/api/notification-preferences",
        "/api/notification-preferences",
        J(|_| json!({"email": true, "in_app": true})),
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/notifications",
        "/api/notifications",
        E,
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/notifications/unread",
        "/api/notifications/unread",
        E,
        "UAAAAA",
    ),
    row(
        "POST",
        "/api/notifications/{id}/read",
        "/api/notifications/{notification}/read",
        E,
        "UANNNN",
    ),
    // Accounts, invitations, teams, settings: Admin only.
    row("GET", "/api/accounts", "/api/accounts", E, "UFFFFA"),
    row(
        "PUT",
        "/api/accounts/{id}",
        "/api/accounts/{victim}",
        J(|_| json!({"role": "operator", "expertise": null, "team_id": null})),
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/accounts/{id}/deactivate",
        "/api/accounts/{victim}/deactivate",
        E,
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/accounts/{id}/reactivate",
        "/api/accounts/{victim}/reactivate",
        E,
        "UFFFFA",
    ),
    row("GET", "/api/invitations", "/api/invitations", E, "UFFFFA"),
    row(
        "POST",
        "/api/invitations",
        "/api/invitations",
        J(|_| json!({})),
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/invitations/{id}/reissue",
        "/api/invitations/{invitation}/reissue",
        E,
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/invitations/{id}/revoke",
        "/api/invitations/{invitation2}/revoke",
        E,
        "UFFFFA",
    ),
    row("GET", "/api/teams", "/api/teams", E, "UFFFFA"),
    row(
        "POST",
        "/api/teams",
        "/api/teams",
        J(|w| json!({"name": "Spare", "lead_account_id": w.spare_lead})),
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/teams/{id}",
        "/api/teams/{team}",
        J(|w| json!({"name": "Desk", "lead_account_id": w.lead, "archived": false})),
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/admin/settings",
        "/api/admin/settings",
        E,
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/admin/settings",
        "/api/admin/settings",
        J(|w| w.org_settings.clone()),
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/settings/smtp",
        "/api/settings/smtp",
        E,
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/settings/smtp",
        "/api/settings/smtp",
        J(
            |_| json!({"host": "127.0.0.1", "port": 1, "security": "implicit_tls", "sender_name": "Desk", "sender_address": "desk@example.com", "enabled": false, "username": "desk", "password": (["matrix", "only"].join("-"))}),
        ),
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/settings/smtp/test",
        "/api/settings/smtp/test",
        J(|_| json!({"to": "desk@example.com"})),
        "UFFFFA",
    ),
    row(
        "DELETE",
        "/api/settings/smtp",
        "/api/settings/smtp",
        E,
        "UFFFFA",
    ),
    row("GET", "/api/settings/ai", "/api/settings/ai", E, "UFFFFA"),
    row(
        "PUT",
        "/api/settings/ai",
        "/api/settings/ai",
        J(|_| json!({"enabled": false})),
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/settings/ai/credentials/{provider}",
        "/api/settings/ai/credentials/openai",
        J(|_| json!({"api_key": (["key", "for", "matrix", "test", "only"].join("-"))})),
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/settings/ai/test",
        "/api/settings/ai/test",
        E,
        "UFFFFA",
    ),
    row(
        "DELETE",
        "/api/settings/ai/credentials/{provider}",
        "/api/settings/ai/credentials/openai",
        E,
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/settings/ai/limits",
        "/api/settings/ai/limits",
        E,
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/settings/ai/limits",
        "/api/settings/ai/limits",
        J(|w| w.ai_limits.clone()),
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/settings/ai/usage",
        "/api/settings/ai/usage",
        E,
        "UFFFFA",
    ),
    // Catalog management.
    row("GET", "/api/catalog/all", "/api/catalog/all", E, "UFFFFA"),
    row(
        "POST",
        "/api/categories",
        "/api/categories",
        J(|_| json!({"name": "Matrix category"})),
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/categories/order",
        "/api/categories/order",
        J(|w| json!({"ids": [w.category]})),
        "UFFFFA",
    ),
    row(
        "PATCH",
        "/api/categories/{id}",
        "/api/categories/{category}",
        J(|_| json!({})),
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/priorities",
        "/api/priorities",
        J(|_| json!({"name": "Matrix priority"})),
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/priorities/order",
        "/api/priorities/order",
        J(|w| json!({"ids": [w.priority]})),
        "UFFFFA",
    ),
    row(
        "PATCH",
        "/api/priorities/{id}",
        "/api/priorities/{priority}",
        J(|_| json!({})),
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/intake-fields",
        "/api/intake-fields",
        J(|w| json!({"label": "Matrix field", "kind": "text", "category_ids": [w.category]})),
        "UFFFFA",
    ),
    row(
        "PUT",
        "/api/intake-fields/order",
        "/api/intake-fields/order",
        J(|w| json!({"ids": [w.field]})),
        "UFFFFA",
    ),
    row(
        "PATCH",
        "/api/intake-fields/{id}",
        "/api/intake-fields/{field}",
        J(|_| json!({})),
        "UFFFFA",
    ),
    // Admin reporting, records, audit, delivery: Admin only, including exports.
    row(
        "GET",
        "/api/admin/submissions",
        "/api/admin/submissions",
        E,
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/admin/submissions/{id}",
        "/api/admin/submissions/{ticket}",
        E,
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/admin/ai/records",
        "/api/admin/ai/records",
        E,
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/admin/ai/outcomes",
        "/api/admin/ai/outcomes",
        E,
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/admin/reports",
        "/api/admin/reports?{range}",
        E,
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/admin/reports/export",
        "/api/admin/reports/export?{range}",
        E,
        "UFFFFA",
    ),
    row("GET", "/api/admin/audit", "/api/admin/audit", E, "UFFFFA"),
    row(
        "GET",
        "/api/admin/audit/actions",
        "/api/admin/audit/actions",
        E,
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/admin/audit/export",
        "/api/admin/audit/export",
        E,
        "UFFFFA",
    ),
    row(
        "GET",
        "/api/admin/notifications",
        "/api/admin/notifications",
        E,
        "UFFFFA",
    ),
    row(
        "POST",
        "/api/admin/notifications/retry",
        "/api/admin/notifications/retry",
        E,
        "UFFFFA",
    ),
    // Staff work lists: never for a User.
    row("GET", "/api/queue", "/api/queue", E, "UFAAAA"),
    row("GET", "/api/queue/counts", "/api/queue/counts", E, "UFAAAA"),
    row(
        "GET",
        "/api/queue/bulk-options",
        "/api/queue/bulk-options",
        E,
        "UFAAAA",
    ),
    row("GET", "/api/saved-views", "/api/saved-views", E, "UFAAAA"),
    row(
        "POST",
        "/api/saved-views",
        "/api/saved-views",
        J(|_| json!({"name": "Matrix view", "filters": {}})),
        "UFAAAA",
    ),
    row(
        "DELETE",
        "/api/saved-views/{id}",
        "/api/saved-views/{view}",
        E,
        "UNANNN",
    ),
    row(
        "POST",
        "/api/tickets/bulk",
        "/api/tickets/bulk",
        J(
            |w| json!({"change": {"action": "start_work"}, "tickets": [{"id": w.ticket, "expected_updated_at": STALE}]}),
        ),
        "UFAAAA",
    ),
    // Own tickets and drafts: any signed-in account may be a requester.
    row("GET", "/api/tickets", "/api/tickets", E, "UAAAAA"),
    row(
        "POST",
        "/api/tickets",
        "/api/tickets",
        J(|_| json!({"idempotency_key": Uuid::new_v4(), "intake": {}})),
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/ticket-drafts",
        "/api/ticket-drafts",
        E,
        "UAAAAA",
    ),
    row(
        "POST",
        "/api/ticket-drafts",
        "/api/ticket-drafts",
        J(|_| json!({})),
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/ticket-drafts/{id}",
        "/api/ticket-drafts/{draft}",
        E,
        "UANNNN",
    ),
    row(
        "PATCH",
        "/api/ticket-drafts/{id}",
        "/api/ticket-drafts/{draft}",
        J(|_| json!({"version": 999, "intake": {}})),
        "UANNNN",
    ),
    row(
        "POST",
        "/api/ticket-drafts/{id}/submit",
        "/api/ticket-drafts/{draft}/submit",
        J(|_| json!({"idempotency_key": Uuid::new_v4(), "version": 999})),
        "UANNNN",
    ),
    row(
        "DELETE",
        "/api/ticket-drafts/{id}",
        "/api/ticket-drafts/{draft}?version={draft_version}",
        E,
        "UANNNN",
    ),
    // Ticket T: the requester and current Desk members read; on the claimed
    // ticket only the assignee, the Desk lead, and the Admin write as staff.
    row(
        "GET",
        "/api/tickets/{id}",
        "/api/tickets/{ticket}",
        E,
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/tickets/{id}/activity",
        "/api/tickets/{ticket}/activity",
        E,
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/tickets/{id}/messages",
        "/api/tickets/{ticket}/messages",
        E,
        "UAAAAA",
    ),
    row(
        "POST",
        "/api/tickets/{id}/actions",
        "/api/tickets/{ticket}/actions",
        J(|_| json!({"action": "accept_resolution", "proposal_version": 99})),
        "UAFFFF",
    ),
    row(
        "POST",
        "/api/tickets/{id}/staff-actions",
        "/api/tickets/{ticket}/staff-actions",
        J(|w| json!({"action": "start_work", "expected_updated_at": w.updated_at()})),
        "UFAFAA",
    ),
    row(
        "POST",
        "/api/tickets/{id}/assignment",
        "/api/tickets/{ticket}/assignment",
        J(
            |w| json!({"action": "assign", "assignee_account_id": w.op, "expected_updated_at": w.updated_at()}),
        ),
        "UFFFAA",
    ),
    row(
        "POST",
        "/api/tickets/{id}/replies",
        "/api/tickets/{ticket}/replies",
        J(|_| json!({"idempotency_key": Uuid::new_v4(), "body": "Matrix reply"})),
        "UAAFAA",
    ),
    row(
        "POST",
        "/api/tickets/{id}/notes",
        "/api/tickets/{ticket}/notes",
        J(|_| json!({"idempotency_key": Uuid::new_v4(), "body": "Matrix note"})),
        "UFAFAA",
    ),
    // Screenshots: uploads by anyone signed in; T's public image follows T's visibility.
    row(
        "POST",
        "/api/screenshots",
        "/api/screenshots?filename=shot.png",
        Png,
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/screenshots/{id}",
        "/api/screenshots/{screenshot}",
        E,
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/screenshots/{id}/preview",
        "/api/screenshots/{screenshot}/preview",
        E,
        "UAAAAA",
    ),
    row(
        "GET",
        "/api/screenshots/{id}/download",
        "/api/screenshots/{screenshot}/download",
        E,
        "UAAAAA",
    ),
    // Only the uploader reaches the handler, and an attached original is `409`
    // (boundary::requester_cannot_rewrite_submitted_content and screenshots tests).
    row(
        "DELETE",
        "/api/screenshots/{id}",
        "/api/screenshots/{screenshot}",
        E,
        "UANNNN",
    ),
    // Last: signing out ends the callers' sessions.
    row("POST", "/api/auth/logout", "/api/auth/logout", E, "AAAAAA"),
];

struct World {
    db: TestDb,
    app: Router,
    cookies: BTreeMap<&'static str, String>,
    ids: BTreeMap<&'static str, String>,
    op: Uuid,
    lead: Uuid,
    spare_lead: Uuid,
    ticket: Uuid,
    category: Uuid,
    priority: Uuid,
    field: Uuid,
    org_settings: Value,
    ai_limits: Value,
    /// T's current `updated_at`, refreshed before every request, so staff
    /// writes are decided by policy rather than by the stale-write check.
    updated_at: std::sync::Mutex<Value>,
}

impl World {
    fn updated_at(&self) -> Value {
        self.updated_at.lock().unwrap().clone()
    }
    async fn refresh(&self) {
        let at: String = sqlx::query_scalar(
            "SELECT to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM tickets WHERE id = $1",
        )
        .bind(self.ticket)
        .fetch_one(&self.db.pool)
        .await
        .unwrap();
        *self.updated_at.lock().unwrap() = Value::String(at);
    }
}

fn png() -> Vec<u8> {
    let image = RgbaImage::from_fn(4, 3, |x, y| image::Rgba([x as u8, y as u8, 128, 255]));
    let mut out = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut out, ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn id(r: &support::Reply) -> Uuid {
    let id = r.body["id"]
        .as_str()
        .or_else(|| r.body["invitation"]["id"].as_str());
    id.unwrap_or_else(|| panic!("no id: {} {}", r.status, r.body))
        .parse()
        .unwrap()
}

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    fixtures::bootstrap_admin(p).await;
    let req = fixtures::account(p, Role::User, "req").await;
    let op = fixtures::account(p, Role::Operator, "op").await;
    let spec = fixtures::account(p, Role::Specialist, "spec").await;
    let lead = fixtures::account(p, Role::TeamLead, "lead").await;
    let spare_lead = fixtures::account(p, Role::TeamLead, "spare_lead").await;
    let victim = fixtures::account(p, Role::Operator, "victim").await;
    let team = fixtures::team(p, "Desk", lead).await.unwrap();
    fixtures::member(p, team, op).await.unwrap();
    fixtures::member(p, team, spec).await.unwrap();

    // Sessions come from a separate app so the matrix app's limiter starts empty.
    let setup_app = test_app(p);
    let mut cookies = BTreeMap::new();
    for name in ["req", "op", "spec", "lead", "admin"] {
        cookies.insert(name, login(&setup_app, name, PASSWORD).await.session());
    }
    let admin = |path: &str, body: Value| {
        let cookie = cookies["admin"].clone();
        let app = setup_app.clone();
        let path = path.to_owned();
        async move { call(&app, "POST", &path, Some(body), Some(&cookie)).await }
    };
    let category = id(&admin("/api/categories", json!({"name": "Software"})).await);
    let priority = id(&admin("/api/priorities", json!({"name": "Normal"})).await);
    let field = admin(
        "/api/intake-fields",
        json!({"label": "Device", "kind": "text", "category_ids": [category]}),
    )
    .await;
    assert_eq!(field.status, StatusCode::CREATED, "{}", field.body);
    let field = id(&field);
    let invitation = id(&admin("/api/invitations", json!({})).await);
    let invitation2 = id(&admin("/api/invitations", json!({})).await);
    let get = |path: &'static str, who: &'static str| {
        let cookie = cookies[who].clone();
        let app = setup_app.clone();
        async move { call(&app, "GET", path, None, Some(&cookie)).await.body }
    };
    let org_settings = get("/api/admin/settings", "admin").await;
    let limits = get("/api/settings/ai/limits", "admin").await;
    let ai_limits: serde_json::Map<String, Value> = [
        "window_hours",
        "org_request_limit",
        "user_request_limit",
        "org_token_limit",
        "user_token_limit",
        "max_concurrency",
        "concurrency_wait_seconds",
        "breaker_failure_threshold",
        "breaker_cooldown_seconds",
        "retention_days",
    ]
    .into_iter()
    .map(|k| (k.to_owned(), limits[k].clone()))
    .collect();
    let ai_limits = Value::Object(ai_limits);

    // T: submitted by req, dispatched to Desk, claimed by op, work started.
    let ticket: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject,
             description, status, team_id, assignee_account_id)
         VALUES ($1, $2, $3, 'Printer', 'Jams', 'in_progress', $4, $5) RETURNING id",
    )
    .bind(req)
    .bind(category)
    .bind(priority)
    .bind(team)
    .bind(op)
    .fetch_one(p)
    .await
    .unwrap();
    // req attaches a screenshot to T with a public reply; op's reply notifies req.
    let upload = raw(
        &setup_app,
        "POST",
        "/api/screenshots?filename=shot.png",
        Some(&cookies["req"]),
        Body_::Png,
        None,
    )
    .await;
    assert_eq!(upload.0, StatusCode::CREATED, "{}", upload.1);
    let screenshot: Uuid = upload.1["id"].as_str().unwrap().parse().unwrap();
    for (who, extra) in [("req", json!([screenshot])), ("op", json!([]))] {
        let body =
            json!({"idempotency_key": Uuid::new_v4(), "body": "Hello", "screenshot_ids": extra});
        let r = call(
            &setup_app,
            "POST",
            &format!("/api/tickets/{ticket}/replies"),
            Some(body),
            Some(&cookies[who]),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{who}: {}", r.body);
    }
    while process_one(p, &Settings::default(), &Metrics::default())
        .await
        .unwrap()
    {}
    let inbox = get("/api/notifications", "req").await;
    let notification = inbox["items"][0]["id"]
        .as_str()
        .expect("notification")
        .to_owned();
    let draft = call(
        &setup_app,
        "POST",
        "/api/ticket-drafts",
        Some(json!({})),
        Some(&cookies["req"]),
    )
    .await;
    let view = call(
        &setup_app,
        "POST",
        "/api/saved-views",
        Some(json!({"name": "Mine", "filters": {}})),
        Some(&cookies["op"]),
    )
    .await;
    let ids = BTreeMap::from([
        ("{victim}", victim.to_string()),
        ("{invitation}", invitation.to_string()),
        ("{invitation2}", invitation2.to_string()),
        ("{team}", team.to_string()),
        ("{category}", category.to_string()),
        ("{priority}", priority.to_string()),
        ("{field}", field.to_string()),
        ("{ticket}", ticket.to_string()),
        ("{draft}", id(&draft).to_string()),
        ("{draft_version}", draft.body["version"].to_string()),
        ("{view}", id(&view).to_string()),
        ("{notification}", notification),
        ("{screenshot}", screenshot.to_string()),
        ("{range}", RANGE.to_owned()),
    ]);
    Some(World {
        app: test_app(p),
        db,
        cookies,
        ids,
        op,
        lead,
        spare_lead,
        ticket,
        category,
        priority,
        field,
        org_settings,
        ai_limits,
        updated_at: Default::default(),
    })
}

/// One request with an optional raw body; returns status and decoded JSON.
async fn raw(
    app: &Router,
    method: &str,
    target: &str,
    cookie: Option<&str>,
    body: Body_,
    world: Option<&World>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(target)
        .header("x-requested-with", "tikkit");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let body = match body {
        Body_::None => Body::empty(),
        Body_::Json(make) => {
            request = request.header("content-type", "application/json");
            Body::from(make(world.expect("world")).to_string())
        }
        Body_::Png => {
            request = request.header("content-type", "image/png");
            Body::from(png())
        }
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn outcome(status: StatusCode, body: &Value) -> char {
    match (
        status.as_u16(),
        body["error"]["code"].as_str().unwrap_or(""),
    ) {
        (401, "unauthorized") => 'U',
        (403, "forbidden") => 'F',
        (404, "not_found") => 'N',
        (403, _) => 'C',
        (405, _) => 'M',
        (500, _) => '!',
        _ => 'A',
    }
}

/// Every documented operation as `METHOD /path`.
fn documented() -> BTreeSet<String> {
    let doc = serde_json::to_value(tikkit_api::openapi()).unwrap();
    let mut ops = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            ops.insert(format!("{} {path}", method.to_uppercase()));
        }
    }
    ops
}

/// Every Rust source file of the API crate (`src/**/*.rs`), without its
/// `#[cfg(test)]` module, as `(path relative to src, text)`.
fn api_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let text = match text.find("#[cfg(test)]") {
                    Some(at) => text[..at].to_owned(),
                    None => text,
                };
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((name, text));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    assert!(out.iter().any(|(name, _)| name == "lib.rs"));
    out
}

/// The private metrics listener's router: not public, not under `/api`, and
/// checked by `observability::public_routes_do_not_expose_metrics_and_scrape_reports_an_outage`.
const PRIVATE_ROUTES: &[(&str, &str)] = &[("metrics.rs", "/metrics")];

/// Paths registered with `.route("...")` anywhere in the API source. Any way
/// of adding routes that this scan cannot see is refused outright, so the
/// scan, the OpenAPI document and the matrix cannot drift apart silently.
fn routed_paths() -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let mut refused = Vec::new();
    for (file, source) in api_sources() {
        for call in [
            ".nest(",
            ".nest_service(",
            ".route_service(",
            ".merge(",
            ".fallback(",
            ".fallback_service(",
        ] {
            if source.contains(call) {
                refused.push(format!("{file}: `{call}` adds routes the matrix cannot see; use `.route(\"/api/...\", ...)` in lib.rs"));
            }
        }
        if source.contains("Router::new()")
            && file != "lib.rs"
            && !PRIVATE_ROUTES.iter().any(|(f, _)| *f == file)
        {
            refused.push(format!("{file}: a router outside lib.rs"));
        }
        let mut rest = source.as_str();
        while let Some(at) = rest.find(".route(") {
            rest = &rest[at + ".route(".len()..];
            let Some(quoted) = rest.trim_start().strip_prefix('"') else {
                refused.push(format!("{file}: `.route(` with a non-literal path"));
                continue;
            };
            let path = &quoted[..quoted.find('"').unwrap()];
            if PRIVATE_ROUTES.contains(&(file.as_str(), path)) {
                continue;
            }
            if file != "lib.rs" || !path.starts_with("/api/") {
                refused.push(format!(
                    "{file}: route {path} outside the public /api router in lib.rs"
                ));
            }
            paths.insert(path.to_owned());
        }
    }
    assert!(refused.is_empty(), "\n{}", refused.join("\n"));
    paths
}

#[test]
fn every_route_has_a_matrix_row() {
    let documented = documented();
    let rows: BTreeSet<String> = MATRIX
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(rows.len(), MATRIX.len(), "duplicate matrix rows");
    let missing: Vec<_> = documented.difference(&rows).collect();
    let extra: Vec<_> = rows.difference(&documented).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "add a MATRIX row for every operation; missing {missing:?}, unknown {extra:?}"
    );
    let doc_paths: BTreeSet<String> = documented
        .iter()
        .map(|op| op.split_once(' ').unwrap().1.to_owned())
        .collect();
    assert_eq!(
        routed_paths(),
        doc_paths,
        "every routed path must be in the OpenAPI document (and so in the matrix)"
    );
}

#[tokio::test]
async fn every_route_allows_exactly_the_expected_callers() {
    let Some(w) = world().await else { return };
    let mut failures = Vec::new();
    for r in MATRIX {
        let mut target = r.target.to_owned();
        for (placeholder, value) in &w.ids {
            target = target.replace(placeholder, value);
        }
        assert!(!target.contains('{'), "unresolved placeholder in {target}");
        let mut actual = String::new();
        let mut seen = Vec::new();
        for caller in CALLERS {
            let cookie = w.cookies.get(caller).map(String::as_str);
            w.refresh().await;
            let (status, body) = raw(&w.app, r.method, &target, cookie, r.body, Some(&w)).await;
            actual.push(outcome(status, &body));
            seen.push(format!(
                "{caller}={} {}",
                status.as_u16(),
                body["error"]["code"]
            ));
        }
        if actual != r.expect {
            failures.push(format!(
                "{} {}: expected {} got {actual} ({})",
                r.method,
                r.path,
                r.expect,
                seen.join(", ")
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    drop(w.db);
}

/// The router has no method beyond the documented ones on any path, so the
/// OpenAPI-derived matrix really is every way in.
#[tokio::test]
async fn undocumented_methods_are_not_routed() {
    let Some(w) = world().await else { return };
    let documented = documented();
    for op in &documented {
        let path = op.split_once(' ').unwrap().1;
        let target = path
            .replace("{id}", &Uuid::new_v4().to_string())
            .replace("{provider}", "openai");
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
            if documented.contains(&format!("{method} {path}")) {
                continue;
            }
            let (status, body) = raw(
                &w.app,
                method,
                &target,
                Some(&w.cookies["admin"]),
                Body_::None,
                None,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}: {body}"
            );
        }
    }
}
