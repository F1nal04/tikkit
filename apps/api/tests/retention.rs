//! Data retention, anonymization, and personal-data export against real
//! PostgreSQL (docs/retention.md): conservative defaults, expiry boundaries,
//! dry-run counts equal to deletions, restart and concurrency safety, linked
//! screenshots, reports before and after, the audit floor, role denial, and
//! what an export contains.

mod support;

use std::{collections::HashMap, path::PathBuf, time::SystemTime};

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{
    Reply, TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, screenshot_dir, test_app,
};
use tikkit_api::{
    audit::{self, Who},
    lifecycle::{self, By, Policy, Transition},
    metrics::Metrics,
    retention::{self, REMOVED, RetentionCounts, RetentionPolicy},
    screenshots,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

const NON_ADMINS: [&str; 4] = ["owner", "op", "spec", "lead"];
const MICRO: Duration = Duration::microseconds(1);

struct World {
    db: TestDb,
    app: Router,
    ids: HashMap<&'static str, Uuid>,
    cookies: HashMap<&'static str, String>,
    desk: Uuid,
    category: Uuid,
    priority: Uuid,
    dir: PathBuf,
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
    ] {
        ids.insert(name, fixtures::account(p, role, name).await);
    }
    let desk = fixtures::team(p, "Desk", ids["lead"]).await.unwrap();
    for name in ["op", "spec"] {
        fixtures::member(p, desk, ids[name]).await.unwrap();
    }
    let category =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    let priority =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    sqlx::query("UPDATE organization SET allow_reopening = true")
        .execute(p)
        .await
        .unwrap();
    let app = test_app(p);
    let mut cookies = HashMap::new();
    for name in ids.keys() {
        cookies.insert(*name, login(&app, name, PASSWORD).await.session());
    }
    let dir = screenshot_dir(p);
    std::fs::create_dir_all(&dir).unwrap();
    Some(World {
        db,
        app,
        ids,
        cookies,
        desk,
        category,
        priority,
        dir,
    })
}

fn all(days: i64) -> RetentionPolicy {
    RetentionPolicy {
        draft_days: Some(days),
        screenshot_days: Some(days),
        ai_content_days: Some(days),
        ticket_content_days: Some(days),
        audit_days: None,
    }
}

