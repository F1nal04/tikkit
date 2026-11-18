//! The audit log against real PostgreSQL (docs/audit.md): what every event
//! carries, atomicity with the change it describes, attribution, redaction,
//! append-only enforcement and the retention path, ticket and AI timelines,
//! denied attempts, privileged-change coverage, and the Admin search/export.

mod support;

use std::collections::HashMap;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{
    Reply, TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};
use tikkit_api::{
    api::TicketStatus,
    audit::{self, Entry, Who},
    autoclose,
    lifecycle::{self, By, Policy, Transition},
};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

const NON_ADMINS: [&str; 5] = ["owner", "op", "spec", "lead", "free"];

struct World {
    db: TestDb,
    app: Router,
    ids: HashMap<&'static str, Uuid>,
    cookies: HashMap<&'static str, String>,
    desk: Uuid,
    software: Uuid,
    exempt: Uuid,
    priority: Uuid,
    urgent: Uuid,
}

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    let mut ids = HashMap::from([("admin", fixtures::bootstrap_admin(p).await)]);
    for (name, role) in [
        ("owner", Role::User),
        ("op", Role::Operator),
        ("spec", Role::Specialist),
        ("lead", Role::TeamLead),
        ("free", Role::Operator),
    ] {
        ids.insert(name, fixtures::account(p, role, name).await);
    }
    let desk = fixtures::team(p, "Desk", ids["lead"]).await.unwrap();
    for name in ["op", "spec"] {
        fixtures::member(p, desk, ids[name]).await.unwrap();
    }
    let one = |sql: &'static str| async move {
        sqlx::query_scalar::<_, Uuid>(sql)
            .fetch_one(p)
            .await
            .unwrap()
    };
    let software = one("INSERT INTO categories (name) VALUES ('Software') RETURNING id").await;
    let exempt =
        one("INSERT INTO categories (name, skip_ai) VALUES ('Network', true) RETURNING id").await;
    let priority = one("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id").await;
    let urgent = one("INSERT INTO priorities (name) VALUES ('Urgent') RETURNING id").await;
    let app = test_app(p);
    let mut cookies = HashMap::new();
    for name in ids.keys() {
        cookies.insert(*name, login(&app, name, PASSWORD).await.session());
    }
    Some(World {
        db,
        app,
        ids,
        cookies,
        desk,
        software,
        exempt,
        priority,
        urgent,
    })
}

/// A response with its headers and raw body (the export is CSV, not JSON).
struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
}

impl World {
    fn pool(&self) -> &PgPool {
        &self.db.pool
    }

    async fn get(&self, path: &str, who: &str) -> Reply {
        call(&self.app, "GET", path, None, Some(&self.cookies[who])).await
    }

    async fn send(&self, method: &str, path: &str, body: Value, who: &str) -> Reply {
        call(
            &self.app,
            method,
            path,
            Some(body),
            Some(&self.cookies[who]),
        )
        .await
    }

