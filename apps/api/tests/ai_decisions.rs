//! Requester decisions on the AI suggestion against real PostgreSQL
//! (docs/ai-decisions.md): solved, still need help, and skip; idempotent
//! retries and concurrent opposite decisions; the finality of AI-solved
//! records for every role and job; their absence from every operator path;
//! and what Admins can inspect and count.

mod support;

use std::collections::HashMap;

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{
    Reply, TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app, test_state,
};
use tikkit_api::{
    ai_attempts,
    api::ApiError,
    auth::Settings,
    autoclose,
    lifecycle::{self, By, Policy, Transition},
    notifications,
};
use uuid::Uuid;

// --- Fixtures ------------------------------------------------------------------

/// Everyone but the ticket's owner, by fixture name.
const OTHERS: [&str; 6] = ["other", "op", "lead", "spec", "free", "admin"];
/// Staff without the Admin role: the operator paths.
const OPERATORS: [&str; 4] = ["op", "lead", "spec", "free"];

struct World {
    db: TestDb,
    app: Router,
    ids: HashMap<&'static str, Uuid>,
    cookies: HashMap<&'static str, String>,
    desk: Uuid,
    category: Uuid,
    priority: Uuid,
    urgent: Uuid,
}

/// `owner` and `other` are Users; `op`, `spec`, and `lead` are the Desk team;
/// `free` is an operator without a team; `staffer` is a Desk operator who also
/// submits tickets.
async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    let mut ids = HashMap::from([("admin", fixtures::bootstrap_admin(p).await)]);
    for (name, role) in [
        ("owner", Role::User),
        ("other", Role::User),
        ("op", Role::Operator),
        ("spec", Role::Specialist),
        ("lead", Role::TeamLead),
        ("free", Role::Operator),
        ("staffer", Role::Operator),
    ] {
        ids.insert(name, fixtures::account(p, role, name).await);
    }
    let desk = fixtures::team(p, "Desk", ids["lead"]).await.unwrap();
    for name in ["op", "spec", "staffer"] {
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
    let urgent = sqlx::query_scalar(
        "INSERT INTO priorities (name, sort_order) VALUES ('Urgent', 5) RETURNING id",
    )
    .fetch_one(p)
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
        category,
        priority,
        urgent,
    })
}

impl World {
    fn pool(&self) -> &PgPool {
        &self.db.pool
    }

    async fn get(&self, path: &str, who: &str) -> Reply {
        call(&self.app, "GET", path, None, Some(&self.cookies[who])).await
    }

    async fn post(&self, path: &str, body: Value, who: &str) -> Reply {
        call(
            &self.app,
            "POST",
            path,
            Some(body),
            Some(&self.cookies[who]),
        )
        .await
    }

    async fn action(&self, id: Uuid, action: &str, who: &str) -> Reply {
        self.post(
            &format!("/api/tickets/{id}/actions"),
            json!({ "action": action }),
            who,
        )
        .await
    }

    async fn detail(&self, id: Uuid, who: &str) -> Reply {
        self.get(&format!("/api/tickets/{id}"), who).await
    }

    async fn allow_skip(&self, allowed: bool) {
        sqlx::query("UPDATE organization SET allow_requester_ai_skip = $1")
            .bind(allowed)
            .execute(self.pool())
            .await
            .unwrap();
    }