fn only_tickets(days: i64) -> RetentionPolicy {
    RetentionPolicy {
        ticket_content_days: Some(days),
        ..Default::default()
    }
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

    async fn scalar<T>(&self, sql: &str, id: Uuid) -> T
    where
        T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    {
        sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    /// A ticket of `who` with frozen intake (one answer), still a draft.
    async fn insert(&self, who: &str, subject: &str) -> Uuid {
        let intake = json!({
            "subject": subject,
            "description": "It jams on page two.",
            "catalog": { "category_id": self.category, "category_name": "Software", "skip_ai": false,
                         "priority_id": self.priority, "priority_name": "Normal",
                         "answers": [{ "field_id": Uuid::new_v4(), "label": "Device",
                                       "choice_id": null, "value": "Printer 4" }] },
            "screenshot_ids": [],
        });
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject,
                                  description, intake_snapshot)
             VALUES ($1, $2, $3, $4, 'It jams on page two.', $5) RETURNING id",
        )
        .bind(self.ids[who])
        .bind(self.category)
        .bind(self.priority)
        .bind(subject)
        .bind(intake)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn submit(&self, who: &str, subject: &str, ai: bool) -> Uuid {
        let id = self.insert(who, subject).await;
        let mut tx = self.pool().begin().await.unwrap();
        let policy = Policy {
            ai_available: ai,
            ..Policy::default()
        };
        let submit = Transition::Submit {
            requester_skip: false,
        };
        let by = By::Account(self.ids[who]);
        lifecycle::transition_with(&mut tx, id, by, &submit, Some(policy))
            .await
            .unwrap();
        if ai {
            fixtures::ai_attempt(&mut *tx, id, "processing").await;
        }
        tx.commit().await.unwrap();
        id
    }

    /// An open ticket of `owner` in the general queue.
    async fn open(&self, subject: &str) -> Uuid {
        self.submit("owner", subject, false).await
    }

    async fn transition(&self, id: Uuid, who: &str, t: Transition) {
        let mut tx = self.pool().begin().await.unwrap();
        lifecycle::transition(&mut tx, id, By::Account(self.ids[who]), &t)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn message(&self, ticket: Uuid, who: &str, kind: &str, body: &str) -> Uuid {
        let author_kind = if who == "owner" { "requester" } else { "staff" };
        sqlx::query_scalar(
            "INSERT INTO ticket_messages (ticket_id, author_account_id, kind, body, author_kind)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(ticket)
        .bind(self.ids[who])
        .bind(kind)
        .bind(body)
        .bind(author_kind)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// A ticket of `owner` worked by `op`, with a public reply and an internal
    /// note, closed by the requester's confirmation.
    async fn closed(&self, subject: &str) -> Uuid {
        let id = self.open(subject).await;
        sqlx::query("UPDATE tickets SET team_id = $2, assignee_account_id = $3 WHERE id = $1")
            .bind(id)
            .bind(self.desk)
            .bind(self.ids["op"])
            .execute(self.pool())
            .await
            .unwrap();
        self.transition(id, "op", Transition::StartWork).await;
        self.message(id, "op", "public_reply", "Please reseat the tray.")
            .await;
        self.message(id, "op", "internal_note", "User sounded annoyed.")
            .await;
        let propose = Transition::ProposeResolution {
            summary: "Replaced the tray.".into(),
        };
        self.transition(id, "op", propose).await;
        let accept = Transition::AcceptResolution {
            proposal_version: 1,
        };
        self.transition(id, "owner", accept).await;
        id
    }

    async fn cancelled(&self, subject: &str) -> Uuid {
        let id = self.open(subject).await;
        let cancel = Transition::Cancel {
            reason: "Found the cable myself".into(),
        };
        self.transition(id, "owner", cancel).await;
        id
    }

    /// An AI-solved record: suggestion stored, confirmed by the requester.
    async fn ai_solved(&self, subject: &str) -> Uuid {
        let id = self.submit("owner", subject, true).await;
        let mut tx = self.pool().begin().await.unwrap();
        lifecycle::transition(&mut tx, id, By::System, &Transition::AiSuggestionReady)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE ai_attempts SET state = 'suggestion_ready', explanation = 'The tray is misaligned.',
                    steps = '[\"Open the tray\"]', finished_at = clock_timestamp()
             WHERE ticket_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        self.transition(id, "owner", Transition::AiSolved).await;
        id
    }

    /// An attached screenshot of `ticket` with its file on the private volume.
    async fn screenshot(&self, ticket: Uuid, message: Option<Uuid>) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO screenshots (id, uploader_account_id, ticket_id, message_id, filename,
                                      content_type, byte_size, width, height, attached_at)
             VALUES ($1, $2, $3, $4, 'shot.png', 'image/png', 3, 1, 1, now())",
        )
        .bind(id)
        .bind(self.ids["owner"])
        .bind(ticket)
        .bind(message)
        .execute(self.pool())
        .await
        .unwrap();
        std::fs::write(self.dir.join(id.to_string()), b"png").unwrap();
        id
    }

    fn file(&self, id: Uuid) -> bool {
        self.dir.join(id.to_string()).exists()
    }

    async fn draft(&self, who: &str, age: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO ticket_drafts (requester_account_id, intake, updated_at)
             VALUES ($1, '{\"subject\": \"half written\"}', now() - $2::interval) RETURNING id",
        )
        .bind(self.ids[who])
        .bind(age)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn final_at(&self, ticket: Uuid) -> OffsetDateTime {
        self.scalar(
            "SELECT ticket_final_at(t) FROM tickets t WHERE t.id = $1",
            ticket,
        )
        .await
    }

    async fn preview(&self, policy: &RetentionPolicy, as_of: OffsetDateTime) -> RetentionCounts {
        let mut db = self.pool().acquire().await.unwrap();
        retention::preview(&mut db, policy, as_of).await.unwrap()
    }

    /// The database clock.
    async fn now(&self) -> OffsetDateTime {
        sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    /// Moves the whole history of these tickets `by` into the past, so they
    /// reached their status that long ago. Only the owner of a (test) database
    /// can do this: it switches the append-only guard off for the statement.
    async fn age(&self, tickets: &[Uuid], by: &str) {
        let mut tx = self.pool().begin().await.unwrap();
        for sql in [
            "ALTER TABLE ticket_events DISABLE TRIGGER ticket_events_append_only",
            "UPDATE ticket_events SET created_at = created_at - $2::interval WHERE ticket_id = ANY($1)",
            "ALTER TABLE ticket_events ENABLE TRIGGER ticket_events_append_only",
        ] {
            let query = sqlx::query(sql);
            let query = if sql.starts_with("UPDATE") {
                query.bind(tickets).bind(by)
            } else {
                query
            };
            query.execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
    }

    /// Lets a waiting run be claimed again at once.
    async fn skip_backoff(&self) {
        sqlx::query(
            "UPDATE retention_runs SET next_attempt_at = clock_timestamp() WHERE state = 'pending'",
        )
        .execute(self.pool())
        .await
        .unwrap();
    }

    /// Previews the saved policy and starts a run from that preview.
    async fn start(&self, who: &str) -> Reply {
        let saved = self.get("/api/admin/retention", who).await;
        let policy = saved.body["settings"]["policy"].clone();
        let p = self
            .send("POST", "/api/admin/retention/preview", policy, who)
            .await;
        assert_eq!(p.status, 200, "{}", p.body);
        let body = json!({ "confirm": "DELETE", "preview_token": p.body["token"], "as_of": p.body["as_of"] });
        self.send("POST", "/api/admin/retention/runs", body, who)
            .await
    }

    async fn step(&self, batch: i64) -> bool {
        retention::step(self.pool(), &self.dir, &Metrics::default(), batch)
            .await
            .unwrap()
    }

    /// Applies `policy` at `as_of` to completion and returns what the run recorded.
    async fn run(&self, policy: &RetentionPolicy, as_of: OffsetDateTime) -> RetentionCounts {
        let id = retention::start_run(self.pool(), policy, Some(self.ids["admin"]), as_of)
            .await
            .unwrap()
            .expect("no other run pending");
        for _ in 0..200 {
            if self.pending().await == 0 {
                break;
            }
            self.step(2).await;
            self.skip_backoff().await;
        }
        self.run_counts(id).await
    }

    async fn pending(&self) -> i64 {
        self.count("SELECT count(*) FROM retention_runs WHERE state = 'pending'")
            .await
    }

    async fn run_counts(&self, id: Uuid) -> RetentionCounts {
        let (state, counts): (String, sqlx::types::Json<RetentionCounts>) =
            sqlx::query_as("SELECT state, counts FROM retention_runs WHERE id = $1")
                .bind(id)
                .fetch_one(self.pool())
                .await
                .unwrap();
        assert_eq!(state, "completed");
        counts.0
    }

    async fn ticket(&self, id: Uuid) -> Value {
        self.scalar("SELECT to_jsonb(t) FROM tickets t WHERE t.id = $1", id)
            .await
    }

    async fn report(&self) -> Value {
        let now = OffsetDateTime::now_utc();
        let fmt = |t: OffsetDateTime| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap()
                .replace('+', "%2B")
        };
        let path = format!(
            "/api/admin/reports?created_from={}&created_before={}",
            fmt(now - Duration::days(30)),
            fmt(now + Duration::days(1))
        );
        let r = self.get(&path, "admin").await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body
    }

    async fn audit(&self, action: &str) -> Vec<Value> {
        sqlx::query_scalar(
            "SELECT to_jsonb(e) FROM audit_events e WHERE e.event_type = $1 ORDER BY e.id",
        )
        .bind(action)
        .fetch_all(self.pool())
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn nothing_is_deleted_until_an_admin_configures_a_policy() {
    let Some(w) = world().await else { return };
    let closed = w.closed("Printer").await;
    w.draft("owner", "900 days").await;

    let r = w.get("/api/admin/retention", "admin").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["settings"]["policy"],
        json!({ "draft_days": null, "screenshot_days": null, "ai_content_days": null,
                "ticket_content_days": null, "audit_days": null })
    );
    assert_eq!(r.body["settings"]["automatic"], false);
    assert_eq!(
        r.body["limits"],
        json!({ "min_days": 1, "max_days": 3650, "audit_min_days": 30 })
    );
    assert_eq!(r.body["runs"], json!([]));

    // No policy: nothing to preview, nothing to run, and the worker idles.
    let r = w
        .send(
            "POST",
            "/api/admin/retention/preview",
            json!(RetentionPolicy::default()),
            "admin",
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["affected"], json!(RetentionCounts::default()));
    let r = w
        .send(
            "POST",
            "/api/admin/retention/runs",
            json!({ "confirm": "DELETE", "preview_token": "none", "as_of": "2026-01-01T00:00:00Z" }),
            "admin",
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.body["error"]["details"]["fields"]["policy"],
        json!(["not_configured"])
    );
    assert!(!w.step(10).await);
    assert_eq!(w.count("SELECT count(*) FROM retention_runs").await, 0);
    assert_eq!(w.ticket(closed).await["subject"], "Printer");
    assert_eq!(w.count("SELECT count(*) FROM ticket_drafts").await, 1);

    // "Apply automatically" with no period still deletes nothing.
    let body = json!({ "draft_days": null, "screenshot_days": null, "ai_content_days": null,
                       "ticket_content_days": null, "audit_days": null, "automatic": true });
    let r = w.send("PUT", "/api/admin/retention", body, "admin").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!w.step(10).await);
    assert_eq!(w.count("SELECT count(*) FROM retention_runs").await, 0);
}

#[tokio::test]
async fn expiry_is_strictly_older_than_the_period_for_tickets_and_drafts() {
    let Some(w) = world().await else { return };
    let closed = w.closed("Printer").await;
    let open = w.open("Still open").await;
    w.age(&[closed, open], "40 days").await;
    let policy = only_tickets(30);
    let limit = w.final_at(closed).await + Duration::days(30);

    for (as_of, expected) in [(limit - MICRO, 0), (limit, 0), (limit + MICRO, 1)] {
        assert_eq!(w.preview(&policy, as_of).await.ticket_content, expected);
        let done = w.run(&policy, as_of).await;
        assert_eq!(done.ticket_content, expected, "{as_of}");
        let removed = w.ticket(closed).await["content_purged_at"] != Value::Null;
        assert_eq!(removed, expected == 1);
    }
    // An open ticket never expires, however old.
    assert_eq!(
        w.run(&only_tickets(1), w.now().await).await,
        RetentionCounts::default()
    );
    assert_eq!(w.ticket(open).await["subject"], "Still open");
    // The run's clock is never in the future: nothing can be made to expire early.
    let tomorrow = w.now().await + Duration::days(1);
    assert!(
        retention::start_run(w.pool(), &policy, None, tomorrow)
            .await
            .is_err()
    );

    let draft = w.draft("owner", "10 days").await;
    let saved: OffsetDateTime = w
        .scalar("SELECT updated_at FROM ticket_drafts WHERE id = $1", draft)
        .await;
    let policy = RetentionPolicy {
        draft_days: Some(7),
        ..Default::default()
    };
    let limit = saved + Duration::days(7);
    for (as_of, expected) in [(limit - MICRO, 0), (limit, 0), (limit + MICRO, 1)] {
        assert_eq!(w.preview(&policy, as_of).await.drafts, expected);
        assert_eq!(w.run(&policy, as_of).await.drafts, expected);
    }
    assert_eq!(w.count("SELECT count(*) FROM ticket_drafts").await, 0);
}

#[tokio::test]
async fn a_dry_run_counts_exactly_what_the_run_removes_and_reports_still_add_up() {
    let Some(w) = world().await else { return };
    let closed = w.closed("Printer").await;
    let note: Uuid = w
        .scalar(
            "SELECT id FROM ticket_messages WHERE ticket_id = $1 AND kind = 'internal_note'",
            closed,
        )
        .await;
    let cancelled = w.cancelled("Cable").await;
    let solved = w.ai_solved("Tray").await;
    let open = w.open("Still open").await;
    let escalated = w.submit("owner", "Escalated", true).await;
    sqlx::query(
        "UPDATE ai_attempts SET state = 'handed_off', outcome = 'timeout', finished_at = now()
         WHERE ticket_id = $1",
    )
    .bind(escalated)
    .execute(w.pool())
    .await
    .unwrap();
    let expired_shots = [
        w.screenshot(closed, None).await,
        w.screenshot(closed, Some(note)).await,
        w.screenshot(solved, None).await,
    ];
    let live_shot = w.screenshot(open, None).await;
    w.draft("owner", "900 days").await;
    let fresh_draft = w.draft("owner", "1 hour").await;
    w.age(&[closed, cancelled, solved, open, escalated], "40 days")
        .await;
    let before = w.report().await;
    let events = w.count("SELECT count(*) FROM ticket_events").await;
    let statuses = w
        .count("SELECT count(*) FROM tickets WHERE status IN ('closed', 'cancelled', 'ai_solved')")
        .await;

    let policy = RetentionPolicy {
        draft_days: Some(30),
        ..all(1)
    };
    let as_of = w.now().await;
    let expected = w.preview(&policy, as_of).await;
    assert_eq!(
        expected,
        RetentionCounts {
            drafts: 1,
            screenshots: 3,
            ai_content: 1,
            ticket_content: 3,
            audit_events: 0,
        }
    );
    // The HTTP preview uses the same counting.
    let r = w
        .send(
            "POST",
            "/api/admin/retention/preview",
            json!(all(3650)),
            "admin",
        )
        .await;
    assert_eq!(
        r.body["affected"],
        json!(RetentionCounts::default()),
        "{}",
        r.body
    );

    let done = w.run(&policy, as_of).await;
    assert_eq!(done, expected, "dry-run counts equal the deletions");
    assert_eq!(w.preview(&policy, as_of).await, RetentionCounts::default());
    // A second run finds nothing: no double work.
    assert_eq!(w.run(&policy, as_of).await, RetentionCounts::default());

    // Operator-handled, cancelled, and AI-solved records lose their content alike.
    for id in [closed, cancelled, solved] {
        let t = w.ticket(id).await;
        assert_eq!(
            (&t["subject"], &t["description"]),
            (&json!(REMOVED), &json!(REMOVED))
        );
        assert_eq!(t["intake_snapshot"]["subject"], REMOVED);
        assert_eq!(t["intake_snapshot"]["catalog"]["answers"], json!([]));
        assert_eq!(t["intake_snapshot"]["catalog"]["category_name"], "Software");
        assert!(t["content_purged_at"].is_string());
    }
    assert_eq!(w.ticket(solved).await["status"], "ai_solved");
    let bodies: Vec<String> =
        sqlx::query_scalar("SELECT body FROM ticket_messages WHERE ticket_id = $1")
            .bind(closed)
            .fetch_all(w.pool())
            .await
            .unwrap();
    assert_eq!(bodies, [REMOVED, REMOVED]);
    let reason: Value = w
        .scalar(
            "SELECT payload FROM ticket_events WHERE ticket_id = $1 AND event_type = 'cancel'",
            cancelled,
        )
        .await;
    assert_eq!(reason["reason"], REMOVED, "the event stays, its text goes");
    // So does the staff's resolution summary; version and deadline stay.
    let proposal: Value = w
        .scalar(
            "SELECT payload FROM ticket_events WHERE ticket_id = $1 AND event_type = 'propose_resolution'",
            closed,
        )
        .await;
    assert_eq!(proposal["summary"], REMOVED);
    assert_eq!(proposal["proposal_version"], 1);
    assert!(proposal["deadline"].is_string(), "{proposal}");
    // The audit mirror's copy of the reason goes through its own guarded path.
    let mirrored: Value = w
        .scalar(
            "SELECT to_jsonb(e) FROM audit_events e WHERE e.ticket_id = $1 AND e.event_type = 'ticket_cancel'",
            cancelled,
        )
        .await;
    assert_eq!(mirrored["metadata"]["reason"], REMOVED);
    assert_eq!(mirrored["metadata"]["to_status"], "cancelled");
    assert_eq!(mirrored["actor_account_id"], json!(w.ids["owner"]));
    let redacted = w.audit("audit_ticket_text_redacted").await;
    assert_eq!(redacted.len(), 1);
    assert_eq!(redacted[0]["metadata"]["ticket_id"], json!(cancelled));
    assert_eq!(redacted[0]["metadata"]["events"], 1);
    let everything: String = sqlx::query_scalar(
        "SELECT (SELECT jsonb_agg(to_jsonb(t)) FROM tickets t WHERE t.id = ANY($1))::text
             || (SELECT jsonb_agg(to_jsonb(e)) FROM ticket_events e WHERE e.ticket_id = ANY($1))::text
             || (SELECT jsonb_agg(to_jsonb(m)) FROM ticket_messages m WHERE m.ticket_id = ANY($1))::text
             || (SELECT jsonb_agg(to_jsonb(a)) FROM audit_events a WHERE a.ticket_id = ANY($1))::text
             || (SELECT jsonb_agg(to_jsonb(a)) FROM ai_attempts a WHERE a.ticket_id = ANY($1))::text",
    )
    .bind([closed, cancelled, solved])
    .fetch_one(w.pool())
    .await
    .unwrap();
    for text in [
        "Printer",
        "jams",
        "Replaced the tray",
        "reseat",
        "annoyed",
        "Found the cable",
        "misaligned",
        "Printer 4",
    ] {
        assert!(!everything.contains(text), "{text} survived retention");
    }
    let attempt: Value = w
        .scalar(
            "SELECT to_jsonb(a) FROM ai_attempts a WHERE a.ticket_id = $1",
            solved,
        )
        .await;
    assert_eq!(attempt["input"], json!({}));
    assert_eq!(attempt["explanation"], REMOVED);
    assert_eq!(attempt["steps"], json!([]));
    assert_eq!(attempt["state"], "suggestion_ready");

    // Linked objects: screenshots of expired tickets are gone with their files;
    // the live ticket's screenshot, its AI attempt, and the fresh draft stay.
    for id in expired_shots {
        assert!(!w.file(id), "file of an expired ticket");
    }
    assert_eq!(w.count("SELECT count(*) FROM screenshots").await, 1);
    assert!(w.file(live_shot));
    assert_eq!(w.ticket(open).await["subject"], "Still open");
    let kept: Value = w
        .scalar(
            "SELECT to_jsonb(a) FROM ai_attempts a WHERE a.ticket_id = $1",
            escalated,
        )
        .await;
    assert_eq!(kept["content_purged_at"], Value::Null);
    let drafts: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM ticket_drafts")
        .fetch_all(w.pool())
        .await
        .unwrap();
    assert_eq!(drafts, [fresh_draft]);

    // History and statistics are untouched: no event is lost, no status moved.
    assert_eq!(w.count("SELECT count(*) FROM ticket_events").await, events);
    assert_eq!(
        w.count(
            "SELECT count(*) FROM tickets WHERE status IN ('closed', 'cancelled', 'ai_solved')"
        )
        .await,
        statuses
    );
    assert_eq!(w.report().await, before, "reports add up after retention");

    // The views say so, and a removed ticket cannot be reopened.
    let r = w.get(&format!("/api/tickets/{solved}"), "owner").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body["content_removed_at"].is_string());
    assert_eq!(r.body["ai"]["suggestion"], Value::Null);
    assert_eq!(r.body["ai"]["content_removed"], true);
    let r = w
        .get(&format!("/api/admin/submissions/{closed}"), "admin")
        .await;
    assert!(r.body["content_removed_at"].is_string(), "{}", r.body);
    let r = w
        .send(
            "POST",
            &format!("/api/tickets/{closed}/actions"),
            json!({ "action": "reopen" }),
            "owner",
        )
        .await;
    assert_eq!(r.status, 409, "{}", r.body);

    // Audited with who and counts, never content.
    let started = w.audit("retention_run_started").await;
    assert_eq!(started[0]["actor_account_id"], json!(w.ids["admin"]));
    let completed = w.audit("retention_run_completed").await;
    assert_eq!(completed[0]["actor_service"], "retention");
    let recorded: RetentionCounts =
        serde_json::from_value(completed[0]["metadata"]["deleted"].clone()).unwrap();
    assert_eq!(recorded, expected);
    assert_eq!(
        completed[0]["metadata"]["requested_by"],
        json!(w.ids["admin"])
    );
    let text = Value::Array([started, completed].concat()).to_string();
    for secret in ["Printer", "jams", "tray", "Cable", "annoyed"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

#[tokio::test]
async fn a_crash_mid_batch_loses_nothing_and_two_workers_do_no_double_work() {
    let Some(w) = world().await else { return };
    let mut tickets = Vec::new();
    for n in 0..5 {
        tickets.push(w.closed(&format!("Printer {n}")).await);
    }
    w.age(&tickets, "40 days").await;
    let policy = only_tickets(1);
    let as_of = w.now().await;
    assert_eq!(w.preview(&policy, as_of).await.ticket_content, 5);
    let run = retention::start_run(w.pool(), &policy, Some(w.ids["admin"]), as_of)
        .await
        .unwrap()
        .unwrap();
    // One run at a time.
    assert_eq!(
        retention::start_run(w.pool(), &policy, None, as_of)
            .await
            .unwrap(),
        None
    );

    // A worker dies inside a batch: the transaction is gone, and with it both
    // the removals and their counts.
    let mut tx = w.pool().begin().await.unwrap();
    let claimed = retention::claim(&mut tx).await.unwrap().unwrap();
    let advance = retention::process(&mut tx, &claimed, 2, 1_000)
        .await
        .unwrap();
    assert!(!advance.completed);
    // While it holds the run, another worker gets nothing to do.
    let mut other = w.pool().acquire().await.unwrap();
    assert!(retention::claim(&mut other).await.unwrap().is_none());
    drop(other);
    tx.rollback().await.unwrap();
    assert_eq!(w.preview(&policy, as_of).await.ticket_content, 5);
    let counts: Value = w
        .scalar("SELECT counts FROM retention_runs WHERE id = $1", run)
        .await;
    assert_eq!(counts, json!({}));

    // One committed batch, then a restart: the run resumes where it stopped.
    assert!(w.step(2).await);
    assert_eq!(w.preview(&policy, as_of).await.ticket_content, 3);
    let two = async {
        while w.pending().await > 0 {
            w.step(1).await;
        }
    };
    let other = async {
        while w.pending().await > 0 {
            w.step(1).await;
        }
    };
    tokio::join!(two, other);
    assert_eq!(
        w.run_counts(run).await.ticket_content,
        5,
        "each ticket once"
    );
    for id in tickets {
        assert_eq!(w.ticket(id).await["subject"], REMOVED);
    }
    assert_eq!(w.audit("retention_run_completed").await.len(), 1);
}

#[tokio::test]
async fn a_crash_between_row_and_file_deletion_is_recovered_by_the_sweep() {
    let Some(w) = world().await else { return };
    let closed = w.closed("Printer").await;
    let open = w.open("Still open").await;
    let expired = w.screenshot(closed, None).await;
    let live = w.screenshot(open, None).await;
    w.age(&[closed, open], "40 days").await;
    let policy = RetentionPolicy {
        screenshot_days: Some(1),
        ..Default::default()
    };
    retention::start_run(w.pool(), &policy, None, w.now().await)
        .await
        .unwrap()
        .unwrap();
    // The batch commits, then the process dies before touching the volume.
    let mut tx = w.pool().begin().await.unwrap();
    let run = retention::claim(&mut tx).await.unwrap().unwrap();
    let advance = retention::process(&mut tx, &run, 10, 1_000).await.unwrap();
    assert_eq!(advance.files, [expired]);
    tx.commit().await.unwrap();
    assert!(w.file(expired), "row gone, file still there");
    assert_eq!(w.count("SELECT count(*) FROM screenshots").await, 1);
    // Only the ticket's text was not part of this policy.
    assert_eq!(w.ticket(closed).await["subject"], "Printer");

    // The stray-file sweep removes files that have no row once they are old.
    let old = SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    for id in [expired, live] {
        let file = std::fs::File::options()
            .write(true)
            .open(w.dir.join(id.to_string()))
            .unwrap();
        file.set_modified(old).unwrap();
    }
    screenshots::cleanup(w.pool(), &w.dir).await.unwrap();
    assert!(!w.file(expired));
    assert!(w.file(live), "a live ticket's screenshot is never removed");
    while w.pending().await > 0 {
        w.step(10).await;
    }
}

#[tokio::test]
async fn the_immutability_triggers_still_refuse_everything_but_the_retention_path() {
    let Some(w) = world().await else { return };
    let closed = w.closed("Printer").await;
    let open = w.open("Still open").await;
    let solved = w.ai_solved("Tray").await;
    let shot = w.screenshot(open, None).await;
    w.message(open, "owner", "public_reply", "Any news?").await;

    let pool = w.pool();
    let refused = |sql: &'static str, id: Uuid| async move {
        let error = sqlx::query(sql).bind(id).execute(pool).await.unwrap_err();
        let code = error
            .as_database_error()
            .unwrap()
            .code()
            .unwrap()
            .into_owned();
        assert_eq!(code, "23001", "{sql}");
    };
    // The same statements the retention functions run, sent directly or from a block.
    refused("UPDATE tickets SET subject = '—', description = '—', content_purged_at = now() WHERE id = $1", closed).await;
    refused(
        "UPDATE tickets SET content_purged_at = now() WHERE id = $1",
        solved,
    )
    .await;
    refused(
        "UPDATE ticket_messages SET body = '—' WHERE ticket_id = $1",
        closed,
    )
    .await;
    refused(
        "UPDATE ticket_events SET payload = payload - 'reason' WHERE ticket_id = $1",
        closed,
    )
    .await;
    refused("DELETE FROM ticket_events WHERE ticket_id = $1", closed).await;
    refused("DELETE FROM screenshots WHERE ticket_id = $1", open).await;
    refused(
        "UPDATE ai_attempts SET input = '{}', content_purged_at = now() WHERE ticket_id = $1",
        solved,
    )
    .await;
    let error = sqlx::query("DO $$ BEGIN UPDATE ticket_messages SET body = '—'; END $$")
        .execute(w.pool())
        .await
        .unwrap_err();
    assert_eq!(error.as_database_error().unwrap().code().unwrap(), "23001");

    // The routines are not a way around that either. Without a run or an
    // anonymized account they refuse, whatever the ticket: a ticket closed a
    // minute ago with every period unset cannot be erased by calling them.
    let call = |sql: &'static str, ticket: Uuid, run: Option<Uuid>, account: Option<Uuid>| async move {
        sqlx::query(sql)
            .bind(ticket)
            .bind(run)
            .bind(account)
            .fetch_all(pool)
            .await
    };
    let code = |error: sqlx::Error| {
        error
            .as_database_error()
            .unwrap()
            .code()
            .unwrap()
            .into_owned()
    };
    const ROUTINES: [&str; 3] = [
        "SELECT retention_delete_screenshots($1, $2, $3)",
        "SELECT retention_purge_ai($1, $2, $3)",
        "SELECT retention_purge_ticket($1, $2, $3)",
    ];
    let closed_shot = w.screenshot(closed, None).await;
    for sql in ROUTINES {
        for ticket in [closed, solved, open] {
            // Neither authorization, both at once, a run that does not exist,
            // an account that is not anonymized.
            let owner = Some(w.ids["owner"]);
            for (run, account) in [
                (None, None),
                (Some(Uuid::new_v4()), owner),
                (Some(Uuid::new_v4()), None),
                (None, owner),
            ] {
                let error = call(sql, ticket, run, account).await.unwrap_err();
                assert_eq!(code(error), "42501", "{sql} {run:?} {account:?}");
            }
        }
    }
    // A pending run authorizes only the categories its snapshot has a period
    // for, and only tickets that have expired for its clock.
    let drafts_only = RetentionPolicy {
        draft_days: Some(1),
        ..Default::default()
    };
    let run = retention::start_run(pool, &drafts_only, None, w.now().await)
        .await
        .unwrap();
    for sql in ROUTINES {
        let error = call(sql, closed, run, None).await.unwrap_err();
        assert_eq!(code(error), "42501", "no period for it: {sql}");
    }
    while w.pending().await > 0 {
        w.step(10).await;
    }
    // A completed run authorizes nothing any more.
    let error = call(ROUTINES[2], closed, run, None).await.unwrap_err();
    assert_eq!(code(error), "42501");
    let run = retention::start_run(pool, &all(30), None, w.now().await)
        .await
        .unwrap();
    for ticket in [closed, solved, open] {
        // Closed moments ago, or not final at all: inside the keep period.
        let files: Vec<Uuid> = sqlx::query_scalar(ROUTINES[0])
            .bind(ticket)
            .bind(run)
            .bind(None::<Uuid>)
            .fetch_all(pool)
            .await
            .unwrap();
        assert!(files.is_empty());
        let ai: bool = sqlx::query_scalar(ROUTINES[1])
            .bind(ticket)
            .bind(run)
            .bind(None::<Uuid>)
            .fetch_one(pool)
            .await
            .unwrap();
        let rows: i32 = sqlx::query_scalar(ROUTINES[2])
            .bind(ticket)
            .bind(run)
            .bind(None::<Uuid>)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!((ai, rows), (false, 0));
    }
    // The run's snapshot cannot be bent afterwards, and runs are not deleted.
    for sql in [
        "UPDATE retention_runs SET ticket_content_days = 1",
        "UPDATE retention_runs SET as_of = as_of + interval '10 years'",
        "DELETE FROM retention_runs",
    ] {
        let error = sqlx::query(sql).execute(pool).await.unwrap_err();
        assert_eq!(code(error), "23001", "{sql}");
    }
    let error = sqlx::query(
        "INSERT INTO retention_runs (as_of, ticket_content_days) VALUES (now() + interval '1 year', 1)",
    )
    .execute(pool)
    .await
    .unwrap_err();
    assert_eq!(code(error), "23514");
    // The audit path refuses a ticket whose content is still stored.
    let error = sqlx::query("SELECT audit_redact_ticket_text($1)")
        .bind(closed)
        .execute(pool)
        .await
        .unwrap_err();
    assert_eq!(code(error), "42501");
    assert!(w.file(closed_shot));
    assert_eq!(w.ticket(closed).await["subject"], "Printer");
    assert_eq!(w.ticket(solved).await["subject"], "Tray");
    assert_eq!(w.count("SELECT count(*) FROM screenshots").await, 2);
    assert!(w.file(shot));
    assert_eq!(w.ticket(open).await["subject"], "Still open");
    assert_eq!(
        w.count("SELECT count(*) FROM ticket_messages WHERE body = 'Any news?'")
            .await,
        1
    );
}

#[tokio::test]
async fn audit_retention_honours_the_thirty_day_floor() {
    let Some(w) = world().await else { return };
    for (action, age) in [("old_event", "40 days"), ("recent_event", "29 days")] {
        sqlx::query(
            "INSERT INTO audit_events (event_type, actor_service, created_at)
             VALUES ($1, 'system', now() - $2::interval)",
        )
        .bind(action)
        .bind(age)
        .execute(w.pool())
        .await
        .unwrap();
    }
    let body = |audit: i64| {
        json!({ "draft_days": null, "screenshot_days": null, "ai_content_days": null,
                "ticket_content_days": null, "audit_days": audit, "automatic": false })
    };
    let r = w
        .send("PUT", "/api/admin/retention", body(29), "admin")
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.body["error"]["details"]["fields"]["audit_days"],
        json!(["out_of_range"])
    );
    let r = w
        .send(
            "POST",
            "/api/admin/retention/preview",
            json!({ "draft_days": null,
            "screenshot_days": null, "ai_content_days": null, "ticket_content_days": null,
            "audit_days": 7 }),
            "admin",
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let error = sqlx::query("UPDATE retention_settings SET audit_days = 29")
        .execute(w.pool())
        .await
        .unwrap_err();
    assert_eq!(error.as_database_error().unwrap().code().unwrap(), "23514");
    // The database path refuses a younger cutoff whoever asks.
    let mut tx = w.pool().begin().await.unwrap();
    let young = OffsetDateTime::now_utc() - Duration::days(29);
    let error = audit::purge_before(&mut tx, young, Who::Service(audit::RETENTION))
        .await
        .unwrap_err();
    assert_eq!(error.as_database_error().unwrap().code().unwrap(), "23514");
    drop(tx);

    let r = w
        .send("PUT", "/api/admin/retention", body(30), "admin")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let policy = RetentionPolicy {
        audit_days: Some(30),
        ..Default::default()
    };
    let now = OffsetDateTime::now_utc();
    assert_eq!(w.preview(&policy, now).await.audit_events, 1);
    let r = w.start("admin").await;
    assert_eq!(r.status, 202, "{}", r.body);
    while w.pending().await > 0 {
        w.step(10).await;
    }
    assert_eq!(w.audit("old_event").await.len(), 0);
    assert_eq!(w.audit("recent_event").await.len(), 1);
    let purged = w.audit("audit_retention_purged").await;
    assert_eq!(purged.len(), 1);
    assert_eq!(purged[0]["metadata"]["deleted"], 1);
    assert_eq!(purged[0]["actor_account_id"], json!(w.ids["admin"]));
    let r = w.get("/api/admin/retention", "admin").await;
    assert_eq!(r.body["runs"][0]["state"], "completed", "{}", r.body);
    assert_eq!(r.body["runs"][0]["deleted"]["audit_events"], 1);
    assert_eq!(r.body["runs"][0]["requested_by_username"], "admin");
}

#[tokio::test]
async fn settings_are_validated_confirmed_and_audited() {
    let Some(w) = world().await else { return };
    let body = |days: Value, automatic: bool, confirm: Value| {
        json!({ "draft_days": days, "screenshot_days": null, "ai_content_days": null,
                "ticket_content_days": 365, "audit_days": null, "automatic": automatic,
                "confirm": confirm })
    };
    for bad in [json!(0), json!(3651), json!(-5)] {
        let r = w
            .send(
                "PUT",
                "/api/admin/retention",
                body(bad, false, Value::Null),
                "admin",
            )
            .await;
        assert_eq!(r.status, 422, "{}", r.body);
        assert_eq!(
            r.body["error"]["details"]["fields"]["draft_days"],
            json!(["out_of_range"])
        );
    }
    // A missing period is an error, not "keep".
    let r = w
        .send(
            "PUT",
            "/api/admin/retention",
            json!({ "automatic": false }),
            "admin",
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);

    // Automatic deletion needs the typed confirmation; a manual policy does not.
    for confirm in [Value::Null, json!("delete"), json!("yes")] {
        let r = w
            .send(
                "PUT",
                "/api/admin/retention",
                body(json!(30), true, confirm),
                "admin",
            )
            .await;
        assert_eq!(r.status, 422, "{}", r.body);
        assert_eq!(
            r.body["error"]["details"]["fields"]["confirm"],
            json!(["invalid"])
        );
    }
    let r = w
        .send(
            "PUT",
            "/api/admin/retention",
            body(json!(30), false, Value::Null),
            "admin",
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["policy"]["draft_days"], 30);
    assert_eq!(r.body["version"], 2);

    // ...and the word alone is not enough: it must confirm a preview of
    // exactly these periods.
    let automatic = |token: Value| {
        let mut b = body(json!(30), true, json!("DELETE"));
        b["preview_token"] = token;
        b
    };
    let periods = |draft: i64| {
        json!({ "draft_days": draft, "screenshot_days": null, "ai_content_days": null,
                "ticket_content_days": 365, "audit_days": null })
    };
    let preview = |days: i64| {
        w.send(
            "POST",
            "/api/admin/retention/preview",
            periods(days),
            "admin",
        )
    };
    let other = preview(31).await;
    for token in [Value::Null, json!("0000"), other.body["token"].clone()] {
        let r = w
            .send("PUT", "/api/admin/retention", automatic(token), "admin")
            .await;
        assert_eq!(r.status, 409, "{}", r.body);
        assert_eq!(r.code(), "retention_preview_stale");
    }
    let reviewed = preview(30).await;
    let r = w
        .send(
            "PUT",
            "/api/admin/retention",
            automatic(reviewed.body["token"].clone()),
            "admin",
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["automatic"], true);
    let events = w.audit("retention_settings_updated").await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["actor_account_id"], json!(w.ids["admin"]));
    assert_eq!(events[0]["resource_type"], "retention");
    assert_eq!(
        events[0]["metadata"]["from"]["policy"]["draft_days"],
        Value::Null
    );
    assert_eq!(events[0]["metadata"]["to"]["policy"]["draft_days"], 30);
    assert_eq!(events[1]["metadata"]["to"]["automatic"], true);

    // Starting a run needs the confirmation too, and only one runs at a time.
    let current = preview(30).await;
    let run = |confirm: &str, token: Value, as_of: Value| {
        let body = json!({ "confirm": confirm, "preview_token": token, "as_of": as_of });
        w.send("POST", "/api/admin/retention/runs", body, "admin")
    };
    let (token, as_of) = (current.body["token"].clone(), current.body["as_of"].clone());
    for confirm in ["", "delete", "DELETE "] {
        let r = run(confirm, token.clone(), as_of.clone()).await;
        assert_eq!(r.status, 422, "{}", r.body);
    }
    // A preview of other periods, a made-up clock, or an old preview starts nothing.
    let format = |t: OffsetDateTime| {
        json!(
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap()
        )
    };
    let now = w.now().await;
    for (token, as_of) in [
        (other.body["token"].clone(), other.body["as_of"].clone()),
        (
            reviewed.body["token"].clone(),
            reviewed.body["as_of"].clone(),
        ),
        (token.clone(), format(now + Duration::days(400))),
        (token.clone(), format(now - Duration::hours(2))),
    ] {
        let r = run("DELETE", token, as_of).await;
        assert_eq!(r.status, 409, "{}", r.body);
        assert_eq!(r.code(), "retention_preview_stale");
    }
    assert_eq!(w.count("SELECT count(*) FROM retention_runs").await, 0);
    let r = run("DELETE", token.clone(), as_of.clone()).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body["state"], "pending");
    assert_eq!(r.body["policy"]["ticket_content_days"], 365);
    assert_eq!(r.body["as_of"], as_of, "the run's clock is the preview's");
    let r = run("DELETE", token, as_of).await;
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.code(), "conflict");
}

