//! The Admin submission view against real PostgreSQL
//! (docs/admin-submissions.md): every submission with mixed outcomes, the
//! filters, paging, the read-only detail of AI-solved and unconfirmed records,
//! role denial, no secrets in responses, and no side effects of viewing.

mod support;

use std::collections::HashMap;

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{
    Reply, TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};
use tikkit_api::{
    ai_attempts,
    lifecycle::{self, By, Policy, Transition},
};
use uuid::Uuid;

/// Every role but Admin, by fixture name.
const NON_ADMINS: [&str; 5] = ["owner", "op", "spec", "lead", "free"];

struct World {
    db: TestDb,
    app: Router,
    ids: HashMap<&'static str, Uuid>,
    cookies: HashMap<&'static str, String>,
    desk: Uuid,
    software: Uuid,
    network: Uuid,
    priority: Uuid,
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
    let software =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    let network = sqlx::query_scalar(
        "INSERT INTO categories (name, skip_ai) VALUES ('Network', true) RETURNING id",
    )
    .fetch_one(p)
    .await
    .unwrap();
    let priority =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    sqlx::query("UPDATE organization SET allow_requester_ai_skip = true")
        .execute(p)
        .await
        .unwrap();
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
        network,
        priority,
    })
}

impl World {
    fn pool(&self) -> &PgPool {
        &self.db.pool
    }

    async fn get(&self, path: &str, who: &str) -> Reply {
        call(&self.app, "GET", path, None, Some(&self.cookies[who])).await
    }

    async fn action(&self, id: Uuid, body: Value, who: &str) -> Reply {
        let path = format!("/api/tickets/{id}/actions");
        call(
            &self.app,
            "POST",
            &path,
            Some(body),
            Some(&self.cookies[who]),
        )
        .await
    }

    /// A draft ticket of `owner` in `category` with its frozen intake.
    async fn insert(&self, category: Uuid, subject: &str) -> Uuid {
        let intake = json!({
            "subject": subject,
            "description": "It jams on page two.",
            "catalog": { "category_id": category, "category_name": "Software", "skip_ai": false,
                         "priority_id": self.priority, "priority_name": "Normal", "answers": [] },
            "screenshot_ids": [],
        });
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject,
                                  description, intake_snapshot)
             VALUES ($1, $2, $3, $4, 'It jams on page two.', $5) RETURNING id",
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
        let status = lifecycle::transition_with(
            &mut tx,
            id,
            By::Account(self.ids["owner"]),
            &submit,
            Some(policy),
        )
        .await
        .unwrap();
        if status == tikkit_api::api::TicketStatus::AiProcessing {
            fixtures::ai_attempt(&mut *tx, id, "processing").await;
        }
        tx.commit().await.unwrap();
        id
    }

    async fn suggest(&self, id: Uuid) {
        let mut tx = self.pool().begin().await.unwrap();
        lifecycle::transition(&mut tx, id, By::System, &Transition::AiSuggestionReady)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE ai_attempts SET state = 'suggestion_ready', explanation = 'The tray is misaligned.',
                    steps = '[\"Open the tray\", \"Push it in until it clicks\"]',
                    finished_at = clock_timestamp()
             WHERE ticket_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn suggested(&self, subject: &str) -> Uuid {
        let id = self.submit(self.software, subject, true).await;
        self.suggest(id).await;
        id
    }

    /// A claimed Desk ticket awaiting the requester's confirmation of proposal 1.
    async fn proposed(&self, subject: &str, due_in: &str) -> Uuid {
        let id = self.submit(self.software, subject, false).await;
        sqlx::query("UPDATE tickets SET team_id = $2, assignee_account_id = $3 WHERE id = $1")
            .bind(id)
            .bind(self.desk)
            .bind(self.ids["op"])
            .execute(self.pool())
            .await
            .unwrap();
        let mut tx = self.pool().begin().await.unwrap();
        let op = By::Account(self.ids["op"]);
        lifecycle::transition(&mut tx, id, op, &Transition::StartWork)
            .await
            .unwrap();
        let propose = Transition::ProposeResolution {
            summary: "Replaced the tray.".into(),
        };
        lifecycle::transition(&mut tx, id, op, &propose)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE tickets SET confirmation_deadline = now() + $2::interval WHERE id = $1",
        )
        .bind(id)
        .bind(due_in)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    /// The submission list for one query (one page of up to 100).
    async fn list(&self, query: &str) -> Vec<Value> {
        let r = self
            .get(
                &format!("/api/admin/submissions?limit=100&{query}"),
                "admin",
            )
            .await;
        assert_eq!(r.status, 200, "{query}: {}", r.body);
        assert_eq!(r.body["next_cursor"], Value::Null);
        r.body["items"].as_array().unwrap().clone()
    }

    async fn subjects(&self, query: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .list(query)
            .await
            .iter()
            .map(|i| i["subject"].as_str().unwrap().to_owned())
            .collect();
        out.sort();
        out
    }

    /// Everything a view could conceivably change, across all tables it reads
    /// or that a handling action would write.
    async fn everything(&self) -> Value {
        sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'tickets', (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.id) FROM tickets t),
                'events', (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.id) FROM ticket_events e),
                'attempts', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM ai_attempts a),
                'messages', (SELECT count(*) FROM ticket_messages),
                'outbox', (SELECT jsonb_agg(to_jsonb(o) ORDER BY o.id) FROM notification_outbox o),
                'notifications', (SELECT count(*) FROM notifications),
                'audit', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM audit_events a),
                'usage', (SELECT count(*) FROM ai_usage))",
        )
        .fetch_one(self.pool())
        .await
        .unwrap()
    }
}