    /// `owner`'s ticket in `ai_processing` with its one processing attempt, as
    /// a submission routed to AI leaves it.
    async fn processing(&self, owner: &str) -> Uuid {
        let mut tx = self.pool().begin().await.unwrap();
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description)
             VALUES ($1, $2, $3, 'Zebra printer ' || gen_random_uuid(), 'It jams on page two.')
             RETURNING id",
        )
        .bind(self.ids[owner])
        .bind(self.category)
        .bind(self.priority)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let ai_on = Policy {
            ai_available: true,
            ..Policy::default()
        };
        let submit = Transition::Submit {
            requester_skip: false,
        };
        lifecycle::transition_with(
            &mut tx,
            id,
            By::Account(self.ids[owner]),
            &submit,
            Some(ai_on),
        )
        .await
        .unwrap();
        fixtures::ai_attempt(&mut *tx, id, "processing").await;
        tx.commit().await.unwrap();
        id
    }

    /// Stores the suggestion as the attempt worker's commit does.
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

    /// `owner`'s ticket in `awaiting_ai_decision` with a stored suggestion.
    async fn suggested(&self, owner: &str) -> Uuid {
        let id = self.processing(owner).await;
        self.suggest(id).await;
        id
    }

    /// `owner`'s AI-solved record, confirmed through the HTTP decision.
    async fn solved(&self, owner: &str) -> Uuid {
        let id = self.suggested(owner).await;
        let r = self.action(id, "ai_solved", owner).await;
        assert_eq!(r.status, 200, "{}", r.body);
        id
    }

    async fn status(&self, id: Uuid) -> String {
        sqlx::query_scalar("SELECT status::text FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    async fn attempt_id(&self, id: Uuid) -> Uuid {
        sqlx::query_scalar("SELECT id FROM ai_attempts WHERE ticket_id = $1")
            .bind(id)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    /// `(event_type, to_status, actor, payload)` of the ticket's events, oldest first.
    async fn events(&self, id: Uuid) -> Vec<(String, Option<String>, Option<Uuid>, Value)> {
        sqlx::query_as(
            "SELECT event_type, to_status::text, actor_account_id, payload FROM ticket_events
             WHERE ticket_id = $1 ORDER BY id",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await
        .unwrap()
    }

    async fn event_types(&self, id: Uuid) -> Vec<String> {
        self.events(id).await.into_iter().map(|e| e.0).collect()
    }

    /// Everything stored about a ticket that a mutation could change: the row,
    /// its events, messages, attempt, screenshots, and queued notifications.
    async fn snapshot(&self, id: Uuid) -> Value {
        sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'ticket', to_jsonb(t),
                'events', (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.id) FROM ticket_events e WHERE e.ticket_id = t.id),
                'messages', (SELECT count(*) FROM ticket_messages m WHERE m.ticket_id = t.id),
                'attempt', (SELECT to_jsonb(a) FROM ai_attempts a WHERE a.ticket_id = t.id),
                'screenshots', (SELECT count(*) FROM screenshots s WHERE s.ticket_id = t.id),
                'outbox', (SELECT count(*) FROM notification_outbox o
                           JOIN ticket_events e ON e.id = o.event_id WHERE e.ticket_id = t.id))
             FROM tickets t WHERE t.id = $1",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// Notification rows for staff about this ticket.
    async fn staff_outbox(&self, id: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM notification_outbox o JOIN ticket_events e ON e.id = o.event_id
             WHERE e.ticket_id = $1 AND o.audience = 'staff'",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// `updated_at` as clients send it back in `expected_updated_at`.
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

    async fn queue_ids(&self, who: &str, query: &str) -> Vec<Uuid> {
        let r = self
            .get(&format!("/api/queue?limit=100&{query}"), who)
            .await;
        assert_eq!(r.status, 200, "{query}: {}", r.body);
        r.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().parse().unwrap())
            .collect()
    }

    async fn stats(&self) -> Value {
        let r = self.get("/api/admin/ai/outcomes", "admin").await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body
    }

    /// An intake screenshot row of the ticket, uploaded by its owner.
    async fn screenshot(&self, id: Uuid, owner: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO screenshots (id, uploader_account_id, ticket_id, filename, content_type,
                                      byte_size, width, height, attached_at)
             VALUES (gen_random_uuid(), $2, $1, 'shot.png', 'image/png', 10, 4, 3, now())
             RETURNING id",
        )
        .bind(id)
        .bind(self.ids[owner])
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// Every staff write the API offers on one ticket, as `(kind, path, body)`.
    async fn staff_writes(&self, id: Uuid) -> Vec<(&'static str, String, Value)> {
        let at = self.updated_at(id).await;
        let mut out = vec![];
        for body in [
            json!({ "action": "start_work", "expected_updated_at": at }),
            json!({ "action": "request_information", "expected_updated_at": at }),
            json!({ "action": "propose_resolution", "expected_updated_at": at, "summary": "Fixed" }),
            json!({ "action": "reopen", "expected_updated_at": at }),
            json!({ "action": "set_priority", "expected_updated_at": at, "priority_id": self.urgent }),
        ] {
            out.push(("staff", format!("/api/tickets/{id}/staff-actions"), body));
        }
        for body in [
            json!({ "action": "dispatch", "expected_updated_at": at, "team_id": self.desk }),
            json!({ "action": "return_to_general", "expected_updated_at": at }),
            json!({ "action": "claim", "expected_updated_at": at }),
            json!({ "action": "assign", "expected_updated_at": at, "assignee_account_id": self.ids["op"] }),
            json!({ "action": "unassign", "expected_updated_at": at }),
        ] {
            out.push(("assign", format!("/api/tickets/{id}/assignment"), body));
        }
        let message = || json!({ "idempotency_key": Uuid::new_v4(), "body": "Please reopen this" });
        out.push(("reply", format!("/api/tickets/{id}/replies"), message()));
        out.push(("note", format!("/api/tickets/{id}/notes"), message()));
        out
    }

    /// Every bulk change, applied to one ticket.
    async fn bulk_changes(&self, id: Uuid) -> Vec<Value> {
        let at = self.updated_at(id).await;
        [
            json!({ "action": "dispatch", "team_id": self.desk }),
            json!({ "action": "return_to_general" }),
            json!({ "action": "claim" }),
            json!({ "action": "assign", "assignee_account_id": self.ids["op"] }),
            json!({ "action": "unassign" }),
            json!({ "action": "start_work" }),
            json!({ "action": "request_information" }),
            json!({ "action": "set_priority", "priority_id": self.urgent }),
        ]
        .into_iter()
        .map(|change| json!({ "change": change, "tickets": [{ "id": id, "expected_updated_at": at }] }))
        .collect()
    }
}

/// The requester actions other than the AI decisions.
fn other_requester_actions() -> Vec<Value> {
    vec![
        json!({ "action": "reopen" }),
        json!({ "action": "cancel", "reason": "Send it to the help desk" }),
        json!({ "action": "accept_resolution", "proposal_version": 0 }),
        json!({ "action": "reject_resolution", "proposal_version": 0 }),
    ]
}

const AI_ACTIONS: [&str; 3] = ["ai_solved", "ai_not_solved", "skip_ai"];

// --- Solved --------------------------------------------------------------------

#[tokio::test]
async fn solved_needs_a_stored_suggestion_and_records_the_confirmation_once() {
    let Some(w) = world().await else { return };
    let id = w.processing("owner").await;
    let counts = w.get("/api/queue/counts", "op").await.body;

    // No suggestion is stored yet: nothing to confirm.
    let early = w.action(id, "ai_solved", "owner").await;
    assert_eq!((early.status.as_u16(), early.code()), (409, "conflict"));
    assert_eq!(early.body["error"]["details"]["status"], "ai_processing");
    assert_eq!(w.event_types(id).await, ["submit"]);

    w.suggest(id).await;
    let before = w.snapshot(id).await;
    // Only the owning requester decides; existence is not revealed to others.
    for action in AI_ACTIONS {
        for who in OTHERS {
            let r = w.action(id, action, who).await;
            let expected = if who == "admin" { 403 } else { 404 };
            assert_eq!(r.status, expected, "{action} by {who}: {}", r.body);
        }
    }
    assert_eq!(w.snapshot(id).await, before);

    let pending = w.detail(id, "owner").await.body;
    assert_eq!(pending["status"], "awaiting_ai_decision");
    assert_eq!(pending["ai"]["state"], "suggestion_ready");
    assert_eq!(pending["ai"]["decision"], Value::Null);
    assert_eq!(pending["ai"]["decided_at"], Value::Null);

    let r = w.action(id, "ai_solved", "owner").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "status": "ai_solved" }));

    // One row update: resolved state, AI source, requester confirmation, no team or assignee.
    let row: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('status', status, 'source', resolution_source,
                'closure', closure_reason, 'team', team_id, 'assignee', assignee_account_id,
                'updated_at', updated_at) FROM tickets WHERE id = $1",
    )
    .bind(id)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(row["status"], "ai_solved");
    assert_eq!(row["source"], "ai");
    assert_eq!(row["closure"], "requester_confirmed");
    assert_eq!(row["team"], Value::Null);
    assert_eq!(row["assignee"], Value::Null);
    // One event with the actor, the attempt, and the confirmation time.
    let events = w.events(id).await;
    let kinds: Vec<_> = events.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(kinds, ["submit", "ai_suggestion_ready", "ai_solved"]);
    let (_, to, actor, payload) = &events[2];
    assert_eq!(to.as_deref(), Some("ai_solved"));
    assert_eq!(*actor, Some(w.ids["owner"]));
    assert_eq!(
        *payload,
        json!({ "resolution_source": "ai", "closure_reason": "requester_confirmed",
                "attempt_id": w.attempt_id(id).await })
    );
    let confirmed_at: bool = sqlx::query_scalar(
        "SELECT e.created_at = t.updated_at FROM ticket_events e JOIN tickets t ON t.id = e.ticket_id
         WHERE t.id = $1 AND e.event_type = 'ai_solved'",
    )
    .bind(id)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert!(confirmed_at);
    // The attempt row is history: the decision did not rewrite it.
    assert_eq!(w.snapshot(id).await["attempt"], before["attempt"]);

    // No operator work: no staff notification, no queue entry, counts unchanged.
    assert_eq!(w.staff_outbox(id).await, 0);
    assert_eq!(w.get("/api/queue/counts", "op").await.body, counts);
    for who in OPERATORS {
        assert!(!w.queue_ids(who, "view=general").await.contains(&id));
    }

    // The owner keeps it as read-only history with the suggestion.
    let detail = w.detail(id, "owner").await.body;
    assert_eq!(detail["view"], "requester");
    assert_eq!(detail["status"], "ai_solved");
    assert_eq!(detail["resolution_source"], "ai");
    assert_eq!(detail["closure_reason"], "requester_confirmed");
    assert_eq!(detail["ai"]["decision"], "solved");
    assert!(detail["ai"]["decided_at"].is_string());
    assert_eq!(
        detail["ai"]["suggestion"],
        json!({ "explanation": "The tray is misaligned.",
                "steps": ["Open the tray", "Push it in until it clicks"] })
    );
    let history = w.get("/api/tickets?status=ai_solved", "owner").await.body;
    assert_eq!(history["items"][0]["id"], json!(id));
    assert_eq!(history["items"].as_array().unwrap().len(), 1);
    let none = w.get("/api/tickets?status=ai_solved", "other").await.body;
    assert_eq!(none["items"], json!([]));

    // Transport retries return the committed state and change nothing.
    let after = w.snapshot(id).await;
    for _ in 0..3 {
        let r = w.action(id, "ai_solved", "owner").await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body, json!({ "status": "ai_solved" }));
    }
    // The opposite decision and a skip lose, and say what was committed.
    for action in ["ai_not_solved", "skip_ai"] {
        let r = w.action(id, action, "owner").await;
        assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"), "{action}");
        assert_eq!(r.body["error"]["details"]["status"], "ai_solved");
    }
    assert_eq!(w.snapshot(id).await, after);
}