/// The stale tab: Admin A reviews a 365-day policy, Admin B saves a 1-day
/// policy, A confirms in the old tab. Nothing A did not review may run.
#[tokio::test]
async fn a_confirmation_never_applies_a_policy_nobody_previewed() {
    let Some(w) = world().await else { return };
    let second = fixtures::account(w.pool(), Role::Admin, "admin2").await;
    let b = login(&w.app, "admin2", PASSWORD).await.session();
    let closed = w.closed("Printer").await;
    w.age(&[closed], "40 days").await;
    let app = &w.app;
    let save = |days: i64, cookie: String| async move {
        let body = json!({ "draft_days": null, "screenshot_days": null, "ai_content_days": null,
                           "ticket_content_days": days, "audit_days": null, "automatic": false });
        let r = call(
            app,
            "PUT",
            "/api/admin/retention",
            Some(body),
            Some(&cookie),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
    };
    let a = w.cookies["admin"].clone();
    save(365, a.clone()).await;

    // A previews the saved 365 days: nothing would go.
    let saved = w.get("/api/admin/retention", "admin").await;
    let policy = saved.body["settings"]["policy"].clone();
    let seen = w
        .send(
            "POST",
            "/api/admin/retention/preview",
            policy.clone(),
            "admin",
        )
        .await;
    assert_eq!(seen.body["affected"]["ticket_content"], 0, "{}", seen.body);

    // B shortens the period to one day.
    save(1, b.clone()).await;

    // A types DELETE in the old tab.
    let confirm = json!({ "confirm": "DELETE", "preview_token": seen.body["token"], "as_of": seen.body["as_of"] });
    let r = w
        .send(
            "POST",
            "/api/admin/retention/runs",
            confirm.clone(),
            "admin",
        )
        .await;
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.code(), "retention_preview_stale");
    assert_eq!(w.count("SELECT count(*) FROM retention_runs").await, 0);
    assert!(!w.step(10).await);
    assert_eq!(w.ticket(closed).await["subject"], "Printer");

    // The same for switching automatic deletion on from the old tab: A's
    // preview was made under settings that no longer exist.
    let mut automatic = policy.clone();
    automatic["automatic"] = json!(true);
    automatic["confirm"] = json!("DELETE");
    automatic["preview_token"] = seen.body["token"].clone();
    let r = w
        .send("PUT", "/api/admin/retention", automatic, "admin")
        .await;
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.code(), "retention_preview_stale");
    let now = w.get("/api/admin/retention", "admin").await;
    assert_eq!(now.body["settings"]["policy"]["ticket_content_days"], 1);
    assert_eq!(now.body["settings"]["automatic"], false);

    // Even if B saves the old periods again, A's preview stays stale: it is
    // bound to the settings version, not only to the numbers.
    save(365, b.clone()).await;
    let r = w
        .send("POST", "/api/admin/retention/runs", confirm, "admin")
        .await;
    assert_eq!(r.status, 409, "{}", r.body);
    // After a new preview of what is stored, A's confirmation works.
    save(1, b).await;
    let r = w.start("admin").await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body["policy"]["ticket_content_days"], 1);
    while w.pending().await > 0 {
        w.step(10).await;
    }
    assert_eq!(w.ticket(closed).await["subject"], REMOVED);
    let _ = second;
}