    /// A request with explicit headers, e.g. a client-chosen `x-request-id`.
    async fn raw(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        who: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Raw {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(who) = who {
            request = request.header("cookie", &self.cookies[who]);
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
            .insert(ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                1234,
            ))));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap();
        Raw {
            status: parts.status,
            headers: parts.headers,
            text: String::from_utf8(bytes.to_vec()).unwrap(),
        }
    }

    async fn action(&self, id: Uuid, body: Value, who: &str) -> Reply {
        self.send("POST", &format!("/api/tickets/{id}/actions"), body, who)
            .await
    }

    async fn updated_at(&self, id: Uuid) -> String {
        sqlx::query_scalar(
            "SELECT to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')
             FROM tickets WHERE id = $1",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// A staff action or assignment change with the ticket's current `updated_at`.
    async fn work(&self, id: Uuid, route: &str, mut body: Value, who: &str) -> Reply {
        body["expected_updated_at"] = json!(self.updated_at(id).await);
        self.send("POST", &format!("/api/tickets/{id}/{route}"), body, who)
            .await
    }

    /// A draft ticket of `owner` in `category` with its frozen intake.
    async fn insert(&self, category: Uuid, subject: &str) -> Uuid {
        let intake = json!({
            "subject": subject,
            "description": "The tray jams on page two.",
            "catalog": { "category_id": category, "category_name": "Software", "skip_ai": false,
                         "priority_id": self.priority, "priority_name": "Normal", "answers": [] },
            "screenshot_ids": [],
        });
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject,
                                  description, intake_snapshot)
             VALUES ($1, $2, $3, $4, 'The tray jams on page two.', $5) RETURNING id",
        )
        .bind(self.ids["owner"])
        .bind(category)
        .bind(self.priority)
        .bind(subject)
        .bind(intake)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// Submits a new ticket with AI on or off; AI-routed ones get their attempt.
    async fn submit(&self, category: Uuid, subject: &str, ai: bool) -> Uuid {
        let id = self.insert(category, subject).await;
        let mut tx = self.pool().begin().await.unwrap();
        let policy = Policy {
            ai_available: ai,
            allow_requester_skip: true,
            ..Policy::default()
        };
        let submit = Transition::Submit {
            requester_skip: false,
        };
        let by = By::Account(self.ids["owner"]);
        let status = lifecycle::transition_with(&mut tx, id, by, &submit, Some(policy))
            .await
            .unwrap();
        if status == TicketStatus::AiProcessing {
            fixtures::ai_attempt(&mut *tx, id, "processing").await;
        }
        tx.commit().await.unwrap();
        id
    }

    async fn system(&self, id: Uuid, t: Transition) {
        let mut tx = self.pool().begin().await.unwrap();
        lifecycle::transition(&mut tx, id, By::System, &t)
            .await
            .unwrap();
        if t == Transition::AiSuggestionReady {
            sqlx::query(
                "UPDATE ai_attempts SET state = 'suggestion_ready', explanation = 'Realign the tray.',
                        steps = '[\"Open the tray\"]', finished_at = clock_timestamp()
                 WHERE ticket_id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    }

    /// A ticket whose AI suggestion waits for the requester's decision.
    async fn suggested(&self, subject: &str) -> Uuid {
        let id = self.submit(self.software, subject, true).await;
        self.system(id, Transition::AiSuggestionReady).await;
        id
    }

    /// A queued ticket dispatched to Desk and claimed by `op`, through the API.
    async fn claimed(&self, subject: &str) -> Uuid {
        let id = self.submit(self.software, subject, false).await;
        let dispatch = json!({ "action": "dispatch", "team_id": self.desk });
        let r = self.work(id, "assignment", dispatch, "free").await;
        assert_eq!(r.status, 200, "{}", r.body);
        let r = self
            .work(id, "assignment", json!({ "action": "claim" }), "op")
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        id
    }

    /// A claimed ticket whose first resolution proposal awaits confirmation.
    async fn proposed(&self, subject: &str) -> Uuid {
        let id = self.claimed(subject).await;
        let start = json!({ "action": "start_work" });
        assert_eq!(
            self.work(id, "staff-actions", start, "op").await.status,
            200
        );
        let propose = json!({ "action": "propose_resolution", "summary": SUMMARY });
        let r = self.work(id, "staff-actions", propose, "op").await;
        assert_eq!(r.status, 200, "{}", r.body);
        id
    }

    async fn reference(&self, id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT reference FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    /// Audit events matching `query`, oldest first, through the Admin search.
    async fn events(&self, query: &str) -> Vec<Value> {
        let r = self
            .get(&format!("/api/admin/audit?limit=100&{query}"), "admin")
            .await;
        assert_eq!(r.status, 200, "{query}: {}", r.body);
        assert_eq!(r.body["next_cursor"], Value::Null, "{query}");
        let mut items = r.body["items"].as_array().unwrap().clone();
        items.reverse();
        items
    }

    /// The ticket's audit trail, oldest first.
    async fn trail(&self, id: Uuid) -> Vec<Value> {
        self.events(&format!("ticket_id={id}")).await
    }

    async fn one(&self, action: &str) -> Value {
        let mut events = self.events(&format!("action={action}")).await;
        assert_eq!(events.len(), 1, "{action}: {events:?}");
        events.remove(0)
    }

    async fn count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM audit_events")
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    /// The whole log as text, for "this never appears anywhere" checks.
    async fn dump(&self) -> String {
        sqlx::query_scalar(
            "SELECT COALESCE(string_agg(to_jsonb(a)::text, ' '), '') FROM audit_events a",
        )
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// The ticket and its own history, which denied attempts must not touch.
    async fn ticket_state(&self, id: Uuid) -> Value {
        sqlx::query_scalar(
            "SELECT jsonb_build_object(
                 'ticket', (SELECT to_jsonb(t) FROM tickets t WHERE t.id = $1),
                 'events', (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.id) FROM ticket_events e
                            WHERE e.ticket_id = $1),
                 'outbox', (SELECT count(*) FROM notification_outbox))",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn settings(&self, change: impl FnOnce(&mut Value)) -> Reply {
        let mut body = self.get("/api/admin/settings", "admin").await.body;
        change(&mut body);
        self.send("PUT", "/api/admin/settings", body, "admin").await
    }
}

const SUMMARY: &str = "Replaced the worn pickup roller in tray two.";

fn actions(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect()
}

fn restricted(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|c| c == "23001")
}

#[tokio::test]
async fn an_event_names_actor_role_team_resource_time_and_request() {
    let Some(w) = world().await else { return };
    let started = OffsetDateTime::now_utc();

    // An Admin change. A request ID that is not a UUIDv4 is never taken over
    // (docs/operations.md): the API answers with, and records, its own.
    let mut body = w.get("/api/admin/settings", "admin").await.body;
    body["allow_reopening"] = json!(true);
    let headers = [("x-requested-with", "tikkit"), ("x-request-id", "req-42")];
    let r = w
        .raw(
            "PUT",
            "/api/admin/settings",
            Some(body),
            Some("admin"),
            &headers,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let answered = r.headers["x-request-id"].to_str().unwrap().to_owned();
    assert_ne!(answered, "req-42");
    assert_eq!(Uuid::parse_str(&answered).unwrap().get_version_num(), 4);

    let event = w.one("organization_settings_updated").await;
    assert_eq!(event["actor"]["account_id"], json!(w.ids["admin"]));
    assert_eq!(event["actor"]["username"], "admin");
    assert_eq!(event["actor"]["role"], "admin");
    assert_eq!(event["actor"]["team_id"], Value::Null);
    assert_eq!(event["actor"]["service"], Value::Null);
    assert_eq!(event["resource_type"], "organization");
    assert_eq!(
        event["request_id"],
        json!(answered),
        "the response header's ID"
    );
    let r = w.get("/api/admin/audit?request_id=req-42", "admin").await;
    assert_eq!((r.status.as_u16(), r.code()), (400, "invalid_query"));
    assert_eq!(event["metadata"]["from"]["allow_reopening"], false);
    assert_eq!(event["metadata"]["to"]["allow_reopening"], true);
    let at = event["created_at"].as_str().unwrap();
    assert!(at.ends_with('Z'), "UTC: {at}");
    let at = OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).unwrap();
    assert!(at >= started - time::Duration::seconds(5) && at <= OffsetDateTime::now_utc());

    // A team member's ticket work carries the acting role and team, and the
    // request ID the API generated.
    let id = w.claimed("Printer jam").await;
    let trail = w.trail(id).await;
    assert_eq!(
        actions(&trail),
        [
            "ticket_submit",
            "ticket_assignment_changed",
            "ticket_assignment_changed"
        ]
    );
    let claim = &trail[2];
    assert_eq!(claim["actor"]["username"], "op");
    assert_eq!(claim["actor"]["role"], "operator");
    assert_eq!(claim["actor"]["team_id"], json!(w.desk));
    assert_eq!(claim["actor"]["team_name"], "Desk");
    assert_eq!(claim["resource_type"], "ticket");
    assert_eq!(claim["resource_id"], json!(id));
    assert_eq!(claim["ticket_reference"], json!(w.reference(id).await));
    let request_id = claim["request_id"].as_str().unwrap();
    assert!(Uuid::parse_str(request_id).is_ok(), "{request_id}");
    // One request, one correlation: the search by request ID finds exactly it.
    let same = w.events(&format!("request_id={request_id}")).await;
    assert_eq!(actions(&same), ["ticket_assignment_changed"]);

    // The ID the private SSR server sends (a UUIDv4) is the one in the response
    // header, on the ticket event, and on its audit mirror.
    let ssr = Uuid::new_v4().to_string();
    let headers = [
        ("x-requested-with", "tikkit"),
        ("x-request-id", ssr.as_str()),
    ];
    let start = json!({ "action": "start_work", "expected_updated_at": w.updated_at(id).await });
    let path = format!("/api/tickets/{id}/staff-actions");
    let r = w
        .raw("POST", &path, Some(start), Some("op"), &headers)
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.headers["x-request-id"], ssr.as_str());
    let on_ticket_event: Option<Uuid> = sqlx::query_scalar(
        "SELECT request_id FROM ticket_events WHERE ticket_id = $1 AND event_type = 'start_work'",
    )
    .bind(id)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(on_ticket_event.map(|u| u.to_string()), Some(ssr.clone()));
    let mirrored = w.events(&format!("request_id={ssr}")).await;
    assert_eq!(actions(&mirrored), ["ticket_start_work"]);
    assert_eq!(mirrored[0]["metadata"]["to_status"], "in_progress");
    // A denied attempt in a request is correlated the same way.
    let ssr = Uuid::new_v4().to_string();
    let headers = [
        ("x-requested-with", "tikkit"),
        ("x-request-id", ssr.as_str()),
    ];
    let path = format!("/api/tickets/{id}/actions");
    // Staff cannot cancel for the requester.
    let cancel = Some(json!({ "action": "cancel", "reason": "Not mine to cancel" }));
    let r = w.raw("POST", &path, cancel, Some("op"), &headers).await;
    assert_eq!(r.status, 403, "{}", r.text);
    let denied = w.events(&format!("request_id={ssr}")).await;
    assert_eq!(actions(&denied), ["ticket_cancel_denied"]);
    // The general queue operator who dispatched has no team.
    assert_eq!(trail[1]["actor"]["username"], "free");
    assert_eq!(trail[1]["actor"]["team_id"], Value::Null);
    // Background work is a named service, never an account.
    let submitted = &trail[0];
    assert_eq!(submitted["request_id"], Value::Null, "direct call, no HTTP");
    audit::system(
        w.pool(),
        "screenshot_cleanup",
        "screenshots_expired",
        json!({}),
    )
    .await
    .unwrap();
    let job = w.one("screenshots_expired").await;
    assert_eq!(job["actor"]["service"], "screenshot_cleanup");
    assert_eq!(job["actor"]["account_id"], Value::Null);
}

#[tokio::test]
async fn a_change_and_its_audit_event_commit_or_roll_back_together() {
    let Some(w) = world().await else { return };
    let id = w.submit(w.software, "Printer jam", false).await;
    let before = (w.ticket_state(id).await, w.count().await);

    // A ticket transition: the mirrored event exists inside the transaction...
    let mut tx = w.pool().begin().await.unwrap();
    sqlx::query("UPDATE tickets SET team_id = $2, assignee_account_id = $3 WHERE id = $1")
        .bind(id)
        .bind(w.desk)
        .bind(w.ids["op"])
        .execute(&mut *tx)
        .await
        .unwrap();
    lifecycle::transition(
        &mut tx,
        id,
        By::Account(w.ids["op"]),
        &Transition::StartWork,
    )
    .await
    .unwrap();
    let inside: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE ticket_id = $1 AND event_type = 'ticket_start_work'",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(inside, 1);
    // ...and is gone with it.
    tx.rollback().await.unwrap();
    assert_eq!((w.ticket_state(id).await, w.count().await), before);

    // A privileged change: the audit row is written on the change's transaction.
    let mut tx = w.pool().begin().await.unwrap();
    sqlx::query("UPDATE organization SET allow_reopening = true")
        .execute(&mut *tx)
        .await
        .unwrap();
    let entry = Entry {
        action: "organization_settings_updated",
        who: Who::Account(w.ids["admin"]),
        subject: None,
        ticket: None,
        metadata: json!({}),
    };
    audit::record(&mut *tx, entry).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(w.count().await, before.1);

    // A change cannot commit without its event: when the audit write fails,
    // the transition fails too and nothing is left behind.
    sqlx::query(
        "CREATE FUNCTION refuse() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'audit storage unavailable'; END $$",
    )
    .execute(w.pool())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse BEFORE INSERT ON audit_events
         FOR EACH ROW WHEN (NEW.event_type = 'ticket_cancel') EXECUTE FUNCTION refuse()",
    )
    .execute(w.pool())
    .await
    .unwrap();
    let cancel = json!({ "action": "cancel", "reason": "Fixed itself" });
    assert_eq!(w.action(id, cancel.clone(), "owner").await.status, 500);
    assert_eq!((w.ticket_state(id).await, w.count().await), before);
    sqlx::query("DROP TRIGGER refuse ON audit_events")
        .execute(w.pool())
        .await
        .unwrap();
    assert_eq!(w.action(id, cancel, "owner").await.status, 200);
    assert_eq!(
        actions(&w.trail(id).await),
        ["ticket_submit", "ticket_cancel"]
    );
}