#[tokio::test]
async fn a_staff_account_decides_only_as_the_requester_of_its_own_ticket() {
    let Some(w) = world().await else { return };
    // A Desk operator's own ticket: requester identity wins over the role.
    let id = w.suggested("staffer").await;
    let detail = w.detail(id, "staffer").await.body;
    assert_eq!(detail["view"], "requester");
    assert_eq!(detail["staff_access"], Value::Null);
    // Team mates never see it while it is in AI.
    for who in ["op", "lead", "spec"] {
        assert_eq!(w.detail(id, who).await.status, 404, "{who}");
    }
    let r = w.action(id, "ai_solved", "staffer").await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The owner cannot work their own AI-solved record as staff either.
    let before = w.snapshot(id).await;
    for (kind, path, body) in w.staff_writes(id).await {
        let r = w.post(&path, body.clone(), "staffer").await;
        let expected = if kind == "reply" { 409 } else { 403 };
        assert_eq!(r.status, expected, "{path} {body}: {}", r.body);
    }
    for body in w.bulk_changes(id).await {
        let r = w.post("/api/tickets/bulk", body.clone(), "staffer").await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["results"][0]["error"], "forbidden", "{body}");
    }
    assert_eq!(w.snapshot(id).await, before);
    assert!(!w.queue_ids("staffer", "view=mine").await.contains(&id));
}

// --- Not solved ----------------------------------------------------------------

#[tokio::test]
async fn not_solved_queues_the_same_ticket_once_with_intake_and_the_suggestion() {
    let Some(w) = world().await else { return };
    let id = w.suggested("owner").await;
    let reference: i64 = sqlx::query_scalar("SELECT reference FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_one(w.pool())
        .await
        .unwrap();
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(w.pool())
        .await
        .unwrap();

    let r = w.action(id, "ai_not_solved", "owner").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "status": "queued" }));

    // The same ticket, in the shared general queue, with no team or assignee.
    let (team, assignee, now_reference): (Option<Uuid>, Option<Uuid>, i64) =
        sqlx::query_as("SELECT team_id, assignee_account_id, reference FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!((team, assignee, now_reference), (None, None, reference));
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(total, tickets, "no second ticket");
    let events = w.events(id).await;
    let (kind, to, actor, payload) = events.last().unwrap();
    assert_eq!(
        (kind.as_str(), to.as_deref()),
        ("ai_not_solved", Some("queued"))
    );
    assert_eq!(*actor, Some(w.ids["owner"]));
    assert_eq!(
        *payload,
        json!({ "route": "not_solved", "attempt_id": w.attempt_id(id).await })
    );
    for who in OPERATORS {
        let listed = w.queue_ids(who, "view=general").await;
        assert_eq!(listed.iter().filter(|t| **t == id).count(), 1, "{who}");
    }
    let notified = w.staff_outbox(id).await;
    assert!(notified > 0, "the help desk is told once");

    // Operators get the original intake and the stored suggestion as context.
    let staff = w.detail(id, "op").await.body;
    assert_eq!(staff["view"], "staff");
    assert_eq!(staff["description"], "It jams on page two.");
    assert_eq!(staff["ai"]["state"], "suggestion_ready");
    assert_eq!(staff["ai"]["decision"], "not_solved");
    assert_eq!(
        staff["ai"]["suggestion"]["explanation"],
        "The tray is misaligned."
    );
    assert_eq!(staff["transitions"].as_array().unwrap().len(), 3);
    let own = w.detail(id, "owner").await.body;
    assert_eq!(own["ai"], staff["ai"]);

    // Retries: the committed state, no second event, queue entry, or notification.
    let after = w.snapshot(id).await;
    for _ in 0..3 {
        let r = w.action(id, "ai_not_solved", "owner").await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body, json!({ "status": "queued" }));
    }
    assert_eq!(w.snapshot(id).await, after);
    assert_eq!(w.staff_outbox(id).await, notified);
    assert_eq!(
        w.queue_ids("op", "view=general")
            .await
            .iter()
            .filter(|t| **t == id)
            .count(),
        1
    );

    // Operators work on it. A stale "Solved" cannot take it back, and a late
    // retry of the escalation reports the current state without touching it.
    let at = w.updated_at(id).await;
    let path = format!("/api/tickets/{id}/assignment");
    let dispatch = json!({ "action": "dispatch", "expected_updated_at": at, "team_id": w.desk });
    assert_eq!(w.post(&path, dispatch, "free").await.status, 200);
    let at = w.updated_at(id).await;
    let claim = json!({ "action": "claim", "expected_updated_at": at });
    assert_eq!(w.post(&path, claim, "op").await.status, 200);
    let at = w.updated_at(id).await;
    let start = json!({ "action": "start_work", "expected_updated_at": at });
    let r = w
        .post(&format!("/api/tickets/{id}/staff-actions"), start, "op")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let working = w.snapshot(id).await;
    for action in ["ai_solved", "skip_ai"] {
        let r = w.action(id, action, "owner").await;
        assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"), "{action}");
        assert_eq!(r.body["error"]["details"]["status"], "in_progress");
    }
    let r = w.action(id, "ai_not_solved", "owner").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "status": "in_progress" }));
    assert_eq!(w.snapshot(id).await, working);
    assert_eq!(working["ticket"]["team_id"], json!(w.desk));
    assert_eq!(working["ticket"]["assignee_account_id"], json!(w.ids["op"]));
}