#[tokio::test]
async fn the_worker_applies_an_automatic_policy_once_a_day() {
    let Some(w) = world().await else { return };
    w.draft("owner", "40 days").await;
    w.draft("owner", "1 day").await;
    sqlx::query("UPDATE retention_settings SET draft_days = 30, automatic = true")
        .execute(w.pool())
        .await
        .unwrap();
    let state = support::test_state(w.pool());
    let step = || retention::step(w.pool(), &w.dir, &state.metrics, 10);
    assert!(step().await.unwrap(), "schedules a run");
    while w.pending().await > 0 {
        assert!(step().await.unwrap());
    }
    assert!(!step().await.unwrap(), "no second run within a day");
    assert_eq!(w.count("SELECT count(*) FROM ticket_drafts").await, 1);
    assert_eq!(w.count("SELECT count(*) FROM retention_runs").await, 1);
    let started = w.audit("retention_run_started").await;
    assert_eq!(started[0]["actor_service"], "retention");
    assert!(
        started[0]["request_id"].is_string(),
        "the job has a correlation ID"
    );
    assert_eq!(
        w.audit("retention_run_completed").await[0]["request_id"],
        started[0]["request_id"]
    );

    // The worker reports like the others.
    let text = support::metrics_text(&state).await;
    assert!(
        text.contains("tikkit_worker_pending_jobs{worker=\"retention\"} 0\n"),
        "{text}"
    );
    assert!(
        !text.contains("tikkit_worker_last_success_timestamp_seconds{worker=\"retention\"} 0\n")
    );
    assert!(text.contains("tikkit_worker_failures_total{worker=\"retention\"} 0\n"));
    // A day later (the run's row is immutable for everyone but the test
    // database's owner).
    for sql in [
        "ALTER TABLE retention_runs DISABLE TRIGGER retention_runs_guard",
        "UPDATE retention_runs SET created_at = now() - interval '25 hours'",
        "ALTER TABLE retention_runs ENABLE TRIGGER retention_runs_guard",
    ] {
        sqlx::query(sql).execute(w.pool()).await.unwrap();
    }
    assert!(step().await.unwrap(), "the next day's run");
    let text = support::metrics_text(&state).await;
    assert!(
        text.contains("tikkit_worker_pending_jobs{worker=\"retention\"} 1\n"),
        "{text}"
    );
}