/// The mixed-outcome fixture, by subject.
struct Mixed {
    ids: HashMap<&'static str, Uuid>,
}

async fn mixed(w: &World) -> Mixed {
    let mut ids = HashMap::new();
    // Escalated and human-handled tickets.
    ids.insert("no_ai", w.submit(w.software, "no_ai", false).await);
    ids.insert("exempt", w.submit(w.network, "exempt", true).await);
    let confirmed = w.proposed("confirmed", "1 hour").await;
    let r = w
        .action(
            confirmed,
            json!({ "action": "accept_resolution", "proposal_version": 1 }),
            "owner",
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    ids.insert("confirmed", confirmed);
    let timed_out = w.proposed("timed_out", "-1 minute").await;
    let mut tx = w.pool().begin().await.unwrap();
    let timeout = Transition::ConfirmationTimeout {
        proposal_version: 1,
    };
    lifecycle::transition(&mut tx, timed_out, By::System, &timeout)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    ids.insert("timed_out", timed_out);
    // AI outcomes. The failure first: invalidation stops every processing attempt.
    let failed = w.submit(w.software, "failed", true).await;
    let mut tx = w.pool().begin().await.unwrap();
    assert_eq!(
        ai_attempts::invalidate(&mut tx, Some(w.software), "ai_disabled")
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    ids.insert("failed", failed);
    ids.insert("processing", w.submit(w.software, "processing", true).await);
    ids.insert("awaiting", w.suggested("awaiting").await);
    let solved = w.suggested("solved").await;
    let r = w
        .action(solved, json!({ "action": "ai_solved" }), "owner")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    ids.insert("solved", solved);
    let not_solved = w.suggested("not_solved").await;
    let r = w
        .action(not_solved, json!({ "action": "ai_not_solved" }), "owner")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    ids.insert("not_solved", not_solved);
    let skipped = w.submit(w.software, "skipped", true).await;
    let r = w
        .action(skipped, json!({ "action": "skip_ai" }), "owner")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    ids.insert("skipped", skipped);
    let abandoned = w.suggested("abandoned").await;
    let cancel = json!({ "action": "cancel", "reason": "Found it myself" });
    let r = w.action(abandoned, cancel, "owner").await;
    assert_eq!(r.status, 200, "{}", r.body);
    ids.insert("abandoned", abandoned);
    // Not a submission.
    ids.insert("draft", w.insert(w.software, "draft").await);
    Mixed { ids }
}

#[tokio::test]
async fn admins_find_every_submission_with_its_outcome_and_filters() {
    let Some(w) = world().await else { return };
    let m = mixed(&w).await;

    let items = w.list("").await;
    // (subject, status, resolution, resolution_source, route, ai_outcome)
    let expected = [
        (
            "abandoned",
            "cancelled",
            "cancelled",
            None,
            Some("ai"),
            Some("cancelled"),
        ),
        (
            "skipped",
            "queued",
            "open",
            None,
            Some("ai"),
            Some("skipped"),
        ),
        (
            "not_solved",
            "queued",
            "open",
            None,
            Some("ai"),
            Some("not_solved"),
        ),
        (
            "solved",
            "ai_solved",
            "ai_confirmed",
            Some("ai"),
            Some("ai"),
            Some("solved"),
        ),
        (
            "awaiting",
            "awaiting_ai_decision",
            "open",
            None,
            Some("ai"),
            Some("awaiting_decision"),
        ),
        (
            "processing",
            "ai_processing",
            "open",
            None,
            Some("ai"),
            Some("processing"),
        ),
        ("failed", "queued", "open", None, Some("ai"), Some("failed")),
        (
            "timed_out",
            "closed",
            "confirmation_timeout",
            Some("operator"),
            Some("ai_disabled"),
            None,
        ),
        (
            "confirmed",
            "closed",
            "requester_confirmed",
            Some("operator"),
            Some("ai_disabled"),
            None,
        ),
        (
            "exempt",
            "queued",
            "open",
            None,
            Some("category_exempt"),
            None,
        ),
        ("no_ai", "queued", "open", None, Some("ai_disabled"), None),
    ];
    // Newest first; drafts are not submissions.
    let subjects: Vec<_> = items
        .iter()
        .map(|i| i["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, expected.map(|e| e.0));
    for (item, (subject, status, resolution, source, route, ai)) in items.iter().zip(expected) {
        assert_eq!(item["id"], json!(m.ids[subject]), "{subject}");
        assert_eq!(item["status"], status, "{subject}");
        assert_eq!(item["resolution"], resolution, "{subject}");
        assert_eq!(item["resolution_source"], json!(source), "{subject}");
        assert_eq!(item["route"], json!(route), "{subject}");
        assert_eq!(item["ai_outcome"], json!(ai), "{subject}");
        assert_eq!(item["requester"]["username"], "owner", "{subject}");
    }
    let confirmed = items.iter().find(|i| i["subject"] == "confirmed").unwrap();
    assert_eq!(confirmed["team_name"], "Desk");
    assert_eq!(confirmed["assignee_display_name"], "op");
    assert_eq!(confirmed["closure_reason"], "requester_confirmed");
    let exempt = items.iter().find(|i| i["subject"] == "exempt").unwrap();
    assert_eq!(exempt["category_name"], "Software"); // frozen intake name
    assert_eq!(exempt["category_id"], json!(w.network));

    // Filters, applied in SQL before the limit.
    assert_eq!(w.subjects("resolution_source=ai").await, ["solved"]);
    assert_eq!(
        w.subjects("resolution_source=operator").await,
        ["confirmed", "timed_out"]
    );
    assert_eq!(w.list("resolution_source=none").await.len(), 8);
    assert_eq!(
        w.subjects("ai_outcome=none").await,
        ["confirmed", "exempt", "no_ai", "timed_out"]
    );
    for outcome in [
        "processing",
        "awaiting_decision",
        "solved",
        "not_solved",
        "skipped",
        "failed",
        "cancelled",
    ] {
        assert_eq!(
            w.list(&format!("ai_outcome={outcome}")).await.len(),
            1,
            "{outcome}"
        );
    }
    assert_eq!(w.subjects("ai_outcome=cancelled").await, ["abandoned"]);
    assert_eq!(
        w.subjects("status=queued").await,
        ["exempt", "failed", "no_ai", "not_solved", "skipped"]
    );
    assert_eq!(
        w.subjects(&format!("category_id={}", w.network)).await,
        ["exempt"]
    );
    assert_eq!(
        w.subjects(&format!(
            "category_id={}&status=queued&ai_outcome=failed",
            w.software
        ))
        .await,
        ["failed"]
    );
    sqlx::query("UPDATE tickets SET created_at = '2026-01-15T12:00:00Z' WHERE id = $1")
        .bind(m.ids["no_ai"])
        .execute(w.pool())
        .await
        .unwrap();
    assert_eq!(
        w.subjects("created_from=2026-01-15T00:00:00Z&created_before=2026-01-16T00:00:00Z")
            .await,
        ["no_ai"]
    );
    assert_eq!(
        w.list("created_before=2026-01-15T12:00:00Z").await.len(),
        0,
        "the upper bound is exclusive"
    );
    assert_eq!(w.list("created_from=2026-01-16T00:00:00Z").await.len(), 10);

    // Paging: newest first, no repeats, every submission once.
    let mut seen = vec![];
    let mut cursor = String::new();
    loop {
        let r = w
            .get(&format!("/api/admin/submissions?limit=4{cursor}"), "admin")
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        seen.extend(
            r.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["id"].clone()),
        );
        match r.body["next_cursor"].as_str() {
            Some(next) => cursor = format!("&cursor={next}"),
            None => break,
        }
    }
    let all: Vec<_> = w.list("").await.iter().map(|i| i["id"].clone()).collect();
    assert_eq!(seen, all);

    // Unknown or contradictory filters.
    for query in [
        "status=draft",
        "status=reopened",
        "ai_outcome=abandoned",
        "resolution_source=human",
        "category_id=nope",
        "created_from=yesterday",
        "created_from=2026-02-01T00:00:00Z&created_before=2026-02-01T00:00:00Z",
    ] {
        let r = w
            .get(&format!("/api/admin/submissions?{query}"), "admin")
            .await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (400, "invalid_query"),
            "{query}"
        );
    }
    let r = w.get("/api/admin/submissions?cursor=bogus", "admin").await;
    assert_eq!(r.status, 422);
}

#[tokio::test]
async fn the_ai_solved_detail_shows_intake_suggestion_and_confirmation_read_only() {
    let Some(w) = world().await else { return };
    let m = mixed(&w).await;
    let solved = m.ids["solved"];
    let r = w
        .get(&format!("/api/admin/submissions/{solved}"), "admin")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let d = &r.body;
    assert_eq!(d["summary"]["status"], "ai_solved");
    assert_eq!(d["summary"]["resolution"], "ai_confirmed");
    assert_eq!(d["summary"]["resolution_source"], "ai");
    assert_eq!(d["summary"]["closure_reason"], "requester_confirmed");
    assert_eq!(d["description"], "It jams on page two.");
    assert_eq!(d["intake_snapshot"]["subject"], "solved");
    assert_eq!(d["intake_snapshot"]["catalog"]["priority_name"], "Normal");
    let ai = &d["ai"];
    assert_eq!(ai["outcome"], "solved");
    assert_eq!(ai["state"], "suggestion_ready");
    assert_eq!(ai["decision"], "solved");
    assert_eq!(ai["provider"], "openai");
    assert_eq!(ai["model"], "test-model");
    assert_eq!(ai["language"], "en");
    assert_eq!(ai["outcome_code"], Value::Null);
    assert_eq!(ai["suggestion"]["explanation"], "The tray is misaligned.");
    assert_eq!(
        ai["suggestion"]["steps"],
        json!(["Open the tray", "Push it in until it clicks"])
    );
    // The explicit confirmation event, by the requester.
    let events = d["events"].as_array().unwrap();
    let types: Vec<_> = events
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["submit", "ai_suggestion_ready", "ai_solved"]);
    let confirmation = &events[2];
    assert_eq!(ai["decision_event_id"], confirmation["id"]);
    assert_eq!(ai["decided_at"], confirmation["created_at"]);
    assert_eq!(confirmation["actor"]["username"], "owner");
    assert_eq!(confirmation["payload"]["resolution_source"], "ai");
    assert_eq!(
        confirmation["payload"]["closure_reason"],
        "requester_confirmed"
    );
    assert_eq!(
        events[1]["actor"],
        Value::Null,
        "the worker is a system actor"
    );

    // Read-only: no mutating route, and the record stays final for Admins.
    let before = w.everything().await;
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let path = format!("/api/admin/submissions/{solved}");
        let r = call(
            &w.app,
            method,
            &path,
            Some(json!({})),
            Some(&w.cookies["admin"]),
        )
        .await;
        assert_eq!(r.status, 405, "{method}");
    }
    for body in [
        json!({ "action": "reopen" }),
        json!({ "action": "ai_not_solved" }),
        json!({ "action": "skip_ai" }),
        json!({ "action": "cancel", "reason": "Escalate this" }),
    ] {
        let r = w.action(solved, body.clone(), "admin").await;
        assert_eq!(r.status, 403, "{body}");
    }
    let path = format!("/api/tickets/{solved}/staff-actions");
    let at: String = sqlx::query_scalar(
        "SELECT to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')
         FROM tickets WHERE id = $1",
    )
    .bind(solved)
    .fetch_one(w.pool())
    .await
    .unwrap();
    let reopen = json!({ "action": "reopen", "expected_updated_at": at });
    let r = call(
        &w.app,
        "POST",
        &path,
        Some(reopen),
        Some(&w.cookies["admin"]),
    )
    .await;
    assert_eq!(r.status, 403);
    assert_eq!(w.everything().await, before);
}