// --- Skip ----------------------------------------------------------------------

#[tokio::test]
async fn skip_and_premature_not_solved_follow_the_admin_policy() {
    let Some(w) = world().await else { return };
    let id = w.processing("owner").await;
    let before = w.snapshot(id).await;

    // Default: requesters cannot skip, and "still need help" is no way around it.
    for action in ["skip_ai", "ai_not_solved"] {
        let r = w.action(id, action, "owner").await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (403, "forbidden"),
            "{action}"
        );
    }
    assert_eq!(w.snapshot(id).await, before);
    assert_eq!(w.status(id).await, "ai_processing");
    for who in OPERATORS {
        assert!(!w.queue_ids(who, "view=general").await.contains(&id));
    }

    // Allowed by the Admin: a decision still needs a suggestion; the skip is explicit.
    w.allow_skip(true).await;
    for action in ["ai_not_solved", "ai_solved"] {
        let r = w.action(id, action, "owner").await;
        assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"), "{action}");
        assert_eq!(r.body["error"]["details"]["status"], "ai_processing");
    }
    assert_eq!(w.snapshot(id).await, before);
    for who in OTHERS {
        let r = w.action(id, "skip_ai", who).await;
        let expected = if who == "admin" { 403 } else { 404 };
        assert_eq!(r.status, expected, "{who}");
    }
    assert_eq!(w.snapshot(id).await, before);

    let r = w.action(id, "skip_ai", "owner").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "status": "queued" }));
    let events = w.events(id).await;
    let (kind, to, actor, payload) = events.last().unwrap();
    assert_eq!((kind.as_str(), to.as_deref()), ("skip_ai", Some("queued")));
    assert_eq!(*actor, Some(w.ids["owner"]));
    assert_eq!(
        *payload,
        json!({ "route": "requester_skip", "attempt_id": w.attempt_id(id).await })
    );
    let attempt: (String, Option<String>, bool) = sqlx::query_as(
        "SELECT state, outcome, explanation IS NULL FROM ai_attempts WHERE ticket_id = $1",
    )
    .bind(id)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(
        attempt,
        ("handed_off".into(), Some("requester_skip".into()), true)
    );
    for who in OPERATORS {
        let listed = w.queue_ids(who, "view=general").await;
        assert_eq!(listed.iter().filter(|t| **t == id).count(), 1, "{who}");
    }
    let staff = w.detail(id, "op").await.body;
    assert_eq!(
        staff["assignment"],
        json!({ "team_name": null, "assignee_display_name": null })
    );
    assert_eq!(staff["ai"]["state"], "handed_off");
    assert_eq!(staff["ai"]["reason"], "requester_skip");
    assert_eq!(staff["ai"]["suggestion"], Value::Null);
    assert_eq!(staff["ai"]["decision"], Value::Null);

    // A retry returns the committed state, even after the Admin forbids skipping again.
    let after = w.snapshot(id).await;
    assert_eq!(w.action(id, "skip_ai", "owner").await.status, 200);
    w.allow_skip(false).await;
    let r = w.action(id, "skip_ai", "owner").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "status": "queued" }));
    for action in ["ai_solved", "ai_not_solved"] {
        let r = w.action(id, action, "owner").await;
        assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"), "{action}");
        assert_eq!(r.body["error"]["details"]["status"], "queued");
    }
    assert_eq!(w.snapshot(id).await, after);

    // Once the suggestion exists there is nothing to skip: decide instead.
    w.allow_skip(true).await;
    let ready = w.suggested("owner").await;
    let before = w.snapshot(ready).await;
    let r = w.action(ready, "skip_ai", "owner").await;
    assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"));
    assert_eq!(r.body["error"]["details"]["status"], "awaiting_ai_decision");
    assert_eq!(w.snapshot(ready).await, before);
}