#[tokio::test]
async fn every_non_admin_role_is_denied_everything() {
    let Some(w) = world().await else { return };
    let id = w.ids["owner"];
    let policy = json!(RetentionPolicy::default());
    let confirm = json!({ "username": "owner" });
    let requests = [
        ("GET", "/api/admin/retention".to_owned(), None),
        (
            "PUT",
            "/api/admin/retention".to_owned(),
            Some(json!({ "draft_days": null,
            "screenshot_days": null, "ai_content_days": null, "ticket_content_days": null,
            "audit_days": null, "automatic": false })),
        ),
        (
            "POST",
            "/api/admin/retention/preview".to_owned(),
            Some(policy),
        ),
        (
            "POST",
            "/api/admin/retention/runs".to_owned(),
            Some(
                json!({ "confirm": "DELETE", "preview_token": "x", "as_of": "2026-01-01T00:00:00Z" }),
            ),
        ),
        (
            "GET",
            "/api/admin/personal-data?username=owner".to_owned(),
            None,
        ),
        (
            "POST",
            format!("/api/admin/accounts/{id}/personal-data/export"),
            Some(confirm.clone()),
        ),
        (
            "POST",
            format!("/api/admin/accounts/{id}/anonymize"),
            Some(confirm),
        ),
    ];
    for (method, path, body) in &requests {
        for who in NON_ADMINS {
            let r = call(&w.app, method, path, body.clone(), Some(&w.cookies[who])).await;
            assert_eq!(r.status, 403, "{who} {method} {path}: {}", r.body);
            assert_eq!(r.code(), "forbidden");
            assert_eq!(r.body.as_object().unwrap().len(), 2, "no data: {}", r.body);
        }
        let r = call(&w.app, method, path, body.clone(), None).await;
        assert_eq!(r.status, 401, "{method} {path}");
    }
    assert_eq!(w.count("SELECT count(*) FROM retention_runs").await, 0);
    assert_eq!(
        w.count(
            "SELECT count(*) FROM audit_events WHERE event_type LIKE 'retention%'
                 OR event_type IN ('personal_data_exported', 'account_anonymized')"
        )
        .await,
        0
    );
    assert_eq!(
        w.count("SELECT count(*) FROM accounts WHERE anonymized_at IS NOT NULL")
            .await,
        0
    );
}