#[tokio::test]
async fn accounts_sharing_an_email_stay_distinct_in_history() {
    let Some(w) = world().await else { return };
    // Every fixture account has the same email address.
    let emails: i64 = sqlx::query_scalar("SELECT count(DISTINCT email) FROM accounts")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(emails, 1);
    // One person, two accounts: an operational one and an Admin one.
    let twin = fixtures::invited_account(w.pool(), Role::Admin, "lead.admin").await;
    assert_ne!(twin, w.ids["lead"]);

    for (who, id) in [("op", w.ids["op"]), ("free", w.ids["free"])] {
        let logins = w
            .events(&format!("actor_username={who}&action=login_succeeded"))
            .await;
        assert_eq!(logins.len(), 1, "{who}");
        assert_eq!(logins[0]["actor"]["account_id"], json!(id));
        assert_eq!(logins[0]["actor"]["username"], who);
        assert_eq!(logins[0]["subject"]["account_id"], json!(id));
        assert_eq!(w.events(&format!("actor={id}")).await, logins);
    }
    // A wrong password names the account it was tried on, by ID, not by email.
    let r = login(&w.app, "spec", "not the password").await;
    assert_eq!(r.status, 401);
    let failed = w.one("login_failed").await;
    assert_eq!(failed["actor"]["service"], "anonymous");
    assert_eq!(failed["subject"]["account_id"], json!(w.ids["spec"]));
    assert_eq!(failed["subject"]["username"], "spec");
    assert_eq!(failed["resource_type"], "account");
    assert_eq!(failed["resource_id"], json!(w.ids["spec"]));

    // The invitation is attributed to the Admin who issued it and to the
    // account it created; the Team Lead account of the same person is not involved.
    let accepted = w.one("invitation_accepted").await;
    assert_eq!(accepted["actor"]["account_id"], json!(twin));
    assert_eq!(accepted["actor"]["username"], "lead.admin");
    assert_eq!(accepted["actor"]["role"], "admin");
    assert_eq!(accepted["metadata"]["role"], "admin");
    let lead_events = w.events(&format!("actor={}", w.ids["lead"])).await;
    assert_eq!(actions(&lead_events), ["login_succeeded"]);
    // Usernames are matched exactly and case-insensitively; emails match nothing.
    assert_eq!(w.events("actor_username=LEAD.ADMIN").await.len(), 1);
    assert!(
        w.events("actor_username=shared@example.com")
            .await
            .is_empty()
    );
    assert!(
        w.events(&format!("actor_username=op&actor={}", w.ids["free"]))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn secrets_and_ticket_content_never_reach_the_log() {
    let Some(w) = world().await else { return };
    let typed = ["wrong", "pass", "phrase", "9000"].join("-");
    assert_eq!(login(&w.app, "op", &typed).await.status, 401);
    // A password typed into the username field of an unknown account.
    assert_eq!(login(&w.app, &typed, "x").await.status, 401);

    let r = w
        .send(
            "POST",
            "/api/invitations",
            json!({ "role": "operator" }),
            "admin",
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let url = r.body["url"].as_str().unwrap().to_owned();
    let token = url.rsplit('/').next().unwrap().split('?').next().unwrap();
    let accept = json!({ "token": token, "username": "newbie", "email": "shared@example.com",
                         "password": PASSWORD, "language": "en" });
    let r = call(
        &w.app,
        "POST",
        "/api/invitations/accept",
        Some(accept),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let session = r.session();

    let id = w.proposed("Paper tray jams").await;
    let reply = json!({ "idempotency_key": Uuid::new_v4(), "body": "Is it tray two or three?" });
    let r = w
        .send("POST", &format!("/api/tickets/{id}/replies"), reply, "op")
        .await;
    assert!(r.status.is_success(), "{}", r.body);
    let reason = "No longer needed";
    let cancel = json!({ "action": "cancel", "reason": reason });
    assert_eq!(w.action(id, cancel, "owner").await.status, 200);

    let dump = w.dump().await;
    let session_token = session.split('=').nth(1).unwrap();
    for (what, secret) in [
        ("password", PASSWORD),
        ("mistyped password", typed.as_str()),
        ("invitation token", token),
        ("invitation link", url.as_str()),
        ("session token", session_token),
        ("password hash", "$argon2id$"),
        ("proposal summary", SUMMARY),
        ("ticket subject", "Paper tray jams"),
        ("ticket description", "The tray jams on page two."),
        ("message body", "Is it tray two or three?"),
    ] {
        assert!(!dump.contains(secret), "{what} in the audit log");
    }
    // The proposal is recorded by version, deadline, and timeout only; the
    // cancellation keeps its required reason.
    let trail = w.trail(id).await;
    let proposal = trail
        .iter()
        .find(|e| e["action"] == "ticket_propose_resolution")
        .unwrap();
    assert_eq!(proposal["metadata"]["proposal_version"], 1);
    assert_eq!(proposal["metadata"]["timeout_hours"], 168);
    assert!(proposal["metadata"]["deadline"].is_string());
    assert!(proposal["metadata"].get("summary").is_none());
    assert_eq!(trail.last().unwrap()["metadata"]["reason"], reason);
    let message = trail
        .iter()
        .find(|e| e["action"] == "ticket_public_reply_added")
        .unwrap();
    assert!(message["metadata"]["message_id"].is_string());

    // The writer's own backstop: a secret-looking value is replaced even if a
    // caller passes one.
    let entry = Entry {
        action: "smtp_updated",
        who: Who::Account(w.ids["admin"]),
        subject: None,
        ticket: None,
        metadata: json!({ "password": typed, "nested": { "session_token": typed },
                          "password_changed": true }),
    };
    audit::record(w.pool(), entry).await.unwrap();
    let event = w.one("smtp_updated").await;
    assert_eq!(event["metadata"]["password"], "[redacted]");
    assert_eq!(event["metadata"]["nested"]["session_token"], "[redacted]");
    assert_eq!(event["metadata"]["password_changed"], true);
    assert!(!w.dump().await.contains(&typed));
}

#[tokio::test]
async fn audit_history_cannot_be_changed_outside_the_retention_path() {
    let Some(w) = world().await else { return };
    let id = w.submit(w.software, "Printer jam", false).await;
    let before = w.dump().await;
    for sql in [
        "UPDATE audit_events SET event_type = 'x'",
        "UPDATE audit_events SET metadata = '{}' WHERE ticket_id IS NOT NULL",
        "UPDATE audit_events SET actor_account_id = NULL, actor_service = 'system'",
        "DELETE FROM audit_events",
        "DELETE FROM audit_events WHERE event_type = 'login_succeeded'",
        "TRUNCATE audit_events",
    ] {
        let error = sqlx::query(sql).execute(w.pool()).await.unwrap_err();
        assert!(restricted(&error), "{sql}: {error}");
    }
    // Ticket history is equally fixed, so its mirror cannot be replayed either.
    let error = sqlx::query("DELETE FROM ticket_events WHERE ticket_id = $1")
        .bind(id)
        .execute(w.pool())
        .await
        .unwrap_err();
    assert!(restricted(&error));
    assert_eq!(w.dump().await, before);
    // Every event names an actor: an account or a service, never neither.
    let error =
        sqlx::query("INSERT INTO audit_events (event_type, actor_account_id, actor_service) VALUES ('x', $1, 'system')")
            .bind(w.ids["admin"])
            .execute(w.pool())
            .await;
    assert!(
        error.is_ok(),
        "the account wins; the service name is dropped"
    );
    let both: (Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT actor_account_id, actor_service FROM audit_events WHERE event_type = 'x'",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(both, (Some(w.ids["admin"]), None));

    // The guard is not a setting: flipping a session variable, or wrapping the
    // statement in a block, does not get a DELETE or UPDATE through.
    for sql in [
        "DELETE FROM audit_events",
        "UPDATE audit_events SET actor_username = NULL",
        "UPDATE audit_events SET metadata = metadata - 'ip'",
        "DO $$ BEGIN DELETE FROM audit_events; END $$",
        "DO $$ BEGIN UPDATE audit_events SET actor_username = NULL; END $$",
    ] {
        let mut tx = w.pool().begin().await.unwrap();
        sqlx::query("SET LOCAL tikkit.audit_retention = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let error = sqlx::query(sql).execute(&mut *tx).await.unwrap_err();
        assert!(restricted(&error), "{sql}: {error}");
    }

    // Retention: the one path that deletes, itself recorded. Old events, and an
    // old record of an earlier purge.
    sqlx::query(
        "INSERT INTO audit_events (event_type, actor_service, created_at)
         SELECT kind, 'system', now() - interval '400 days'
         FROM unnest(ARRAY['old_event', 'old_event', 'old_event', 'audit_retention_purged']) kind",
    )
    .execute(w.pool())
    .await
    .unwrap();
    let total = w.count().await;
    let now = OffsetDateTime::now_utc();
    let cutoff = now - time::Duration::days(365);
    let admin = Who::Account(w.ids["admin"]);
    let code = |error: sqlx::Error| {
        let code = error.as_database_error().and_then(|e| e.code());
        code.map(|c| c.into_owned()).unwrap_or_default()
    };
    // A cutoff younger than the 30-day floor is refused: now, the future, 29 days.
    assert_eq!(audit::RETENTION_FLOOR_DAYS, 30);
    for recent in [
        now,
        now + time::Duration::hours(1),
        now - time::Duration::days(29),
    ] {
        let mut tx = w.pool().begin().await.unwrap();
        let error = audit::purge_before(&mut tx, recent, admin)
            .await
            .unwrap_err();
        assert_eq!(code(error), "23514", "{recent}");
        let service = Who::Service(audit::RETENTION);
        let error = audit::purge_before(&mut tx, recent, service).await;
        assert!(error.is_err(), "{recent}");
    }
    // Only an active Admin (or the retention service) may purge.
    let former = fixtures::account(w.pool(), Role::Admin, "former").await;
    sqlx::query("UPDATE accounts SET is_active = false WHERE id = $1")
        .bind(former)
        .execute(w.pool())
        .await
        .unwrap();
    for who in [
        w.ids["op"],
        w.ids["lead"],
        w.ids["owner"],
        former,
        Uuid::new_v4(),
    ] {
        let mut tx = w.pool().begin().await.unwrap();
        let error = audit::purge_before(&mut tx, cutoff, Who::Account(who))
            .await
            .unwrap_err();
        assert_eq!(code(error), "42501");
    }
    assert_eq!(w.count().await, total);

    let mut tx = w.pool().begin().await.unwrap();
    assert_eq!(
        audit::purge_before(&mut tx, cutoff, admin).await.unwrap(),
        3
    );
    // The permission is the function, not the transaction: right after the
    // purge, in the same transaction, a DELETE is refused again.
    sqlx::query("SAVEPOINT s").execute(&mut *tx).await.unwrap();
    let error = sqlx::query("DELETE FROM audit_events")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(restricted(&error));
    sqlx::query("ROLLBACK TO s")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(w.count().await, total - 3 + 1);
    assert!(w.events("action=old_event").await.is_empty());
    // The earlier purge record is older than the cutoff and is still there,
    // next to the new one: who, the cutoff, the count.
    let purges = w.events("action=audit_retention_purged").await;
    assert_eq!(purges.len(), 2);
    assert_eq!(purges[0]["actor"]["service"], "system");
    let purged = &purges[1];
    assert_eq!(purged["actor"]["account_id"], json!(w.ids["admin"]));
    assert_eq!(purged["metadata"]["deleted"], 3);
    assert!(purged["metadata"]["cutoff"].is_string());
    assert_eq!(purged["resource_type"], "audit_log");

    // The retention job is a named service. Nothing recent is ever deleted.
    let mut tx = w.pool().begin().await.unwrap();
    let job = Who::Service(audit::RETENTION);
    let floor = now - time::Duration::days(audit::RETENTION_FLOOR_DAYS + 1);
    assert_eq!(audit::purge_before(&mut tx, floor, job).await.unwrap(), 0);
    tx.commit().await.unwrap();
    let purges = w.events("action=audit_retention_purged").await;
    assert_eq!(purges.len(), 3);
    assert_eq!(purges[2]["actor"]["service"], "retention");
    // Still no other way.
    let error = sqlx::query("DELETE FROM audit_events")
        .execute(w.pool())
        .await
        .unwrap_err();
    assert!(restricted(&error));
}

#[tokio::test]
async fn anonymizing_an_account_blanks_its_usernames_and_ips_only() {
    let Some(w) = world().await else { return };
    let (op, admin) = (w.ids["op"], w.ids["admin"]);
    // History of `op`: a sign-in (actor and subject, with IP), a failed sign-in
    // (subject, with IP), ticket work, and an Admin change about the account.
    assert_eq!(login(&w.app, "op", "not the password").await.status, 401);
    let id = w.claimed("Printer jam").await;
    let update = json!({ "role": "specialist", "expertise": "Printers", "team_id": w.desk });
    let r = w
        .send("PUT", &format!("/api/accounts/{op}"), update, "admin")
        .await;
    assert_eq!(r.status, 204, "{}", r.body);

    // Everything that must not change: identity, action, time, and all other accounts' rows.
    let fixed = |pool: &PgPool| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Value>(
                "SELECT jsonb_agg(jsonb_build_array(a.id, a.event_type, a.created_at, a.actor_account_id,
                            a.subject_account_id, a.actor_role, a.actor_team_id, a.request_id,
                            a.resource_type, a.resource_id, a.ticket_id, a.metadata - 'ip',
                            CASE WHEN a.actor_account_id IS DISTINCT FROM $1 THEN a.actor_username END,
                            CASE WHEN a.subject_account_id IS DISTINCT FROM $1 THEN a.subject_username END,
                            CASE WHEN a.actor_account_id IS DISTINCT FROM $1
                                  AND a.subject_account_id IS DISTINCT FROM $1 THEN a.metadata END)
                        ORDER BY a.id)
                 FROM audit_events a WHERE a.event_type <> 'audit_account_anonymized'",
            )
            .bind(op)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let before = fixed(w.pool()).await;
    let named: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events
         WHERE (actor_account_id = $1 AND actor_username IS NOT NULL)
            OR (subject_account_id = $1 AND subject_username IS NOT NULL)
            OR ((actor_account_id = $1 OR subject_account_id = $1) AND metadata ? 'ip')",
    )
    .bind(op)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert!(named >= 4, "{named}");

    // Not an Admin, an unknown account, or plain SQL: refused, nothing changes.
    for (account, who) in [(op, w.ids["lead"]), (op, op), (Uuid::new_v4(), admin)] {
        let mut tx = w.pool().begin().await.unwrap();
        let result = audit::anonymize_account(&mut tx, account, Who::Account(who)).await;
        assert!(result.is_err());
    }
    for sql in [
        "UPDATE audit_events SET actor_username = NULL WHERE actor_account_id = $1",
        "UPDATE audit_events SET metadata = metadata - 'ip' WHERE subject_account_id = $1",
    ] {
        let error = sqlx::query(sql)
            .bind(op)
            .execute(w.pool())
            .await
            .unwrap_err();
        assert!(restricted(&error), "{sql}");
    }
    assert_eq!(fixed(w.pool()).await, before);

    let mut tx = w.pool().begin().await.unwrap();
    let changed = audit::anonymize_account(&mut tx, op, Who::Account(admin))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(changed, named);
    assert_eq!(fixed(w.pool()).await, before);

    // The account's events are still found by its ID, without a name or an IP.
    let acted = w.events(&format!("actor={op}")).await;
    assert_eq!(
        actions(&acted),
        ["login_succeeded", "ticket_assignment_changed"]
    );
    for event in &acted {
        assert_eq!(event["actor"]["account_id"], json!(op));
        assert_eq!(event["actor"]["username"], Value::Null);
        assert!(event["metadata"].get("ip").is_none(), "{event}");
        assert!(event["actor"]["role"].is_string());
    }
    let about = w
        .events(&format!("resource_type=account&resource_id={op}"))
        .await;
    for event in about.iter().filter(|e| !e["subject"].is_null()) {
        assert_eq!(event["subject"]["account_id"], json!(op));
        assert_eq!(event["subject"]["username"], Value::Null);
        assert!(event["metadata"].get("ip").is_none());
    }
    assert!(about.iter().any(|e| e["action"] == "login_failed"));
    assert!(about.iter().any(|e| e["action"] == "account_updated"));
    assert!(!w.dump().await.contains("\"op\""));
    // Others keep their names and IPs, also on the same ticket.
    let trail = w.trail(id).await;
    assert_eq!(trail[1]["actor"]["username"], "free");
    assert_eq!(trail[2]["actor"]["username"], Value::Null);
    let admin_login = w
        .events(&format!("actor={admin}&action=login_succeeded"))
        .await;
    assert_eq!(admin_login[0]["actor"]["username"], "admin");
    assert!(admin_login[0]["metadata"]["ip"].is_string());

    // The anonymization is itself an event about the account, without its name.
    let event = w.one("audit_account_anonymized").await;
    assert_eq!(event["actor"]["account_id"], json!(admin));
    assert_eq!(event["resource_type"], "account");
    assert_eq!(event["resource_id"], json!(op));
    assert_eq!(event["subject"], Value::Null);
    assert_eq!(event["metadata"]["events"], json!(changed));
    // Running it again finds nothing left, and the service may run it too.
    let mut tx = w.pool().begin().await.unwrap();
    let again = audit::anonymize_account(&mut tx, op, Who::Service(audit::RETENTION))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(again, 0);
}

#[tokio::test]
async fn rejected_and_denied_attempts_cannot_flood_the_log() {
    let Some(w) = world().await else { return };
    // Anonymous: bodies the extractor rejects never reach the handler's limiter.
    let tampered = json!({ "token": "t", "username": "climber", "email": "a@example.com",
                           "password": PASSWORD, "language": "en", "role": "admin" });
    for _ in 0..40 {
        let r = call(
            &w.app,
            "POST",
            "/api/invitations/accept",
            Some(tampered.clone()),
            None,
        )
        .await;
        assert_eq!((r.status.as_u16(), r.code()), (422, "unknown_field"));
    }
    let rejected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event_type = 'invitation_rejected'",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(rejected, 10, "bounded per client");

    // Signed in: one account repeating one denied action on one ticket.
    let solved = w.suggested("Solved by AI").await;
    let r = w
        .action(solved, json!({ "action": "ai_solved" }), "owner")
        .await;
    assert_eq!(r.status, 200);
    let cancel = json!({ "action": "cancel", "reason": "Escalate" });
    for _ in 0..12 {
        let r = w.action(solved, cancel.clone(), "owner").await;
        assert_eq!(r.status, 409, "the answer never changes");
    }
    assert_eq!(w.events("action=ai_solved_change_denied").await.len(), 5);
    // Another action, or another account, has its own bound.
    assert_eq!(
        w.action(solved, json!({ "action": "reopen" }), "owner")
            .await
            .status,
        409
    );
    assert_eq!(w.action(solved, cancel.clone(), "admin").await.status, 403);
    assert_eq!(w.events("action=ai_solved_change_denied").await.len(), 7);

    // Tickets the caller may not see leave no ticket-specific row, whatever
    // the endpoint: a missing ticket and an invisible one look the same.
    let other = fixtures::account(w.pool(), Role::User, "other").await;
    let other_cookie = login(&w.app, "other", PASSWORD).await.session();
    let closed = w.proposed("Printer jam").await;
    let accept = json!({ "action": "accept_resolution", "proposal_version": 1 });
    assert_eq!(w.action(closed, accept, "owner").await.status, 200);
    let before = w.count().await;
    let missing = Uuid::new_v4();
    for ticket in [solved, closed, missing] {
        // A User on the staff endpoint: the role is refused before anything else.
        let body = json!({ "action": "reopen", "expected_updated_at": "2026-01-01T00:00:00Z" });
        let path = format!("/api/tickets/{ticket}/staff-actions");
        let r = call(&w.app, "POST", &path, Some(body), Some(&other_cookie)).await;
        assert_eq!(r.status, 403, "{ticket}");
        for action in ["reopen", "cancel", "ai_not_solved"] {
            let mut body = json!({ "action": action });
            if action == "cancel" {
                body["reason"] = json!("Not mine");
            }
            let path = format!("/api/tickets/{ticket}/actions");
            let r = call(&w.app, "POST", &path, Some(body), Some(&other_cookie)).await;
            assert_eq!(r.status, 404, "{ticket} {action}");
        }
    }
    // Operators never see AI-solved records: their attempts get 404 and no row.
    for who in ["op", "lead", "free"] {
        let dispatch = json!({ "action": "dispatch", "team_id": w.desk });
        assert_eq!(
            w.work(solved, "assignment", dispatch, who).await.status,
            404
        );
        let reopen = json!({ "action": "reopen" });
        assert_eq!(
            w.work(solved, "staff-actions", reopen, who).await.status,
            404
        );
    }
    assert_eq!(w.count().await, before);
    let by_other = w.events(&format!("actor={other}")).await;
    assert_eq!(actions(&by_other), ["login_succeeded"]);
    // The owner's own denied reopen of the closed ticket is still recorded.
    assert_eq!(
        w.action(closed, json!({ "action": "reopen" }), "owner")
            .await
            .status,
        403
    );
    assert_eq!(w.events("action=ticket_reopen_denied").await.len(), 1);
}

#[tokio::test]
async fn ai_decisions_can_be_reconstructed_and_solved_records_stay_final() {
    let Some(w) = world().await else { return };
    sqlx::query("UPDATE organization SET allow_requester_ai_skip = true, allow_reopening = true")
        .execute(w.pool())
        .await
        .unwrap();

    // Confirmed solved.
    let solved = w.suggested("Solved by AI").await;
    let r = w
        .action(solved, json!({ "action": "ai_solved" }), "owner")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let trail = w.trail(solved).await;
    assert_eq!(
        actions(&trail),
        [
            "ticket_submit",
            "ticket_ai_suggestion_ready",
            "ticket_ai_solved"
        ]
    );
    assert_eq!(trail[0]["metadata"]["route"], "ai");
    assert_eq!(trail[0]["metadata"]["priority_name"], "Normal");
    assert_eq!(trail[0]["metadata"]["priority_id"], json!(w.priority));
    assert_eq!(trail[0]["metadata"]["category_id"], json!(w.software));
    assert_eq!(trail[1]["actor"]["service"], "ai_attempts");
    assert_eq!(trail[1]["actor"]["account_id"], Value::Null);
    let decision = &trail[2];
    assert_eq!(decision["actor"]["account_id"], json!(w.ids["owner"]));
    assert_eq!(decision["actor"]["role"], "user");
    assert_eq!(decision["metadata"]["from_status"], "awaiting_ai_decision");
    assert_eq!(decision["metadata"]["to_status"], "ai_solved");
    assert_eq!(decision["metadata"]["resolution_source"], "ai");
    assert_eq!(
        decision["metadata"]["closure_reason"],
        "requester_confirmed"
    );
    assert!(decision["metadata"]["attempt_id"].is_string());
    assert!(decision["request_id"].is_string());

    // Not solved: escalated to the general queue once.
    let unsolved = w.suggested("Still broken").await;
    let r = w
        .action(unsolved, json!({ "action": "ai_not_solved" }), "owner")
        .await;
    assert_eq!(r.status, 200);
    let trail = w.trail(unsolved).await;
    assert_eq!(
        actions(&trail),
        [
            "ticket_submit",
            "ticket_ai_suggestion_ready",
            "ticket_ai_not_solved"
        ]
    );
    assert_eq!(trail[2]["metadata"]["route"], "not_solved");
    assert_eq!(trail[2]["metadata"]["to_status"], "queued");

    // Requester bypass while the suggestion is prepared.
    let skipped = w.submit(w.software, "Skip it", true).await;
    let r = w
        .action(skipped, json!({ "action": "skip_ai" }), "owner")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let trail = w.trail(skipped).await;
    assert_eq!(actions(&trail), ["ticket_submit", "ticket_skip_ai"]);
    assert_eq!(trail[1]["metadata"]["route"], "requester_skip");
    assert_eq!(trail[1]["actor"]["username"], "owner");

    // Failed generation: the service hands off, and the event says why with
    // the attempt's stable outcome code.
    let failed = w.submit(w.software, "Provider down", true).await;
    let mut tx = w.pool().begin().await.unwrap();
    let outcome = tikkit_api::ai_attempts::CREDENTIAL_MISSING;
    let handed_off = tikkit_api::ai_attempts::invalidate(&mut tx, None, outcome)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(handed_off, 1);
    let trail = w.trail(failed).await;
    assert_eq!(actions(&trail), ["ticket_submit", "ticket_ai_failed"]);
    assert_eq!(trail[1]["metadata"]["route"], "ai_failed");
    assert_eq!(trail[1]["metadata"]["outcome"], "credential_missing");
    assert_eq!(trail[1]["actor"]["service"], "ai_attempts");

    // Category exemption and AI off: the effective routing reason, no provider.
    let exempt = w.submit(w.exempt, "Network down", true).await;
    let off = w.submit(w.software, "No AI", false).await;
    assert_eq!(
        w.trail(exempt).await[0]["metadata"]["route"],
        "category_exempt"
    );
    assert_eq!(w.trail(off).await[0]["metadata"]["route"], "ai_disabled");

    // The solved record is final. Every attempt to reopen or escalate it is
    // recorded, and nothing else changes: not the ticket, not its history,
    // not the statistics.
    let state = w.ticket_state(solved).await;
    let outcomes = w.get("/api/admin/ai/outcomes", "admin").await.body;
    let before = w.count().await;
    let attempts: [(&str, Value, &str, u16); 6] = [
        ("staff-actions", json!({ "action": "reopen" }), "owner", 403),
        ("actions", json!({ "action": "reopen" }), "owner", 409),
        (
            "actions",
            json!({ "action": "ai_not_solved" }),
            "owner",
            409,
        ),
        ("actions", json!({ "action": "reopen" }), "admin", 403),
        ("staff-actions", json!({ "action": "reopen" }), "admin", 403),
        (
            "assignment",
            json!({ "action": "dispatch", "team_id": w.desk }),
            "op",
            404,
        ),
    ];
    for (route, body, who, status) in &attempts {
        let r = match *route {
            "actions" => w.action(solved, body.clone(), who).await,
            _ => w.work(solved, route, body.clone(), who).await,
        };
        assert_eq!(r.status, *status, "{who} {body}: {}", r.body);
    }
    // Repeating the committed decision is a retry, not an attempt to change it.
    let r = w
        .action(solved, json!({ "action": "ai_solved" }), "owner")
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(w.ticket_state(solved).await, state);
    assert_eq!(
        w.get("/api/admin/ai/outcomes", "admin").await.body,
        outcomes
    );
    // Operators cannot see the record: `404`, and no row that would confirm it exists.
    assert_eq!(w.count().await, before + 5);
    let denied = w.events("action=ai_solved_change_denied").await;
    let seen: Vec<(&str, &str, &str)> = denied
        .iter()
        .map(|e| {
            (
                e["actor"]["username"].as_str().unwrap(),
                e["metadata"]["attempted"].as_str().unwrap(),
                e["metadata"]["error"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("owner", "reopen", "forbidden"),
            ("owner", "reopen", "conflict"),
            ("owner", "ai_not_solved", "conflict"),
            ("admin", "reopen", "forbidden"),
            ("admin", "reopen", "forbidden"),
        ]
    );
    for event in &denied {
        assert_eq!(event["ticket_id"], json!(solved));
        assert_eq!(event["metadata"]["status"], "ai_solved");
        assert!(event["request_id"].is_string());
    }
    // The trail of the record reads: decided once, then five refused attempts.
    let trail = w.trail(solved).await;
    assert_eq!(trail.len(), 8);
    assert_eq!(trail[2]["action"], "ticket_ai_solved");
    // Tickets the caller cannot see leave no trace when they are not AI-solved.
    let before = w.count().await;
    let r = w
        .work(off, "assignment", json!({ "action": "claim" }), "owner")
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(w.count().await, before);
}

#[tokio::test]
async fn reopen_and_cancellation_are_recorded_accepted_and_denied() {
    let Some(w) = world().await else { return };
    let id = w.proposed("Printer jam").await;
    let accept = json!({ "action": "accept_resolution", "proposal_version": 1 });
    assert_eq!(w.action(id, accept, "owner").await.status, 200);

    // Reopening is off by default: the attempt is refused and recorded.
    let state = w.ticket_state(id).await;
    let r = w.action(id, json!({ "action": "reopen" }), "owner").await;
    assert_eq!((r.status, r.code()), (StatusCode::FORBIDDEN, "forbidden"));
    let r = w
        .work(id, "staff-actions", json!({ "action": "reopen" }), "op")
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(w.ticket_state(id).await, state);
    let denied = w.events("action=ticket_reopen_denied").await;
    assert_eq!(denied.len(), 2);
    assert_eq!(denied[0]["actor"]["username"], "owner");
    assert_eq!(denied[1]["actor"]["username"], "op");
    assert_eq!(denied[1]["actor"]["team_id"], json!(w.desk));
    for event in &denied {
        assert_eq!(event["ticket_id"], json!(id));
        assert_eq!(event["metadata"]["allow_reopening"], false);
        assert_eq!(event["metadata"]["status"], "closed");
    }

    // The toggle change is audited with both values, then a reopen is accepted.
    assert_eq!(
        w.settings(|s| s["allow_reopening"] = json!(true))
            .await
            .status,
        200
    );
    let toggled = w.one("organization_settings_updated").await;
    assert_eq!(toggled["metadata"]["from"]["allow_reopening"], false);
    assert_eq!(toggled["metadata"]["to"]["allow_reopening"], true);
    assert_eq!(
        w.action(id, json!({ "action": "reopen" }), "owner")
            .await
            .status,
        200
    );
    let reopened = w.one("ticket_reopen").await;
    assert_eq!(reopened["actor"]["username"], "owner");
    assert_eq!(reopened["metadata"]["allow_reopening"], true);
    assert_eq!(reopened["metadata"]["from_status"], "closed");
    assert_eq!(reopened["metadata"]["to_status"], "in_progress");

    // Cancellation under a policy that forbids it, then one that allows it.
    assert_eq!(
        w.settings(|s| s["requester_cancellation_policy"] = json!("disabled"))
            .await
            .status,
        200
    );
    let state = w.ticket_state(id).await;
    let cancel = json!({ "action": "cancel", "reason": "Changed my mind" });
    assert_eq!(w.action(id, cancel.clone(), "owner").await.status, 403);
    // Staff cannot cancel for the requester; that is a denied attempt too.
    assert_eq!(w.action(id, cancel.clone(), "op").await.status, 403);
    assert_eq!(w.ticket_state(id).await, state);
    let denied = w.events("action=ticket_cancel_denied").await;
    assert_eq!(denied.len(), 2);
    assert_eq!(denied[0]["actor"]["username"], "owner");
    assert_eq!(denied[0]["metadata"]["cancellation_policy"], "disabled");
    assert_eq!(denied[0]["metadata"]["status"], "in_progress");
    assert!(denied[0]["metadata"].get("reason").is_none());

    assert_eq!(
        w.settings(|s| s["requester_cancellation_policy"] = json!("anytime_while_open"))
            .await
            .status,
        200
    );
    assert_eq!(w.action(id, cancel, "owner").await.status, 200);
    let cancelled = w.one("ticket_cancel").await;
    assert_eq!(cancelled["actor"]["account_id"], json!(w.ids["owner"]));
    assert_eq!(cancelled["metadata"]["from_status"], "in_progress");
    assert_eq!(cancelled["metadata"]["reason"], "Changed my mind");
    assert_eq!(
        cancelled["metadata"]["cancellation_policy"],
        "anytime_while_open"
    );
    // The earlier timeline is all still there.
    let trail = w.trail(id).await;
    assert_eq!(
        actions(&trail),
        [
            "ticket_submit",
            "ticket_assignment_changed",
            "ticket_assignment_changed",
            "ticket_start_work",
            "ticket_propose_resolution",
            "ticket_accept_resolution",
            "ticket_reopen_denied",
            "ticket_reopen_denied",
            "ticket_reopen",
            "ticket_cancel_denied",
            "ticket_cancel_denied",
            "ticket_cancel",
        ]
    );
}

#[tokio::test]
async fn resolution_priority_and_assignment_history_is_complete() {
    let Some(w) = world().await else { return };
    let id = w.proposed("Printer jam").await;

    // Rejection is its own event, not a reopen; a second proposal is version 2.
    let reject = json!({ "action": "reject_resolution", "proposal_version": 1 });
    assert_eq!(w.action(id, reject.clone(), "owner").await.status, 200);
    // A stale decision changes nothing and rewrites nothing.
    let before = w.trail(id).await;
    assert_eq!(w.action(id, reject, "owner").await.status, 409);
    assert_eq!(w.trail(id).await, before);

    // Priority up and down: both selections stay, the original stays in the intake.
    for (priority, who) in [(w.urgent, "op"), (w.priority, "lead")] {
        let body = json!({ "action": "set_priority", "priority_id": priority });
        let r = w.work(id, "staff-actions", body, who).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    // The Team Lead reassigns a member's ticket inside the team.
    let assign = json!({ "action": "assign", "assignee_account_id": w.ids["spec"] });
    assert_eq!(w.work(id, "assignment", assign, "lead").await.status, 200);
    let propose = json!({ "action": "propose_resolution", "summary": SUMMARY });
    assert_eq!(
        w.work(id, "staff-actions", propose, "spec").await.status,
        200
    );

    // The proposal runs out unanswered: the worker closes it as a timeout.
    sqlx::query(
        "UPDATE tickets SET confirmation_deadline = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(id)
    .execute(w.pool())
    .await
    .unwrap();
    assert!(autoclose::close_one(w.pool()).await.unwrap());

    let trail = w.trail(id).await;
    assert_eq!(
        actions(&trail),
        [
            "ticket_submit",
            "ticket_assignment_changed",
            "ticket_assignment_changed",
            "ticket_start_work",
            "ticket_propose_resolution",
            "ticket_reject_resolution",
            "ticket_priority_changed",
            "ticket_priority_changed",
            "ticket_assignment_changed",
            "ticket_propose_resolution",
            "ticket_confirmation_timeout",
        ]
    );
    let meta = |i: usize| &trail[i]["metadata"];
    // Dispatch from the general queue, then the claim.
    assert_eq!(meta(1)["action"], "dispatch");
    assert_eq!(meta(1)["previous_team_id"], Value::Null);
    assert_eq!(meta(1)["team_id"], json!(w.desk));
    assert_eq!(meta(2)["action"], "claim");
    assert_eq!(meta(2)["previous_assignee_account_id"], Value::Null);
    assert_eq!(meta(2)["assignee_account_id"], json!(w.ids["op"]));
    assert_eq!(meta(5)["proposal_version"], 1);
    assert_eq!(trail[5]["actor"]["username"], "owner");
    assert_eq!(meta(5)["to_status"], "in_progress");
    // Priority: previous and new, by ID and name, with the acting account.
    assert_eq!(meta(6)["previous_priority_name"], "Normal");
    assert_eq!(meta(6)["priority_name"], "Urgent");
    assert_eq!(meta(6)["priority_id"], json!(w.urgent));
    assert_eq!(trail[6]["actor"]["username"], "op");
    assert_eq!(meta(7)["previous_priority_name"], "Urgent");
    assert_eq!(meta(7)["priority_name"], "Normal");
    assert_eq!(trail[7]["actor"]["role"], "team_lead");
    assert_eq!(
        meta(0)["priority_name"],
        "Normal",
        "the requester's selection"
    );
    // The lead's reassignment: acting role and team scope, previous and new assignee.
    assert_eq!(meta(8)["action"], "assign");
    assert_eq!(meta(8)["previous_assignee_account_id"], json!(w.ids["op"]));
    assert_eq!(meta(8)["assignee_account_id"], json!(w.ids["spec"]));
    assert_eq!(trail[8]["actor"]["role"], "team_lead");
    assert_eq!(trail[8]["actor"]["team_id"], json!(w.desk));
    // The second proposal, by the Specialist: a plain staff role, no extra scope.
    assert_eq!(meta(9)["proposal_version"], 2);
    assert_eq!(trail[9]["actor"]["role"], "specialist");
    // Auto-close: the system actor, the proposal, its deadline and timeout
    // snapshot, and a closure reason that is not a requester acceptance.
    let closed = &trail[10];
    assert_eq!(closed["actor"]["service"], "auto_close");
    assert_eq!(closed["actor"]["account_id"], Value::Null);
    assert_eq!(closed["metadata"]["proposal_version"], 2);
    assert_eq!(closed["metadata"]["closure_reason"], "confirmation_timeout");
    assert_eq!(closed["metadata"]["resolution_source"], "operator");
    assert_eq!(closed["metadata"]["timeout_hours"], 168);
    assert_eq!(closed["metadata"]["deadline"], meta(9)["deadline"]);
    assert_eq!(closed["metadata"]["to_status"], "closed");
    // The worker's event carries the ID of the request that caused it: the
    // proposal that ran out. Searching that ID shows cause and effect together.
    let origin = trail[9]["request_id"].as_str().unwrap();
    assert_eq!(closed["request_id"], origin);
    let correlated = w.events(&format!("request_id={origin}")).await;
    assert_eq!(
        actions(&correlated),
        ["ticket_propose_resolution", "ticket_confirmation_timeout"]
    );
    assert!(w.events("action=ticket_accept_resolution").await.is_empty());
    // The intake and the original selection were never rewritten.
    let (snapshot, category): (Value, Uuid) =
        sqlx::query_as("SELECT intake_snapshot, category_id FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!(snapshot["catalog"]["priority_name"], "Normal");
    assert_eq!(category, w.software);
}

#[tokio::test]
async fn privileged_changes_are_covered() {
    let Some(w) = world().await else { return };
    let admin = w.ids["admin"];

    // Invitation: created with its role, accepted once, then replayed and tampered with.
    let r = w
        .send(
            "POST",
            "/api/invitations",
            json!({ "role": "team_lead" }),
            "admin",
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let invitation = r.body["invitation"]["id"].clone();
    let url = r.body["url"].as_str().unwrap().to_owned();
    let token = url.rsplit('/').next().unwrap().split('?').next().unwrap();
    let accept = |username: &str| {
        json!({ "token": token, "username": username, "email": "shared@example.com",
                "password": PASSWORD, "language": "en" })
    };
    let mut tampered = accept("climber");
    tampered["role"] = json!("admin");
    let r = call(
        &w.app,
        "POST",
        "/api/invitations/accept",
        Some(tampered),
        None,
    )
    .await;
    assert_eq!((r.status.as_u16(), r.code()), (422, "unknown_field"));
    let r = call(
        &w.app,
        "POST",
        "/api/invitations/accept",
        Some(accept("second")),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let second: Uuid = r.body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        r.body["role"], "team_lead",
        "the invitation's role, not the caller's"
    );
    let r = call(
        &w.app,
        "POST",
        "/api/invitations/accept",
        Some(accept("again")),
        None,
    )
    .await;
    assert_eq!((r.status.as_u16(), r.code()), (410, "invitation_invalid"));
    let mut unknown = accept("nobody");
    unknown["token"] = json!(Uuid::new_v4().to_string());
    let r = call(
        &w.app,
        "POST",
        "/api/invitations/accept",
        Some(unknown),
        None,
    )
    .await;
    assert_eq!(r.status, 410);

    let created = w.one("invitation_created").await;
    assert_eq!(created["actor"]["account_id"], json!(admin));
    assert_eq!(created["metadata"]["role"], "team_lead");
    assert_eq!(created["resource_type"], "invitation");
    assert_eq!(created["resource_id"], invitation);
    let accepted = w.one("invitation_accepted").await;
    assert_eq!(accepted["actor"]["account_id"], json!(second));
    assert_eq!(accepted["resource_id"], invitation);
    let rejected = w.events("action=invitation_rejected").await;
    let reasons: Vec<&str> = rejected
        .iter()
        .map(|e| e["metadata"]["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        ["unexpected_field", "already_accepted", "unknown_token"]
    );
    assert_eq!(rejected[0]["metadata"]["field"], "role");
    assert_eq!(rejected[1]["resource_id"], invitation);
    assert_eq!(rejected[2]["resource_id"], Value::Null);
    for event in &rejected {
        assert_eq!(event["actor"]["service"], "anonymous");
    }
    assert!(!w.dump().await.contains(token));
    // Neither rejected attempt created an account.
    let extra: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts WHERE username IN ('climber', 'again', 'nobody')",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(extra, 0);
    // Revocation.
    let r = w.send("POST", "/api/invitations", json!({}), "admin").await;
    let revoked = r.body["invitation"]["id"].as_str().unwrap().to_owned();
    let r = w
        .send(
            "POST",
            &format!("/api/invitations/{revoked}/revoke"),
            json!({}),
            "admin",
        )
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert_eq!(
        w.one("invitation_revoked").await["resource_id"],
        json!(revoked)
    );

    // Role, Specialist field, and team membership of the exact account.
    let free = w.ids["free"];
    let update = json!({ "role": "specialist", "expertise": "Printers", "team_id": w.desk });
    let r = w
        .send("PUT", &format!("/api/accounts/{free}"), update, "admin")
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    let updated = w.one("account_updated").await;
    assert_eq!(updated["actor"]["account_id"], json!(admin));
    assert_eq!(updated["subject"]["account_id"], json!(free));
    assert_eq!(updated["subject"]["username"], "free");
    assert_eq!(updated["metadata"]["from_role"], "operator");
    assert_eq!(updated["metadata"]["to_role"], "specialist");
    assert_eq!(updated["metadata"]["to_expertise"], "Printers");
    let joined = w.one("team_membership_changed").await;
    assert_eq!(joined["subject"]["account_id"], json!(free));
    assert_eq!(joined["metadata"]["from_team_id"], Value::Null);
    assert_eq!(joined["metadata"]["to_team_id"], json!(w.desk));

    // Team creation appoints its one lead; a replacement is one event with
    // the previous and the new lead.
    let create = json!({ "name": "Field", "lead_account_id": second });
    let r = w.send("POST", "/api/teams", create, "admin").await;
    assert_eq!(r.status, 201, "{}", r.body);
    let field = r.body["id"].clone();
    let team_created = w.one("team_created").await;
    assert_eq!(team_created["resource_type"], "team");
    assert_eq!(team_created["resource_id"], field);
    assert_eq!(
        team_created["metadata"]["team"]["lead_account_id"],
        json!(second)
    );
    let third = fixtures::account(w.pool(), Role::TeamLead, "third").await;
    let replace = json!({ "name": "Field", "lead_account_id": third, "archived": false });
    let path = format!("/api/teams/{}", field.as_str().unwrap());
    let r = w.send("PUT", &path, replace, "admin").await;
    assert_eq!(r.status, 200, "{}", r.body);
    let replaced = w.one("team_updated").await;
    assert_eq!(replaced["actor"]["account_id"], json!(admin));
    assert_eq!(replaced["resource_id"], field);
    assert_eq!(
        replaced["metadata"]["from"]["lead_account_id"],
        json!(second)
    );
    assert_eq!(replaced["metadata"]["to"]["lead_account_id"], json!(third));
    let leads: i64 = sqlx::query_scalar("SELECT count(*) FROM teams WHERE lead_account_id IS NULL")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(leads, 0, "never leaderless");

    // The category exemption ("Skip AI automatically") with both values.
    let path = format!("/api/categories/{}", w.software);
    let r = w
        .send("PATCH", &path, json!({ "skip_ai": true }), "admin")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let exempted = w.one("catalog_item_updated").await;
    assert_eq!(exempted["resource_type"], "category");
    assert_eq!(exempted["resource_id"], json!(w.software));
    assert_eq!(exempted["metadata"]["before"]["skip_ai"], false);
    assert_eq!(exempted["metadata"]["item"]["skip_ai"], true);

    // Policy: the requester-skip toggle and the confirmation timeout.
    let r = w
        .settings(|s| {
            s["allow_requester_ai_skip"] = json!(true);
            s["resolution_confirmation_hours"] = json!(72);
        })
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let policy = w.one("organization_settings_updated").await;
    assert_eq!(policy["metadata"]["from"]["allow_requester_ai_skip"], false);
    assert_eq!(policy["metadata"]["to"]["allow_requester_ai_skip"], true);
    assert_eq!(
        policy["metadata"]["from"]["resolution_confirmation_hours"],
        168
    );
    assert_eq!(
        policy["metadata"]["to"]["resolution_confirmation_hours"],
        72
    );

    // Deactivation ends sessions; both are recorded against the account.
    let r = w
        .send(
            "POST",
            &format!("/api/accounts/{free}/deactivate"),
            json!({}),
            "admin",
        )
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert_eq!(
        w.one("account_deactivated").await["subject"]["account_id"],
        json!(free)
    );
    // Everything above happened in requests: each event has its request ID.
    let missing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events
         WHERE request_id IS NULL AND event_type NOT IN ('login_succeeded')
           AND created_at > (SELECT min(created_at) FROM audit_events WHERE event_type = 'invitation_created')",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(missing, 0);
}

#[tokio::test]
async fn search_is_admin_only_paginated_filtered_and_writes_nothing() {
    let Some(w) = world().await else { return };
    let id = w.proposed("Printer jam").await;
    for who in NON_ADMINS {
        for path in ["/api/admin/audit", "/api/admin/audit/actions"] {
            let r = w.get(path, who).await;
            assert_eq!(
                (r.status.as_u16(), r.code()),
                (403, "forbidden"),
                "{who} {path}"
            );
            assert!(r.body.get("items").is_none());
        }
        let r = w
            .send("POST", "/api/admin/audit/export", json!({}), who)
            .await;
        assert_eq!((r.status.as_u16(), r.code()), (403, "forbidden"), "{who}");
    }
    for (method, path) in [
        ("GET", "/api/admin/audit"),
        ("GET", "/api/admin/audit/actions"),
        ("POST", "/api/admin/audit/export"),
    ] {
        let r = call(&w.app, method, path, None, None).await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (401, "unauthorized"),
            "{path}"
        );
    }
    // Denied reads were not recorded as events either.
    let total = w.count().await;

    // Newest first, in pages that never repeat or skip.
    let mut seen = Vec::new();
    let mut cursor = String::new();
    loop {
        let r = w
            .get(&format!("/api/admin/audit?limit=4{cursor}"), "admin")
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        let items = r.body["items"].as_array().unwrap();
        assert!(items.len() <= 4);
        seen.extend(items.iter().map(|e| e["id"].as_i64().unwrap()));
        match r.body["next_cursor"].as_str() {
            Some(next) => cursor = format!("&cursor={next}"),
            None => break,
        }
    }
    assert_eq!(seen.len() as i64, total);
    let mut sorted = seen.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    assert_eq!(seen, sorted, "strictly descending, no duplicates");
    let r = w.get("/api/admin/audit?limit=1000", "admin").await;
    assert_eq!(
        r.body["items"].as_array().unwrap().len() as i64,
        total.min(100)
    );

    // Filters combine, in SQL, before the page limit.
    let reference = w.reference(id).await;
    let by_reference = w.events(&format!("ticket_reference={reference}")).await;
    assert_eq!(by_reference, w.trail(id).await);
    assert_eq!(by_reference.len(), 5);
    let by_resource = w
        .events(&format!("resource_type=ticket&resource_id={id}"))
        .await;
    assert_eq!(by_resource, by_reference);
    let op_on_ticket = w
        .events(&format!("ticket_id={id}&actor={}", w.ids["op"]))
        .await;
    assert_eq!(
        actions(&op_on_ticket),
        [
            "ticket_assignment_changed",
            "ticket_start_work",
            "ticket_propose_resolution"
        ]
    );
    assert_eq!(
        w.events(&format!("ticket_id={id}&action=ticket_start_work"))
            .await
            .len(),
        1
    );
    assert!(w.events("ticket_reference=999999").await.is_empty());
    assert!(
        w.events(&format!("ticket_id={}", Uuid::new_v4()))
            .await
            .is_empty()
    );
    // Time range: inclusive start, exclusive end.
    let first = by_reference[0]["created_at"].as_str().unwrap();
    let last = by_reference[4]["created_at"].as_str().unwrap();
    let range = |from: &str, before: &str| {
        format!("ticket_id={id}&created_from={from}&created_before={before}").replace('+', "%2B")
    };
    let within = w.events(&range(first, last)).await;
    assert_eq!(within.len(), 4, "the last event is excluded");
    assert_eq!(within[0]["id"], by_reference[0]["id"]);
    assert_eq!(
        w.events("created_before=2000-01-01T00:00:00Z").await.len(),
        0
    );

    let listed = w.get("/api/admin/audit/actions", "admin").await;
    let listed: Vec<&str> = listed.body["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        listed,
        [
            "login_succeeded",
            "ticket_assignment_changed",
            "ticket_propose_resolution",
            "ticket_start_work",
            "ticket_submit"
        ]
    );

    for bad in [
        "action=Not%20An%20Identifier",
        "actor=not-a-uuid",
        "ticket_reference=abc",
        "created_from=yesterday",
        "created_from=2026-02-01T00:00:00Z&created_before=2026-01-01T00:00:00Z",
        "request_id=",
        "request_id=req-1",
        "unknown=1",
    ] {
        let r = w.get(&format!("/api/admin/audit?{bad}"), "admin").await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (400, "invalid_query"),
            "{bad}"
        );
    }
    let r = w.get("/api/admin/audit?cursor=nonsense", "admin").await;
    assert_eq!(r.status, 422);
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let r = w.send(method, "/api/admin/audit", json!({}), "admin").await;
        assert_eq!(r.status, 405, "{method}");
    }
    // Searching is a pure read: no event, nothing else.
    assert_eq!(w.count().await, total);
}

#[tokio::test]
async fn export_is_permissioned_bounded_neutralized_and_audited() {
    let Some(w) = world().await else { return };
    // A username a spreadsheet would run as a formula (inserted directly: the
    // API's own username rules would refuse it).
    let formula = "=HYPERLINK(\"http://evil.example\",\"x\")";
    let evil = fixtures::account(w.pool(), Role::Operator, formula).await;
    let entry = Entry {
        action: "profile_updated",
        who: Who::Account(evil),
        subject: Some(evil),
        ticket: None,
        metadata: json!({ "fields": ["email"] }),
    };
    audit::record(w.pool(), entry).await.unwrap();

    let csrf = [("x-requested-with", "tikkit")];
    let export = |query: &'static str| {
        let path = format!("/api/admin/audit/export?{query}");
        let w = &w;
        async move { w.raw("POST", &path, None, Some("admin"), &csrf).await }
    };
    // Without the CSRF header the export does not run and is not recorded.
    let r = w
        .raw("POST", "/api/admin/audit/export", None, Some("admin"), &[])
        .await;
    assert_eq!(r.status, 403);
    assert!(r.text.contains("csrf_rejected"));
    assert!(w.events("action=audit_exported").await.is_empty());

    let r = export("action=profile_updated").await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.headers["content-type"], "text/csv; charset=utf-8");
    assert_eq!(r.headers["cache-control"], "private, no-store");
    assert_eq!(r.headers["x-export-rows"], "1");
    assert_eq!(r.headers["x-export-truncated"], "false");
    let disposition = r.headers["content-disposition"].to_str().unwrap();
    assert!(disposition.starts_with("attachment; filename=\"tikkit-audit-"));
    let lines: Vec<&str> = r.text.split("\r\n").collect();
    assert_eq!(lines.len(), 3, "header, one row, trailing newline");
    assert!(lines[0].starts_with("\"id\",\"time_utc\",\"action\",\"actor_account_id\""));
    let row = lines[1];
    // The formula is text now: prefixed, and its quotes doubled.
    assert!(
        row.contains("\"'=HYPERLINK(\"\"http://evil.example\"\",\"\"x\"\")\""),
        "{row}"
    );
    assert!(row.contains("\"profile_updated\""));
    assert!(row.contains(&format!("\"{evil}\",\"'=HYPERLINK")));
    assert!(row.contains("\"operator\""));

    // The export is itself an event: who, the filters, how many rows.
    let exported = w.one("audit_exported").await;
    assert_eq!(exported["actor"]["account_id"], json!(w.ids["admin"]));
    assert_eq!(exported["resource_type"], "audit_log");
    assert_eq!(exported["metadata"]["rows"], 1);
    assert_eq!(exported["metadata"]["truncated"], false);
    assert_eq!(exported["metadata"]["filters"]["action"], "profile_updated");
    assert!(exported["request_id"].is_string());

    // Bounded: a result longer than the limit is cut and says so.
    sqlx::query(
        "INSERT INTO audit_events (event_type, actor_service)
         SELECT 'bulk_test', 'system' FROM generate_series(1, $1)",
    )
    .bind(audit::EXPORT_MAX as i32 + 1)
    .execute(w.pool())
    .await
    .unwrap();
    let r = export("action=bulk_test").await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.headers["x-export-rows"],
        audit::EXPORT_MAX.to_string().as_str()
    );
    assert_eq!(r.headers["x-export-truncated"], "true");
    assert_eq!(r.text.matches("\r\n").count() as i64, audit::EXPORT_MAX + 1);
    let exports = w.events("action=audit_exported").await;
    assert_eq!(exports.len(), 2);
    assert_eq!(exports[1]["metadata"]["truncated"], true);
    assert_eq!(exports[1]["metadata"]["rows"], audit::EXPORT_MAX);

    let r = export("action=NOPE").await;
    assert_eq!(r.status, 400);
    assert_eq!(w.events("action=audit_exported").await.len(), 2);
}

#[tokio::test]
async fn resources_follow_the_writers_conventions() {
    // The same function names the resource of new events and of the history
    // the migration backfilled.
    let Some(w) = world().await else { return };
    let (kind, id): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT resource_type, resource_id FROM audit_resource_of('team_updated', NULL, $1)",
    )
    .bind(json!({ "from": { "id": "a" }, "to": { "id": "team-1" } }))
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(
        (kind.as_deref(), id.as_deref()),
        (Some("team"), Some("team-1"))
    );
    for (event, metadata, expected) in [
        (
            "draft_created",
            json!({ "draft_id": "d1" }),
            ("draft", Some("d1")),
        ),
        ("smtp_tested", json!({}), ("smtp", None)),
        (
            "ai_credential_set",
            json!({ "provider": "openai" }),
            ("ai_credential", Some("openai")),
        ),
        ("ai_limits_updated", json!({}), ("ai_settings", None)),
        (
            "catalog_item_created",
            json!({ "kind": "priority", "item": { "id": "p1" } }),
            ("priority", Some("p1")),
        ),
        (
            "ai_attempt_orphaned",
            json!({ "ticket_id": "t1" }),
            ("ticket", Some("t1")),
        ),
        ("setup_completed", json!({}), ("organization", None)),
    ] {
        let got: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT resource_type, resource_id FROM audit_resource_of($1, NULL, $2)",
        )
        .bind(event)
        .bind(metadata)
        .fetch_one(w.pool())
        .await
        .unwrap();
        assert_eq!(
            (got.0.as_deref(), got.1.as_deref()),
            (Some(expected.0), expected.1),
            "{event}"
        );
    }
}