// --- Concurrency ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_opposite_decisions_and_retries_have_one_winner() {
    let Some(w) = world().await else { return };
    let mut winners = vec![];
    for _ in 0..6 {
        let id = w.suggested("owner").await;
        let mut calls = vec![];
        for n in 0..8 {
            let action = if n % 2 == 0 {
                "ai_solved"
            } else {
                "ai_not_solved"
            };
            let (app, cookie) = (w.app.clone(), w.cookies["owner"].clone());
            calls.push(tokio::spawn(async move {
                let path = format!("/api/tickets/{id}/actions");
                let body = json!({ "action": action });
                (
                    action,
                    call(&app, "POST", &path, Some(body), Some(&cookie)).await,
                )
            }));
        }
        let mut replies = vec![];
        for c in calls {
            replies.push(c.await.unwrap());
        }
        // Exactly one decision is stored, whatever raced.
        let kinds = w.event_types(id).await;
        assert_eq!(kinds.len(), 3, "{kinds:?}");
        let winner = kinds[2].clone();
        let committed = w.status(id).await;
        assert_eq!(
            committed,
            if winner == "ai_solved" {
                "ai_solved"
            } else {
                "queued"
            }
        );
        // Every caller learns the committed state: the winner and its retries
        // succeed with it, the opposite decision conflicts with it.
        for (action, r) in replies {
            if action == winner {
                assert_eq!(r.status, 200, "{action}: {}", r.body);
                assert_eq!(r.body["status"], json!(committed));
            } else {
                assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"), "{action}");
                assert_eq!(r.body["error"]["details"]["status"], json!(committed));
            }
        }
        let queued = w.queue_ids("op", "view=general").await.contains(&id);
        assert_eq!(queued, winner == "ai_not_solved");
        assert_eq!(w.staff_outbox(id).await > 0, winner == "ai_not_solved");
        winners.push(winner);
    }
    // Both outcomes are reachable; which one wins a given race is not asserted.
    assert!(
        winners
            .iter()
            .all(|k| k == "ai_solved" || k == "ai_not_solved")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_skip_racing_the_worker_cancellation_and_retries_ends_processing_once() {
    let Some(w) = world().await else { return };
    w.allow_skip(true).await;
    for round in 0..6 {
        let id = w.processing("owner").await;
        let mut calls = vec![];
        for n in 0..4 {
            let body = if n == 3 {
                json!({ "action": "cancel", "reason": "Not needed" })
            } else {
                json!({ "action": "skip_ai" })
            };
            let (app, cookie) = (w.app.clone(), w.cookies["owner"].clone());
            calls.push(tokio::spawn(async move {
                let path = format!("/api/tickets/{id}/actions");
                call(&app, "POST", &path, Some(body), Some(&cookie))
                    .await
                    .status
                    .as_u16()
            }));
        }
        // The system side: a failed attempt, or a suggestion arriving just now.
        let pool = w.pool().clone();
        let system = tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            if round % 2 == 0 {
                ai_attempts::invalidate(&mut tx, None, "ai_disabled")
                    .await
                    .unwrap();
            } else {
                let t = Transition::AiSuggestionReady;
                if lifecycle::transition(&mut tx, id, By::System, &t)
                    .await
                    .is_err()
                {
                    return;
                }
                sqlx::query(
                    "UPDATE ai_attempts SET state = 'suggestion_ready', explanation = 'x',
                            steps = '[\"y\"]', finished_at = clock_timestamp() WHERE ticket_id = $1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
            }
            tx.commit().await.unwrap();
        });
        for c in calls {
            let status = c.await.unwrap();
            assert!([200, 409].contains(&status), "{status}");
        }
        system.await.unwrap();
        // `ai_processing` was left exactly once, and the ticket was queued at most once.
        let events = w.events(id).await;
        let left: Vec<_> = events
            .iter()
            .filter(|e| ["skip_ai", "ai_failed", "ai_suggestion_ready"].contains(&e.0.as_str()))
            .collect();
        let cancelled = events.iter().filter(|e| e.0 == "cancel").count();
        assert!(left.len() <= 1 && cancelled <= 1, "{events:?}");
        assert!(left.len() + cancelled >= 1, "{events:?}");
        let queued = events
            .iter()
            .filter(|e| e.1.as_deref() == Some("queued"))
            .count();
        assert!(queued <= 1, "{events:?}");
        let attempt: String =
            sqlx::query_scalar("SELECT state FROM ai_attempts WHERE ticket_id = $1")
                .bind(id)
                .fetch_one(w.pool())
                .await
                .unwrap();
        assert_ne!(attempt, "processing");
        assert_ne!(w.status(id).await, "ai_processing");
    }
}

// --- Finality ------------------------------------------------------------------

#[tokio::test]
async fn no_role_can_mutate_an_ai_solved_record() {
    let Some(w) = world().await else { return };
    // The most permissive policy: reopening and skipping are both allowed.
    sqlx::query("UPDATE organization SET allow_reopening = true, allow_requester_ai_skip = true")
        .execute(w.pool())
        .await
        .unwrap();
    let id = w.solved("owner").await;
    w.screenshot(id, "owner").await;
    let before = w.snapshot(id).await;
    let stats = w.stats().await;
    let counts: HashMap<_, _> = {
        let mut counts = HashMap::new();
        for who in OPERATORS.into_iter().chain(["admin"]) {
            counts.insert(who, w.get("/api/queue/counts", who).await.body);
        }
        counts
    };

    for who in ["owner"].into_iter().chain(OTHERS) {
        let operator = OPERATORS.contains(&who);
        // Requester decisions: reopen, cancel, escalate, accept, reject, skip.
        let mut actions = other_requester_actions();
        actions.extend(["ai_not_solved", "skip_ai"].map(|a| json!({ "action": a })));
        for body in actions {
            let r = w
                .post(&format!("/api/tickets/{id}/actions"), body.clone(), who)
                .await;
            let expected = match who {
                "owner" => 409,
                "admin" => 403,
                _ => 404,
            };
            assert_eq!(r.status, expected, "{who} {body}: {}", r.body);
        }
        // Staff work: status, priority, dispatch, claim, assign, reply, note.
        for (kind, path, body) in w.staff_writes(id).await {
            let r = w.post(&path, body.clone(), who).await;
            let expected = match (who, kind) {
                ("owner", "reply") => 409,
                ("owner", _) | ("admin", _) | ("other", "staff") => 403,
                _ => 404,
            };
            assert_eq!(r.status, expected, "{who} {path} {body}: {}", r.body);
        }
        for body in w.bulk_changes(id).await {
            let r = w.post("/api/tickets/bulk", body.clone(), who).await;
            if operator || who == "admin" {
                assert_eq!(r.status, 200, "{who}: {}", r.body);
                let expected = if operator { "not_found" } else { "forbidden" };
                assert_eq!(r.body["results"][0]["error"], expected, "{who} {body}");
            } else {
                assert_eq!(r.status, 403, "{who}: {}", r.body);
            }
        }
        assert_eq!(w.snapshot(id).await, before, "after {who}");
    }

    // Denied mutations moved no event, statistic, count, or notification.
    assert_eq!(w.stats().await, stats);
    for (who, expected) in counts {
        assert_eq!(
            w.get("/api/queue/counts", who).await.body,
            expected,
            "{who}"
        );
    }
    assert_eq!(w.detail(id, "owner").await.body["status"], "ai_solved");
}