#[tokio::test]
async fn an_export_holds_the_accounts_own_data_and_nothing_else() {
    let Some(w) = world().await else { return };
    let closed = w.closed("Printer").await;
    w.message(closed, "owner", "public_reply", "Thanks, it works.")
        .await;
    let cancelled = w.cancelled("Cable").await;
    let solved = w.ai_solved("Tray").await;
    w.screenshot(closed, None).await;
    w.draft("owner", "1 hour").await;
    // Another requester, sharing the email address, and a ticket of theirs.
    let twin = fixtures::account(w.pool(), Role::User, "twin").await;
    sqlx::query(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description)
         VALUES ($1, $2, $3, 'Twin secret subject', 'Twin secret description')",
    )
    .bind(twin)
    .bind(w.category)
    .bind(w.priority)
    .execute(w.pool())
    .await
    .unwrap();
    let provider_key = ["sk", "live", &"0123456789abcdef".repeat(3)].join("-");
    sqlx::query(
        "INSERT INTO ai_credentials (provider, key_ciphertext, key_hint, set_by)
         VALUES ('openai', convert_to($1, 'UTF8'), 'cdef', $2)",
    )
    .bind(&provider_key)
    .bind(w.ids["admin"])
    .execute(w.pool())
    .await
    .unwrap();

    let owner = w.ids["owner"];
    let path = format!("/api/admin/accounts/{owner}/personal-data/export");
    // The typed username must be this account's: not another one's, not an email.
    for wrong in ["twin", "shared@example.com", ""] {
        let r = w
            .send("POST", &path, json!({ "username": wrong }), "admin")
            .await;
        assert_eq!(r.status, 422, "{wrong}: {}", r.body);
        assert_eq!(
            r.body["error"]["details"]["fields"]["username"],
            json!(["mismatch"])
        );
    }
    let missing = format!(
        "/api/admin/accounts/{}/personal-data/export",
        Uuid::new_v4()
    );
    let r = w
        .send("POST", &missing, json!({ "username": "owner" }), "admin")
        .await;
    assert_eq!(r.status, 404);
    assert_eq!(w.audit("personal_data_exported").await.len(), 0);

    let r = w
        .send("POST", &path, json!({ "username": "OWNER" }), "admin")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let doc = &r.body;
    assert_eq!(doc["format"], "tikkit.personal_data.v1");
    assert_eq!(doc["account"]["id"], json!(owner));
    assert_eq!(doc["account"]["username"], "owner");
    assert_eq!(doc["account"]["email"], "shared@example.com");
    assert_eq!(doc["truncated"], json!([]));
    let subjects: Vec<&str> = doc["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, ["Printer", "Cable", "Tray"]);
    assert_eq!(doc["tickets"][0]["answers"][0]["value"], "Printer 4");
    let conversation = doc["conversation"].as_array().unwrap();
    let said: Vec<(&str, &str)> = conversation
        .iter()
        .map(|m| (m["author"].as_str().unwrap(), m["body"].as_str().unwrap()))
        .collect();
    assert_eq!(
        said,
        [
            ("staff", "Please reseat the tray."),
            ("account", "Thanks, it works.")
        ]
    );
    assert_eq!(
        doc["ai_suggestions"][0]["explanation"],
        "The tray is misaligned."
    );
    assert_eq!(doc["screenshots"].as_array().unwrap().len(), 1);
    assert_eq!(doc["drafts"].as_array().unwrap().len(), 1);
    assert_eq!(doc["sessions"].as_array().unwrap().len(), 1);
    let cancel = doc["ticket_actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_type"] == "cancel")
        .unwrap();
    assert_eq!(cancel["reason"], "Found the cable myself");
    assert!(
        doc["audit_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "login_succeeded")
    );

    // Not in it: internal notes, staff identities, other accounts, hashes, keys.
    let text = doc.to_string();
    for absent in [
        "User sounded annoyed",
        "internal_note",
        "Twin secret",
        "\"op\"",
        &w.ids["op"].to_string(),
        fixtures::PASSWORD_HASH,
        "password",
        "token",
        &provider_key,
        "key_ciphertext",
    ] {
        assert!(!text.contains(absent), "{absent} must not be exported");
    }
    let _ = (cancelled, solved);

    // A staff account's export names the tickets it worked on, without their text.
    let op = w.ids["op"];
    let path = format!("/api/admin/accounts/{op}/personal-data/export");
    let r = w
        .send("POST", &path, json!({ "username": "op" }), "admin")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["staff_messages"].as_array().unwrap().len(), 2);
    assert_eq!(r.body["tickets"], json!([]));
    let text = r.body.to_string();
    assert!(
        !text.contains("Please reseat") && !text.contains("annoyed") && !text.contains("Printer")
    );

    let events = w.audit("personal_data_exported").await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["actor_account_id"], json!(w.ids["admin"]));
    assert_eq!(events[0]["subject_account_id"], json!(owner));
    assert_eq!(events[0]["metadata"]["rows"]["tickets"], 3);
    assert!(!events[0].to_string().contains("Printer"));
}