#[tokio::test]
async fn unconfirmed_abandoned_and_failed_attempts_are_not_solved() {
    let Some(w) = world().await else { return };
    let m = mixed(&w).await;
    // (subject, outcome, state, decision, resolution, outcome_code, has suggestion)
    let cases = [
        (
            "awaiting",
            "awaiting_decision",
            "suggestion_ready",
            None,
            "open",
            None,
            true,
        ),
        (
            "processing",
            "processing",
            "processing",
            None,
            "open",
            None,
            false,
        ),
        (
            "abandoned",
            "cancelled",
            "suggestion_ready",
            None,
            "cancelled",
            None,
            true,
        ),
        (
            "failed",
            "failed",
            "handed_off",
            None,
            "open",
            Some("ai_disabled"),
            false,
        ),
        (
            "skipped",
            "skipped",
            "handed_off",
            None,
            "open",
            Some("requester_skip"),
            false,
        ),
        (
            "not_solved",
            "not_solved",
            "suggestion_ready",
            Some("not_solved"),
            "open",
            None,
            true,
        ),
    ];
    for (subject, outcome, state, decision, resolution, code, suggestion) in cases {
        let id = m.ids[subject];
        let d = w
            .get(&format!("/api/admin/submissions/{id}"), "admin")
            .await
            .body;
        assert_eq!(d["ai"]["outcome"], outcome, "{subject}");
        assert_eq!(d["ai"]["state"], state, "{subject}");
        assert_eq!(d["ai"]["decision"], json!(decision), "{subject}");
        assert_eq!(d["ai"]["outcome_code"], json!(code), "{subject}");
        assert_eq!(d["ai"]["suggestion"].is_object(), suggestion, "{subject}");
        assert_eq!(d["summary"]["resolution"], resolution, "{subject}");
        assert_eq!(d["summary"]["resolution_source"], Value::Null, "{subject}");
        let types: Vec<_> = d["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["event_type"].as_str().unwrap().to_owned())
            .collect();
        assert!(!types.contains(&"ai_solved".to_owned()), "{subject}");
    }
    // Human outcomes are distinguished too, and have no AI attempt.
    let d = w
        .get(
            &format!("/api/admin/submissions/{}", m.ids["timed_out"]),
            "admin",
        )
        .await
        .body;
    assert_eq!(d["ai"], Value::Null);
    assert_eq!(d["summary"]["resolution"], "confirmation_timeout");
    let last = d["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["event_type"], "confirmation_timeout");
    assert_eq!(last["actor"], Value::Null);
    let d = w
        .get(
            &format!("/api/admin/submissions/{}", m.ids["confirmed"]),
            "admin",
        )
        .await
        .body;
    let types: Vec<_> = d["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "submit",
            "start_work",
            "propose_resolution",
            "accept_resolution"
        ]
    );
    // A draft is not a submission; an unknown ID is the same answer.
    for id in [m.ids["draft"], Uuid::new_v4()] {
        let r = w
            .get(&format!("/api/admin/submissions/{id}"), "admin")
            .await;
        assert_eq!((r.status.as_u16(), r.code()), (404, "not_found"));
    }
    let r = w.get("/api/admin/submissions/not-a-uuid", "admin").await;
    assert_eq!(r.status, 400);
}

#[tokio::test]
async fn a_handed_off_attempt_keeps_its_outcome_when_the_ticket_is_cancelled_later() {
    let Some(w) = world().await else { return };
    let m = mixed(&w).await;
    // Both tickets reached the help desk first; cancelling afterwards is not "abandoned".
    for subject in ["failed", "not_solved"] {
        let cancel = json!({ "action": "cancel", "reason": "No longer needed" });
        let r = w.action(m.ids[subject], cancel, "owner").await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    assert_eq!(w.subjects("ai_outcome=failed").await, ["failed"]);
    assert_eq!(w.subjects("ai_outcome=not_solved").await, ["not_solved"]);
    assert_eq!(w.subjects("ai_outcome=cancelled").await, ["abandoned"]);
    let mut cancelled = w.subjects("status=cancelled").await;
    cancelled.sort();
    assert_eq!(cancelled, ["abandoned", "failed", "not_solved"]);
    for item in w.list("status=cancelled").await {
        assert_eq!(item["resolution"], "cancelled", "{item}");
        assert_eq!(item["resolution_source"], Value::Null, "{item}");
    }
}

#[tokio::test]
async fn viewing_has_no_side_effects_and_returns_no_secrets() {
    let Some(w) = world().await else { return };
    let m = mixed(&w).await;
    // A stored AI credential and SMTP password that must never surface.
    let marker = ["never", "in", "a", "response", "0123456789"].join("-");
    sqlx::query(
        "INSERT INTO ai_credentials (provider, key_ciphertext, key_hint) VALUES ('openai', $1, 'QZQZ')",
    )
    .bind(marker.as_bytes())
    .execute(w.pool())
    .await
    .unwrap();
    let before = w.everything().await;
    let mut bodies = vec![];
    for _ in 0..2 {
        bodies.push(
            w.get("/api/admin/submissions?limit=100", "admin")
                .await
                .body,
        );
        for id in m.ids.values() {
            bodies.push(
                w.get(&format!("/api/admin/submissions/{id}"), "admin")
                    .await
                    .body,
            );
        }
    }
    assert_eq!(w.everything().await, before, "viewing changed stored state");

    let text = bodies.iter().map(Value::to_string).collect::<String>();
    // The last needle is the start of the test ENCRYPTION_KEY (support::test_config).
    for needle in [
        marker.as_str(),
        "QZQZ",
        "ciphertext",
        "key_hint",
        "credential",
        "000102030405060708090a0b0c0d0e0f",
        "\"input\"",
    ] {
        assert!(!text.contains(needle), "response contains {needle}");
    }
}

#[tokio::test]
async fn non_admins_get_neither_the_dataset_nor_counts() {
    let Some(w) = world().await else { return };
    let m = mixed(&w).await;
    let solved = m.ids["solved"];
    let paths = [
        "/api/admin/submissions".to_owned(),
        "/api/admin/submissions?ai_outcome=solved".to_owned(),
        format!("/api/admin/submissions/{solved}"),
        format!("/api/admin/submissions/{}", m.ids["no_ai"]),
        format!("/api/admin/submissions/{}", Uuid::new_v4()),
    ];
    for path in &paths {
        for who in NON_ADMINS {
            let r = w.get(path, who).await;
            assert_eq!(
                (r.status.as_u16(), r.code()),
                (403, "forbidden"),
                "{who} {path}"
            );
            assert!(r.body.get("items").is_none() && r.body.get("summary").is_none());
        }
        assert_eq!(
            call(&w.app, "GET", path, None, None).await.status,
            401,
            "{path}"
        );
    }
    // The requester's own AI-solved record stays readable through their own view.
    let r = w.get(&format!("/api/tickets/{solved}"), "owner").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["view"], "requester");
}