#[tokio::test]
async fn jobs_late_callbacks_and_direct_writes_cannot_reopen_an_ai_solved_record() {
    let Some(w) = world().await else { return };
    let id = w.solved("owner").await;
    let before = w.snapshot(id).await;

    // Late AI results, timeouts, and every other transition, by any actor.
    let transitions = [
        Transition::AiSuggestionReady,
        Transition::AiFailed,
        Transition::SkipAi,
        Transition::AiNotSolved,
        Transition::StartWork,
        Transition::Reopen,
        Transition::ConfirmationTimeout {
            proposal_version: 0,
        },
        Transition::Cancel {
            reason: "late".into(),
        },
    ];
    let permissive = Policy {
        ai_available: true,
        allow_requester_skip: true,
        allow_reopen: true,
        ..Policy::default()
    };
    for t in &transitions {
        for by in [
            By::System,
            By::Account(w.ids["owner"]),
            By::Account(w.ids["admin"]),
            By::Account(w.ids["op"]),
        ] {
            let mut tx = w.pool().begin().await.unwrap();
            let r = lifecycle::transition_with(&mut tx, id, by, t, Some(permissive)).await;
            assert!(r.is_err(), "{t:?} by {by:?}");
        }
    }
    // The workers: attempt sweep and invalidation, auto-close, notifications.
    let worker = ai_attempts::Worker::new(&test_state(w.pool()), [0; 32]);
    assert!(!ai_attempts::sweep_one(&worker).await.unwrap());
    let mut tx = w.pool().begin().await.unwrap();
    assert_eq!(
        ai_attempts::invalidate(&mut tx, None, "ai_disabled")
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    assert!(!autoclose::close_one(w.pool()).await.unwrap());
    while notifications::process_one(w.pool(), &Settings::default(), &Default::default())
        .await
        .unwrap()
    {}
    // Only the outbox bookkeeping of the requester's receipt moved.
    assert_eq!(w.snapshot(id).await, before);

    // Writes that bypass the lifecycle hit the database backstops.
    for sql in [
        "UPDATE tickets SET status = 'queued', resolution_source = NULL, closure_reason = NULL WHERE id = $1",
        "UPDATE tickets SET priority_id = (SELECT id FROM priorities WHERE name = 'Urgent') WHERE id = $1",
        "UPDATE tickets SET updated_at = now() WHERE id = $1",
        "UPDATE tickets SET resolution_source = 'operator' WHERE id = $1",
        "UPDATE ai_attempts SET state = 'handed_off', outcome = 'timeout', explanation = NULL, steps = NULL WHERE ticket_id = $1",
        "DELETE FROM ai_attempts WHERE ticket_id = $1",
        "DELETE FROM ticket_events WHERE ticket_id = $1",
        "INSERT INTO ticket_events (ticket_id, event_type, from_status, to_status)
         VALUES ($1, 'ai_not_solved', 'awaiting_ai_decision', 'queued')",
        "INSERT INTO ticket_events (ticket_id, event_type, from_status, to_status)
         VALUES ($1, 'ai_solved', 'awaiting_ai_decision', 'ai_solved')",
        "INSERT INTO ticket_events (ticket_id, event_type, from_status, to_status)
         VALUES ($1, 'cancel', 'awaiting_ai_decision', 'cancelled')",
        "INSERT INTO ticket_events (ticket_id, event_type) VALUES ($1, 'public_reply_added')",
        "INSERT INTO ticket_events (ticket_id, event_type) VALUES ($1, 'ai_failed')",
    ] {
        let r = sqlx::query(sql).bind(id).execute(w.pool()).await;
        assert!(r.is_err(), "{sql}");
    }
    assert_eq!(w.snapshot(id).await, before);

    // The operators' side never saw any of it.
    assert_eq!(w.staff_outbox(id).await, 0);
    for who in OPERATORS {
        let r = w.get("/api/notifications", who).await;
        assert_eq!(r.body["items"], json!([]), "{who}");
        assert_eq!(
            w.get("/api/notifications/unread", who).await.body["count"],
            0
        );
    }
    // The requester's receipt is the one notification.
    let own = w.get("/api/notifications", "owner").await.body;
    let titles: Vec<_> = own["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Receipt: you confirmed the AI solution"]);
}

/// A decision without a stored suggestion cannot commit, even if a ticket were
/// ever left in `awaiting_ai_decision` without one.
#[tokio::test]
async fn a_decision_without_a_stored_suggestion_is_refused() {
    let Some(w) = world().await else { return };
    let id = w.processing("owner").await;
    let mut tx = w.pool().begin().await.unwrap();
    lifecycle::transition(&mut tx, id, By::System, &Transition::AiSuggestionReady)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = w.snapshot(id).await;
    for t in [Transition::AiSolved, Transition::AiNotSolved] {
        let mut tx = w.pool().begin().await.unwrap();
        let r = lifecycle::transition(&mut tx, id, By::Account(w.ids["owner"]), &t).await;
        assert!(matches!(r, Err(ApiError::Conflict)), "{t:?}");
    }
    for action in ["ai_solved", "ai_not_solved"] {
        let r = w.action(id, action, "owner").await;
        assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"), "{action}");
    }
    assert_eq!(w.snapshot(id).await, before);
}

// --- One suggestion, no chat ---------------------------------------------------

#[tokio::test]
async fn there_is_no_chat_or_regenerate_and_replies_do_not_decide() {
    let Some(w) = world().await else { return };
    let id = w.suggested("owner").await;
    let before = w.snapshot(id).await;

    // No such routes.
    for path in [
        format!("/api/tickets/{id}/ai"),
        format!("/api/tickets/{id}/ai/regenerate"),
        format!("/api/tickets/{id}/ai/retry"),
        format!("/api/tickets/{id}/ai/chat"),
        format!("/api/tickets/{id}/ai/messages"),
        format!("/api/tickets/{id}/suggestion"),
        format!("/api/tickets/{id}/suggestions"),
        format!("/api/tickets/{id}/regenerate"),
        format!("/api/tickets/{id}/chat"),
        format!("/api/ai/attempts/{}", w.attempt_id(id).await),
        "/api/ai/attempts".to_owned(),
        "/api/ai/chat".to_owned(),
        "/api/ai/suggestions".to_owned(),
    ] {
        for method in ["POST", "PUT", "PATCH"] {
            let body = Some(json!({ "message": "Try again", "ticket_id": id }));
            let r = call(&w.app, method, &path, body, Some(&w.cookies["owner"])).await;
            assert_eq!(
                (r.status.as_u16(), r.code()),
                (404, "not_found"),
                "{method} {path}"
            );
        }
    }
    // No such actions, and no extra input on a decision.
    for body in [
        json!({ "action": "regenerate" }),
        json!({ "action": "ai_regenerate" }),
        json!({ "action": "ai_retry" }),
        json!({ "action": "ai_chat", "message": "Explain step two" }),
        json!({ "action": "ai_not_solved", "message": "Try something else" }),
        json!({ "action": "ai_solved", "regenerate": true }),
        json!({ "action": "skip_ai", "prompt": "Shorter please" }),
        json!({ "action": "reopen", "message": "Ask the AI again" }),
    ] {
        let r = w
            .post(&format!("/api/tickets/{id}/actions"), body.clone(), "owner")
            .await;
        assert_eq!(r.status, 422, "{body}: {}", r.body);
    }
    // A reply while the suggestion waits is not a conversation with the AI and
    // not a decision.
    for kind in ["replies", "notes"] {
        let body =
            json!({ "idempotency_key": Uuid::new_v4(), "body": "That did not work, try again" });
        let r = w
            .post(&format!("/api/tickets/{id}/{kind}"), body, "owner")
            .await;
        assert!(
            [403, 409].contains(&r.status.as_u16()),
            "{kind}: {}",
            r.body
        );
    }
    // Submitting again cannot attach a second attempt to the ticket.
    let second = sqlx::query(
        "INSERT INTO ai_attempts (ticket_id, language, input, provider, model, credential_set_at, deadline_at)
         VALUES ($1, 'en', '{}', 'openai', 'test-model', now(), now())",
    )
    .bind(id)
    .execute(w.pool())
    .await;
    assert!(second.is_err());
    assert_eq!(w.snapshot(id).await, before);
    assert_eq!(w.status(id).await, "awaiting_ai_decision");

    // The decision still applies to that one suggestion afterwards.
    assert_eq!(w.action(id, "ai_not_solved", "owner").await.status, 200);
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_attempts WHERE ticket_id = $1")
        .bind(id)
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(attempts, 1);
}