#[tokio::test]
async fn anonymization_removes_the_identity_and_keeps_relationships_and_reports() {
    let Some(w) = world().await else { return };
    let closed = w.closed("Printer").await;
    let solved = w.ai_solved("Tray").await;
    let open = w.open("Still open").await;
    let shot = w.screenshot(closed, None).await;
    w.draft("owner", "1 hour").await;
    let twin = fixtures::account(w.pool(), Role::User, "twin").await;
    let before = w.report().await;
    let owner = w.ids["owner"];
    let path = format!("/api/admin/accounts/{owner}/anonymize");

    let r = w
        .get("/api/admin/personal-data?username=Owner", "admin")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["account_id"], json!(owner));
    assert_eq!(
        (&r.body["tickets"], &r.body["open_tickets"]),
        (&json!(3), &json!(1))
    );
    assert_eq!(r.body["final_tickets_with_content"], 2);
    assert_eq!(r.body["drafts"], 1);
    let r = w
        .get(
            "/api/admin/personal-data?username=shared@example.com",
            "admin",
        )
        .await;
    assert_eq!(r.status, 404, "never found by email");

    // Only a deactivated account, and only with its own username typed.
    let r = w
        .send("POST", &path, json!({ "username": "owner" }), "admin")
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.body["error"]["details"]["fields"]["account"],
        json!(["active"])
    );
    // The last active Admin can be neither deactivated nor, therefore, anonymized.
    let admin = w.ids["admin"];
    let r = w
        .send(
            "POST",
            &format!("/api/accounts/{admin}/deactivate"),
            json!({}),
            "admin",
        )
        .await;
    assert_eq!(r.status, 409, "{}", r.body);
    let r = w
        .send(
            "POST",
            &format!("/api/admin/accounts/{admin}/anonymize"),
            json!({ "username": "admin" }),
            "admin",
        )
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    let r = w
        .send(
            "POST",
            &format!("/api/accounts/{owner}/deactivate"),
            json!({}),
            "admin",
        )
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    let r = w
        .send("POST", &path, json!({ "username": "twin" }), "admin")
        .await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.body["error"]["details"]["fields"]["username"],
        json!(["mismatch"])
    );

    let body = json!({ "username": "owner", "purge_ticket_content": true });
    let r = w.send("POST", &path, body.clone(), "admin").await;
    assert_eq!(r.status, 200, "{}", r.body);
    let placeholder = format!("anonymized-{}", owner.simple());
    assert_eq!(r.body["username"], json!(placeholder));
    assert_eq!(r.body["drafts"], 1);
    assert_eq!(
        r.body["ticket_content"],
        json!({ "drafts": 0, "screenshots": 1, "ai_content": 1, "ticket_content": 2, "audit_events": 0 })
    );
    assert!(r.body["audit_events"].as_i64().unwrap() > 0);

    let account: Value = w
        .scalar("SELECT to_jsonb(a) FROM accounts a WHERE a.id = $1", owner)
        .await;
    assert_eq!(account["username"], json!(placeholder));
    assert_eq!(account["display_name"], json!(placeholder));
    assert_eq!(account["email"], "");
    assert_eq!(account["password_hash"], "!");
    assert_eq!(account["is_active"], false);
    assert!(account["anonymized_at"].is_string());
    assert_eq!(account["role"], "user", "the role stays for statistics");
    for (table, column) in [
        ("sessions", "account_id"),
        ("ticket_drafts", "requester_account_id"),
        ("password_resets", "account_id"),
    ] {
        let left: i64 = w
            .scalar(
                &format!("SELECT count(*) FROM {table} WHERE {column} = $1"),
                owner,
            )
            .await;
        assert_eq!(left, 0, "{table}");
    }
    // The audit log keeps the account ID and loses the name and the address.
    let named: i64 = w
        .scalar(
            "SELECT count(*) FROM audit_events
             WHERE (actor_account_id = $1 AND actor_username = 'owner')
                OR (subject_account_id = $1 AND subject_username = 'owner')
                OR ((actor_account_id = $1 OR subject_account_id = $1) AND metadata ? 'ip')",
            owner,
        )
        .await;
    assert_eq!(named, 0);
    let still: i64 = w
        .scalar(
            "SELECT count(*) FROM audit_events WHERE actor_account_id = $1",
            owner,
        )
        .await;
    assert!(still > 0);
    let events = w.audit("account_anonymized").await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["actor_account_id"], json!(admin));
    assert_eq!(events[0]["subject_account_id"], json!(owner));
    assert!(
        !events[0].to_string().contains("\"owner\""),
        "{}",
        events[0]
    );
    assert_eq!(w.audit("audit_account_anonymized").await.len(), 1);

    // Relationships and statistics stay: every ticket is still this account's,
    // open work continues, and the report is the same.
    let mine: i64 = w
        .scalar(
            "SELECT count(*) FROM tickets WHERE requester_account_id = $1",
            owner,
        )
        .await;
    assert_eq!(mine, 3);
    assert_eq!(w.ticket(open).await["subject"], "Still open");
    assert_eq!(w.ticket(closed).await["subject"], REMOVED);
    assert_eq!(w.ticket(solved).await["subject"], REMOVED);
    assert!(!w.file(shot));
    assert_eq!(
        w.report().await,
        before,
        "reports add up after anonymization"
    );
    let r = w
        .get(&format!("/api/admin/submissions/{open}"), "admin")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.to_string().contains("\"owner\""), "{}", r.body);

    // The account sharing the email address is a different account: untouched.
    let other: Value = w
        .scalar("SELECT to_jsonb(a) FROM accounts a WHERE a.id = $1", twin)
        .await;
    assert_eq!(
        (&other["username"], &other["email"]),
        (&json!("twin"), &json!("shared@example.com"))
    );
    assert_eq!(other["anonymized_at"], Value::Null);

    // Final: no login, no reactivation, no second anonymization.
    let r = login(&w.app, "owner", PASSWORD).await;
    assert_eq!(r.status, 401);
    let r = login(&w.app, &placeholder, PASSWORD).await;
    assert_eq!(r.status, 401);
    let r = w
        .send(
            "POST",
            &format!("/api/accounts/{owner}/reactivate"),
            json!({}),
            "admin",
        )
        .await;
    assert_eq!(r.status, 409, "{}", r.body);
    let again = json!({ "username": placeholder });
    let r = w.send("POST", &path, again, "admin").await;
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.body["error"]["details"]["fields"]["account"],
        json!(["already_anonymized"])
    );
    let r = w.get("/api/accounts?limit=100", "admin").await;
    let listed = r.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == json!(owner))
        .unwrap()
        .clone();
    assert_eq!(listed["anonymized"], true);
}