// --- Operator paths ------------------------------------------------------------

#[tokio::test]
async fn ai_stage_and_ai_solved_records_are_absent_from_every_operator_path() {
    let Some(w) = world().await else { return };
    // The baseline: one ordinary escalated ticket operators do see.
    let visible = w.suggested("owner").await;
    assert_eq!(
        w.action(visible, "ai_not_solved", "owner").await.status,
        200
    );
    let mut baseline = HashMap::new();
    for who in OPERATORS.into_iter().chain(["admin"]) {
        baseline.insert(who, w.get("/api/queue/counts", who).await.body);
    }

    let hidden = [
        ("ai_processing", w.processing("owner").await),
        ("awaiting_ai_decision", w.suggested("owner").await),
        ("ai_solved", w.solved("owner").await),
    ];
    let mut shots = vec![];
    for (_, id) in hidden {
        shots.push(w.screenshot(id, "owner").await);
    }
    let before = {
        let mut all = vec![];
        for (_, id) in hidden {
            all.push(w.snapshot(id).await);
        }
        all
    };

    for who in OPERATORS.into_iter().chain(["admin"]) {
        let admin = who == "admin";
        // Counts and their change marker do not move for work that is not theirs.
        assert_eq!(
            w.get("/api/queue/counts", who).await.body,
            baseline[who],
            "{who}"
        );
        // Queues: every view and status filter, and search by text or reference.
        let mut queries = vec![];
        let views: &[&str] = if admin {
            &["general", "team", "all"]
        } else {
            &["general", "team", "mine"]
        };
        for view in views {
            queries.push(format!("view={view}"));
            for status in [
                "queued",
                "in_progress",
                "waiting_for_requester",
                "awaiting_confirmation",
                "closed",
                "cancelled",
            ] {
                queries.push(format!("view={view}&status={status}"));
            }
            queries.push(format!("view={view}&q=Zebra"));
            queries.push(format!("view={view}&q=jams%20on%20page"));
            queries.push(format!("view={view}&q=owner"));
            queries.push(format!("view={view}&unassigned=true&sort=oldest"));
            queries.push(format!("view={view}&sort=priority"));
        }
        for query in &queries {
            let listed = w.queue_ids(who, query).await;
            for (status, id) in hidden {
                assert!(!listed.contains(&id), "{who} {query} lists {status}");
            }
        }
        assert!(
            w.queue_ids(who, "view=general&q=Zebra")
                .await
                .contains(&visible)
        );
        for (status, id) in hidden {
            let reference: i64 = sqlx::query_scalar("SELECT reference FROM tickets WHERE id = $1")
                .bind(id)
                .fetch_one(w.pool())
                .await
                .unwrap();
            let found = w
                .queue_ids(who, &format!("view=general&q=%23{reference}"))
                .await;
            assert!(found.is_empty(), "{who} finds {status} by reference");
        }
        // The AI states are not queue filters, live or saved.
        for status in ["ai_processing", "awaiting_ai_decision", "ai_solved"] {
            let r = w.get(&format!("/api/queue?status={status}"), who).await;
            assert_eq!(
                (r.status.as_u16(), r.code()),
                (400, "invalid_query"),
                "{who} {status}"
            );
            let view = json!({ "name": format!("AI {status}"), "filters": { "status": status } });
            let r = w.post("/api/saved-views", view, who).await;
            assert_eq!(r.status, 422, "{who} {status}: {}", r.body);
        }

        for (n, (status, id)) in hidden.into_iter().enumerate() {
            // Detail, activity polling, conversation.
            for path in [
                format!("/api/tickets/{id}"),
                format!("/api/tickets/{id}/activity"),
                format!("/api/tickets/{id}/messages"),
                format!("/api/screenshots/{}", shots[n]),
                format!("/api/screenshots/{}/preview", shots[n]),
                format!("/api/screenshots/{}/download", shots[n]),
            ] {
                let r = w.get(&path, who).await;
                if admin {
                    // Admins may inspect; the file itself is not stored in this test.
                    assert_ne!(r.status, 403, "{path}");
                } else {
                    assert_eq!(
                        (r.status.as_u16(), r.code()),
                        (404, "not_found"),
                        "{who} {status} {path}"
                    );
                }
            }
            // Writes, single and bulk.
            for (_, path, body) in w.staff_writes(id).await {
                let r = w.post(&path, body.clone(), who).await;
                let expected = if admin { 403 } else { 404 };
                assert_eq!(
                    r.status, expected,
                    "{who} {status} {path} {body}: {}",
                    r.body
                );
            }
            for body in w.bulk_changes(id).await {
                let r = w.post("/api/tickets/bulk", body.clone(), who).await;
                let expected = if admin { "forbidden" } else { "not_found" };
                assert_eq!(
                    r.body["results"][0]["error"], expected,
                    "{who} {status} {body}"
                );
            }
            for action in AI_ACTIONS {
                let r = w.action(id, action, who).await;
                assert_eq!(
                    r.status,
                    if admin { 403 } else { 404 },
                    "{who} {status} {action}"
                );
            }
            // Notifications: nothing was queued for staff, nothing is listed.
            assert_eq!(w.staff_outbox(id).await, 0, "{status}");
        }
        while notifications::process_one(w.pool(), &Settings::default(), &Default::default())
            .await
            .unwrap()
        {}
        let inbox = w.get("/api/notifications", who).await.body;
        for n in inbox["items"].as_array().unwrap() {
            for (status, id) in hidden {
                assert!(
                    !n["href"].as_str().unwrap().contains(&id.to_string()),
                    "{who} {status}"
                );
            }
        }
    }
    // Nothing above changed a hidden ticket.
    for (n, (_, id)) in hidden.into_iter().enumerate() {
        let now = w.snapshot(id).await;
        assert_eq!(now["ticket"], before[n]["ticket"]);
        assert_eq!(now["events"], before[n]["events"]);
        assert_eq!(now["attempt"], before[n]["attempt"]);
        assert_eq!(now["messages"], before[n]["messages"]);
    }

    // Admins inspect them through ticket detail (read-only), never as queue work.
    for (status, id) in hidden {
        let r = w.detail(id, "admin").await;
        assert_eq!(r.status, 200, "{status}");
        assert_eq!(r.body["view"], "staff");
        assert_eq!(r.body["status"], status);
        let access = &r.body["staff_access"];
        for flag in [
            "can_handle",
            "can_reopen",
            "can_dispatch",
            "can_claim",
            "can_assign",
        ] {
            assert_eq!(access[flag], false, "{status} {flag}");
        }
        assert!(r.body["ai"]["state"].is_string());
        assert_eq!(
            w.get(&format!("/api/tickets/{id}/activity"), "admin")
                .await
                .status,
            200
        );
    }
    let all = w.get("/api/queue/counts", "admin").await.body;
    assert_eq!(all["all"], 1, "only the escalated ticket is work: {all}");
}

// --- Admin visibility ----------------------------------------------------------

#[tokio::test]
async fn admins_inspect_ai_records_and_outcome_statistics() {
    let Some(w) = world().await else { return };
    w.allow_skip(true).await;
    let solved = w.solved("owner").await;
    let not_solved = w.suggested("owner").await;
    assert_eq!(
        w.action(not_solved, "ai_not_solved", "owner").await.status,
        200
    );
    let failed = w.processing("other").await;
    let mut tx = w.pool().begin().await.unwrap();
    assert_eq!(
        ai_attempts::invalidate(&mut tx, None, "ai_disabled")
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let skipped = w.processing("owner").await;
    assert_eq!(w.action(skipped, "skip_ai", "owner").await.status, 200);
    let cancelled = w.suggested("owner").await;
    let cancel = json!({ "action": "cancel", "reason": "Found it myself" });
    let r = w
        .post(
            &format!("/api/tickets/{cancelled}/actions"),
            cancel,
            "owner",
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let awaiting = w.suggested("staffer").await;
    let processing = w.processing("owner").await;

    // Admin only, for both reads.
    for path in ["/api/admin/ai/records", "/api/admin/ai/outcomes"] {
        for who in ["owner", "other", "op", "lead", "spec", "free", "staffer"] {
            let r = w.get(path, who).await;
            assert_eq!(
                (r.status.as_u16(), r.code()),
                (403, "forbidden"),
                "{who} {path}"
            );
        }
        assert_eq!(call(&w.app, "GET", path, None, None).await.status, 401);
    }

    let stats = w.stats().await;
    assert_eq!(
        stats,
        json!({ "days": 30, "solved": 1, "not_solved": 1, "skipped": 1, "failed": 1,
                "category_exempt": 0, "cancelled": 1, "processing": 1, "awaiting_decision": 1,
                "failures": [{ "outcome": "ai_disabled", "count": 1 }] })
    );
    for days in ["0", "3651"] {
        let r = w
            .get(&format!("/api/admin/ai/outcomes?days={days}"), "admin")
            .await;
        assert_eq!(r.status, 422, "{days}");
    }

    let expected = [
        ("solved", solved, "ai_solved"),
        ("not_solved", not_solved, "queued"),
        ("failed", failed, "queued"),
        ("skipped", skipped, "queued"),
        ("cancelled", cancelled, "cancelled"),
        ("awaiting_decision", awaiting, "awaiting_ai_decision"),
        ("processing", processing, "ai_processing"),
    ];
    let all = w.get("/api/admin/ai/records?limit=100", "admin").await.body;
    let items = all["items"].as_array().unwrap();
    assert_eq!(items.len(), expected.len());
    for (outcome, id, status) in expected {
        let page = w
            .get(&format!("/api/admin/ai/records?outcome={outcome}"), "admin")
            .await
            .body;
        let rows = page["items"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{outcome}: {page}");
        let row = &rows[0];
        assert_eq!(row["ticket_id"], json!(id), "{outcome}");
        assert_eq!(row["outcome"], outcome);
        assert_eq!(row["status"], status);
        assert_eq!(row["id"], json!(w.attempt_id(id).await));
        assert_eq!(row["provider"], "openai");
        assert_eq!(
            row["decided_at"].is_string(),
            ["solved", "not_solved"].contains(&outcome),
            "{outcome}"
        );
    }
    let row = |outcome: &str| items.iter().find(|r| r["outcome"] == outcome).unwrap();
    assert_eq!(row("failed")["outcome_code"], "ai_disabled");
    assert_eq!(row("failed")["requester"]["username"], "other");
    assert_eq!(row("skipped")["outcome_code"], "requester_skip");
    assert_eq!(row("solved")["outcome_code"], Value::Null);
    assert_eq!(row("solved")["attempt_state"], "suggestion_ready");
    assert_eq!(row("awaiting_decision")["requester"]["username"], "staffer");

    // Newest first, paged by cursor without repeats.
    let mut seen = vec![];
    let mut cursor = String::new();
    loop {
        let page = w
            .get(&format!("/api/admin/ai/records?limit=3{cursor}"), "admin")
            .await
            .body;
        for r in page["items"].as_array().unwrap() {
            seen.push(r["id"].clone());
        }
        match page["next_cursor"].as_str() {
            Some(next) => cursor = format!("&cursor={next}"),
            None => break,
        }
    }
    let ids: Vec<_> = items.iter().map(|r| r["id"].clone()).collect();
    assert_eq!(seen, ids);
    let r = w
        .get("/api/admin/ai/records?outcome=reopened", "admin")
        .await;
    assert_eq!((r.status.as_u16(), r.code()), (400, "invalid_query"));

    // The Admin reads the solved record itself, with the suggestion, read-only.
    let detail = w.detail(solved, "admin").await.body;
    assert_eq!(detail["status"], "ai_solved");
    assert_eq!(detail["resolution_source"], "ai");
    assert_eq!(detail["ai"]["decision"], "solved");
    assert_eq!(
        detail["ai"]["suggestion"]["explanation"],
        "The tray is misaligned."
    );
    assert_eq!(detail["requester"]["username"], "owner");
    assert_eq!(detail["staff_access"]["can_reopen"], false);

    // Retries and refused requests do not move the statistics.
    for id in [solved, not_solved, skipped, cancelled] {
        for action in AI_ACTIONS {
            w.action(id, action, "owner").await;
            w.action(id, action, "admin").await;
            w.action(id, action, "op").await;
        }
    }
    assert_eq!(w.stats().await, stats);

    // A category exemption and a submission-time skip are counted, not listed
    // as attempts: no provider call and no decision is involved.
    sqlx::query(
        "INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, payload)
         VALUES ($1, $2, 'submit', 'draft', 'queued', '{\"route\": \"category_exempt\"}'),
                ($1, $2, 'submit', 'draft', 'queued', '{\"route\": \"requester_skip\"}')",
    )
    .bind(not_solved)
    .bind(w.ids["owner"])
    .execute(w.pool())
    .await
    .unwrap();
    let stats = w.stats().await;
    assert_eq!(
        (stats["category_exempt"].clone(), stats["skipped"].clone()),
        (json!(1), json!(2))
    );
}
