//! Manual dispatch, transfer, return to intake, claim, and Lead/Admin assignment over
//! HTTP: targets, stale and concurrent changes, team isolation, self-handling,
//! retained context, and AI-solved exclusion (docs/queues.md).
use std::collections::HashMap;

use axum::Router;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

mod support;
use support::{
    Reply, TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};
use tikkit_api::lifecycle::{By, CancellationPolicy, Policy, Transition, transition_with};

struct World {
    db: TestDb,
    app: Router,
    ids: HashMap<&'static str, Uuid>,
    cookies: HashMap<&'static str, String>,
    category: Uuid,
    exempt: Uuid,
    normal: Uuid,
    desk: Uuid,
    network: Uuid,
}

const PEOPLE: [(&str, Role); 9] = [
    ("owner", Role::User),
    ("op", Role::Operator),
    ("peer", Role::Specialist),
    ("lead", Role::TeamLead),
    ("outsider", Role::Operator),
    ("net_lead", Role::TeamLead),
    ("net_op", Role::Operator),
    ("gone", Role::Operator),
    ("admin2", Role::Admin),
];

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    let mut ids = HashMap::from([("admin", fixtures::bootstrap_admin(p).await)]);
    for (name, role) in PEOPLE {
        ids.insert(name, fixtures::account(p, role, name).await);
    }
    let desk = fixtures::team(p, "Desk", ids["lead"]).await.unwrap();
    let network = fixtures::team(p, "Network", ids["net_lead"]).await.unwrap();
    for name in ["op", "peer", "gone"] {
        fixtures::member(p, desk, ids[name]).await.unwrap();
    }
    fixtures::member(p, network, ids["net_op"]).await.unwrap();
    let id = |sql: &'static str| sqlx::query_scalar(sql).fetch_one(p);
    let category = id("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
        .await
        .unwrap();
    let exempt = id("INSERT INTO categories (name, skip_ai) VALUES ('Network', true) RETURNING id")
        .await
        .unwrap();
    let normal = id("INSERT INTO priorities (name, sort_order) VALUES ('Normal', 2) RETURNING id")
        .await
        .unwrap();
    let app = test_app(p);
    let mut cookies = HashMap::new();
    for name in ids.keys().copied().collect::<Vec<_>>() {
        if name != "gone" {
            cookies.insert(name, login(&app, name, PASSWORD).await.session());
        }
    }
    // An inactive member of Desk: never an eligible assignee.
    sqlx::query("UPDATE accounts SET is_active = false WHERE id = $1")
        .bind(ids["gone"])
        .execute(p)
        .await
        .unwrap();
    Some(World {
        db,
        app,
        ids,
        cookies,
        category,
        exempt,
        normal,
        desk,
        network,
    })
}

impl World {
    /// A ticket by `requester` in `status`, optionally in a team and assigned.
    async fn seed(
        &self,
        requester: &str,
        status: &str,
        team: Option<Uuid>,
        assignee: Option<&str>,
    ) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description,
                status, team_id, assignee_account_id, proposal_version, resolution_source, closure_reason)
             VALUES ($1, $2, $3, 'Printer', 'Jams', $4, $5, $6,
               CASE WHEN $4 = 'closed' THEN 1 ELSE 0 END,
               CASE $4 WHEN 'closed' THEN 'operator' WHEN 'ai_solved' THEN 'ai' END,
               CASE WHEN $4 IN ('closed', 'ai_solved') THEN 'requester_confirmed' END)
             RETURNING id",
        )
        .bind(self.ids[requester])
        .bind(self.category)
        .bind(self.normal)
        .bind(status)
        .bind(team)
        .bind(assignee.map(|a| self.ids[a]))
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
    }
    async fn detail(&self, id: Uuid, who: &str) -> Reply {
        let path = format!("/api/tickets/{id}");
        call(&self.app, "GET", &path, None, Some(&self.cookies[who])).await
    }
    async fn updated_at(&self, id: Uuid) -> Value {
        let at: OffsetDateTime = sqlx::query_scalar("SELECT updated_at FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(&self.db.pool)
            .await
            .unwrap();
        json!(
            at.format(&time::format_description::well_known::Rfc3339)
                .unwrap()
        )
    }
    /// An assignment change with the ticket's current `updated_at`.
    async fn change(&self, id: Uuid, who: &str, action: Value) -> Reply {
        let at = self.updated_at(id).await;
        self.change_at(id, who, action, at).await
    }
    async fn change_at(&self, id: Uuid, who: &str, mut action: Value, at: Value) -> Reply {
        action["expected_updated_at"] = at;
        let path = format!("/api/tickets/{id}/assignment");
        call(
            &self.app,
            "POST",
            &path,
            Some(action),
            Some(&self.cookies[who]),
        )
        .await
    }
    async fn placement(&self, id: Uuid) -> (Option<Uuid>, Option<Uuid>) {
        sqlx::query_as("SELECT team_id, assignee_account_id FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(&self.db.pool)
            .await
            .unwrap()
    }
    async fn events(&self, id: Uuid) -> Vec<(Uuid, Value)> {
        sqlx::query_as(
            "SELECT actor_account_id, payload FROM ticket_events
             WHERE ticket_id = $1 AND event_type = 'assignment_changed' ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.db.pool)
        .await
        .unwrap()
    }
    async fn queue(&self, who: &str, view: &str) -> Vec<Uuid> {
        let path = format!("/api/queue?view={view}&limit=100");
        let r = call(&self.app, "GET", &path, None, Some(&self.cookies[who])).await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().parse().unwrap())
            .collect()
    }
    fn dispatch(&self, team: Uuid) -> Value {
        json!({ "action": "dispatch", "team_id": team })
    }
    fn assign(&self, who: &str) -> Value {
        json!({ "action": "assign", "assignee_account_id": self.ids[who] })
    }
}

/// Transitions that take a fresh draft into human support.
type Steps = Vec<(By, Transition)>;

fn act(action: &str) -> Value {
    json!({ "action": action })
}

/// Every route into human support (AI off, category exempt, requester skip, AI
/// failure, not solved) lands once in shared intake without team or assignee,
/// whatever the category; dispatch then moves that one ticket.
#[tokio::test]
async fn every_intake_route_enters_general_intake_once_and_dispatches_manually() {
    let Some(w) = world().await else { return };
    let on = Policy {
        ai_available: true,
        allow_requester_skip: true,
        allow_reopen: false,
        cancellation: CancellationPolicy::AnytimeWhileOpen,
        confirmation_timeout_hours: 168,
    };
    let off = Policy {
        ai_available: false,
        ..on
    };
    let owner = By::Account(w.ids["owner"]);
    let skip = Transition::Submit {
        requester_skip: true,
    };
    let plain = Transition::Submit {
        requester_skip: false,
    };
    let cases: Vec<(&str, Uuid, Policy, Steps)> = vec![
        ("ai_disabled", w.category, off, vec![(owner, plain.clone())]),
        ("ai_disabled", w.exempt, off, vec![(owner, plain.clone())]),
        (
            "category_exempt",
            w.exempt,
            on,
            vec![(owner, plain.clone())],
        ),
        ("requester_skip", w.category, on, vec![(owner, skip)]),
        (
            "ai_failed",
            w.category,
            on,
            vec![(owner, plain.clone()), (By::System, Transition::AiFailed)],
        ),
        (
            "not_solved",
            w.category,
            on,
            vec![
                (owner, plain.clone()),
                (By::System, Transition::AiSuggestionReady),
                (owner, Transition::AiNotSolved),
            ],
        ),
    ];
    let before = w.queue("op", "general").await.len();
    let mut queued = vec![];
    for (route, category, policy, steps) in cases {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description)
             VALUES ($1, $2, $3, 'Printer', 'Jams') RETURNING id",
        )
        .bind(w.ids["owner"])
        .bind(category)
        .bind(w.normal)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
        for (by, t) in steps {
            let mut tx = w.db.pool.begin().await.unwrap();
            transition_with(&mut tx, id, by, &t, Some(policy))
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        let entry: Value = sqlx::query_scalar(
            "SELECT payload FROM ticket_events WHERE ticket_id = $1 AND to_status = 'queued'",
        )
        .bind(id)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
        assert_eq!(entry["route"], route);
        assert_eq!(w.placement(id).await, (None, None), "{route}");
        queued.push(id);
    }
    let general = w.queue("op", "general").await;
    assert_eq!(general.len(), before + queued.len());
    for id in &queued {
        assert_eq!(general.iter().filter(|g| *g == id).count(), 1);
    }
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    // Each dispatch moves the same ticket; none is created.
    for id in &queued {
        let r = w.change(*id, "outsider", w.dispatch(w.desk)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(w.placement(*id).await, (Some(w.desk), None));
    }
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(after, tickets);
    assert!(
        w.queue("op", "general")
            .await
            .iter()
            .all(|g| !queued.contains(g))
    );
    let team = w.queue("op", "team").await;
    assert!(queued.iter().all(|id| team.contains(id)));
}

/// Dispatch keeps the ticket's identity and history, records who moved it where,
/// and hands a dispatcher outside the team a receipt instead of the ticket.
#[tokio::test]
async fn dispatch_retains_context_records_the_change_and_drops_the_dispatcher() {
    let Some(w) = world().await else { return };
    let id = w.seed("owner", "in_progress", None, None).await;
    sqlx::query(
        "INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, created_at)
         VALUES ($1, $2, 'submit', 'draft', 'queued', now() - interval '2 days'),
                ($1, $3, 'start_work', 'queued', 'in_progress', now() - interval '1 day')",
    )
    .bind(id)
    .bind(w.ids["owner"])
    .bind(w.ids["admin"])
    .execute(&w.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ticket_messages (ticket_id, author_account_id, author_kind, kind, body) VALUES ($1, $2, 'requester', 'public_reply', 'Help')",
    )
    .bind(id)
    .bind(w.ids["owner"])
    .execute(&w.db.pool)
    .await
    .unwrap();
    let before = w.detail(id, "admin").await.body;

    let r = w.change(id, "outsider", w.dispatch(w.desk)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["reference"], before["reference"]);
    assert_eq!(r.body["team_name"], "Desk");
    assert_eq!(r.body["assignee_display_name"], Value::Null);
    assert_eq!(r.body["visible"], false);
    assert_eq!(w.detail(id, "outsider").await.code(), "not_found");
    assert!(!w.queue("outsider", "general").await.contains(&id));

    let after = w.detail(id, "op").await.body;
    for key in [
        "reference",
        "status",
        "subject",
        "description",
        "category_id",
        "priority_id",
        "intake_snapshot",
        "created_at",
        "transitions",
        "requester",
    ] {
        assert_eq!(after[key], before[key], "{key}");
    }
    assert_ne!(after["updated_at"], before["updated_at"]);
    assert_eq!(after["assignment"]["team_name"], "Desk");
    let messages = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{id}/messages"),
        None,
        Some(&w.cookies["op"]),
    )
    .await;
    assert_eq!(messages.body["items"].as_array().unwrap().len(), 1);
    // The audit trail: actor, previous and new team and assignee.
    let events = w.events(id).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, w.ids["outsider"]);
    assert_eq!(
        events[0].1,
        json!({ "action": "dispatch", "previous_team_id": null, "team_id": w.desk,
                "previous_assignee_account_id": null, "assignee_account_id": null })
    );
    let history = &after["assignment_changes"][0];
    assert_eq!(history["action"], "dispatch");
    assert_eq!(history["team_name"], "Desk");
    assert_eq!(history["actor_display_name"], "outsider");
    // The requester sees the team, no invented assignee, and no staff history.
    let own = w.detail(id, "owner").await.body;
    assert_eq!(
        own["assignment"],
        json!({ "team_name": "Desk", "assignee_display_name": null })
    );
    assert_eq!(own["assignment_changes"], json!([]));
    // A member who stays in the team keeps the ticket after dispatching.
    let back = w.change(id, "op", act("return_to_general")).await;
    assert_eq!(back.body["visible"], true);
    let again = w.change(id, "op", w.dispatch(w.desk)).await;
    assert_eq!(again.body["visible"], true);
}

/// Claim, then Lead/Admin reassignment: the old assignee turns read-only, the new
/// one gains write access, and nobody else takes over a claimed ticket.
#[tokio::test]
async fn claim_and_reassignment_follow_team_roles() {
    let Some(w) = world().await else { return };
    let id = w.seed("owner", "queued", Some(w.desk), None).await;
    // Admins never claim; outsiders cannot see the ticket.
    assert_eq!(w.change(id, "admin", act("claim")).await.status, 403);
    assert_eq!(w.change(id, "outsider", act("claim")).await.status, 404);
    assert_eq!(w.change(id, "net_lead", act("claim")).await.status, 404);
    // Ordinary members cannot assign.
    assert_eq!(w.change(id, "op", w.assign("peer")).await.status, 403);

    let r = w.change(id, "peer", act("claim")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["assignee_display_name"], "peer");
    assert_eq!(w.placement(id).await, (Some(w.desk), Some(w.ids["peer"])));
    // Another member cannot take it over or write on it.
    assert_eq!(w.change(id, "op", act("claim")).await.status, 403);
    assert_eq!(w.change(id, "op", act("unassign")).await.status, 403);
    assert_eq!(
        w.detail(id, "op").await.body["staff_access"]["can_handle"],
        false
    );

    // The lead reassigns to op: peer becomes read-only, op may write.
    let r = w.change(id, "lead", w.assign("op")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let peer = w.detail(id, "peer").await.body;
    assert_eq!(peer["staff_access"]["can_handle"], false);
    assert_eq!(
        w.detail(id, "op").await.body["staff_access"]["can_handle"],
        true
    );
    let notes = format!("/api/tickets/{id}/notes");
    let note = |who: &'static str| {
        call(
            &w.app,
            "POST",
            &notes,
            Some(json!({ "body": "x", "idempotency_key": Uuid::new_v4() })),
            Some(&w.cookies[who]),
        )
    };
    assert_eq!(note("peer").await.status, 403);
    assert_eq!(note("op").await.status, 201);

    // Admin manages across teams; the lead's affordances list eligible members only.
    let lead_view = w.detail(id, "lead").await.body;
    let access = &lead_view["staff_access"];
    assert_eq!(access["can_assign"], true);
    assert_eq!(access["can_claim"], false);
    let names: Vec<&str> = access["assignees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["lead", "peer"]);
    let r = w.change(id, "admin", w.assign("lead")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = w.change(id, "admin", act("unassign")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(w.placement(id).await, (Some(w.desk), None));
    assert_eq!(w.change(id, "lead", act("unassign")).await.status, 409);
    // Another team's lead sees nothing.
    assert_eq!(w.change(id, "net_lead", w.assign("op")).await.status, 404);

    let actions: Vec<(Uuid, String)> = w
        .events(id)
        .await
        .into_iter()
        .map(|(actor, p)| (actor, p["action"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(
        actions,
        [
            (w.ids["peer"], "claim".into()),
            (w.ids["lead"], "assign".into()),
            (w.ids["admin"], "assign".into()),
            (w.ids["admin"], "unassign".into()),
        ]
    );
    let events = w.events(id).await;
    assert_eq!(
        events[1].1["previous_assignee_account_id"],
        json!(w.ids["peer"])
    );
    assert_eq!(events[1].1["assignee_account_id"], json!(w.ids["op"]));
}

/// Only active staff members of the ticket's own team qualify; never an Admin,
/// the requester, an inactive account, or another team's member.
#[tokio::test]
async fn invalid_and_disabled_targets_are_rejected_without_writing() {
    let Some(w) = world().await else { return };
    let id = w.seed("owner", "queued", Some(w.desk), Some("peer")).await;
    for who in [
        "outsider", "net_op", "admin", "admin2", "gone", "owner", "peer",
    ] {
        let r = w.change(id, "lead", w.assign(who)).await;
        assert_eq!(r.status, 422, "{who}: {}", r.body);
        assert_eq!(
            r.body["error"]["details"]["fields"]["assignee_account_id"][0],
            "invalid"
        );
    }
    let nowhere = w
        .change(
            id,
            "lead",
            json!({ "action": "assign", "assignee_account_id": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(nowhere.status, 422);

    let general = w.seed("owner", "queued", None, None).await;
    let archived: Uuid = sqlx::query_scalar(
        "INSERT INTO teams (name, archived_at, lead_account_id, lead_role, lead_active)
         VALUES ('Old', now(), NULL, NULL, NULL) RETURNING id",
    )
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    for team in [archived, Uuid::new_v4()] {
        let r = w.change(general, "op", w.dispatch(team)).await;
        assert_eq!(r.status, 422, "{}", r.body);
        assert_eq!(
            r.body["error"]["details"]["fields"]["team_id"][0],
            "invalid"
        );
    }
    // Same team again, nothing to return, nothing to unassign.
    assert_eq!(w.change(id, "lead", w.dispatch(w.desk)).await.status, 422);
    assert_eq!(
        w.change(general, "op", act("return_to_general"))
            .await
            .status,
        409
    );
    assert_eq!(
        w.change(general, "admin", act("unassign")).await.status,
        403
    );
    // The requester (a User) only ever gets the requester view.
    assert_eq!(
        w.change(general, "owner", w.dispatch(w.desk)).await.status,
        403
    );
    for t in [id, general] {
        assert!(w.events(t).await.is_empty());
    }
    assert_eq!(w.placement(id).await, (Some(w.desk), Some(w.ids["peer"])));
}

/// Transfer to another team clears the assignee and moves visibility at once;
/// returning to intake reopens it to every operator.
#[tokio::test]
async fn transfer_and_return_move_visibility_everywhere() {
    let Some(w) = world().await else { return };
    let id = w
        .seed("owner", "in_progress", Some(w.desk), Some("op"))
        .await;
    // A read-only peer of a claimed ticket cannot transfer it; the assignee can.
    assert_eq!(
        w.change(id, "peer", w.dispatch(w.network)).await.status,
        403
    );
    let r = w.change(id, "op", w.dispatch(w.network)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["visible"], false);
    assert_eq!(w.placement(id).await, (Some(w.network), None));
    for who in ["op", "peer", "lead"] {
        assert_eq!(w.detail(id, who).await.code(), "not_found", "{who}");
        for view in ["general", "team", "mine"] {
            assert!(!w.queue(who, view).await.contains(&id), "{who} {view}");
        }
        let r = call(
            &w.app,
            "GET",
            &format!("/api/tickets/{id}/messages"),
            None,
            Some(&w.cookies[who]),
        )
        .await;
        assert_eq!(r.code(), "not_found");
    }
    assert!(w.queue("net_op", "team").await.contains(&id));
    assert_eq!(
        w.detail(id, "net_op").await.body["staff_access"]["can_claim"],
        true
    );

    let r = w.change(id, "net_lead", act("return_to_general")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["visible"], true);
    assert_eq!(w.placement(id).await, (None, None));
    for who in ["op", "outsider", "net_op"] {
        assert!(w.queue(who, "general").await.contains(&id), "{who}");
    }
    let events = w.events(id).await;
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].1["previous_assignee_account_id"],
        json!(w.ids["op"])
    );
    assert_eq!(events[0].1["previous_team_id"], json!(w.desk));
    assert_eq!(events[0].1["team_id"], json!(w.network));
    assert_eq!(events[1].1["action"], "return_to_general");
}

/// Concurrent claims and transfers: exactly one wins, the others get `409` (or
/// `404` once they can no longer see the ticket), and one event is written.
#[tokio::test]
async fn concurrent_claims_and_transfers_have_one_winner() {
    let Some(w) = world().await else { return };
    let id = w.seed("owner", "queued", Some(w.desk), None).await;
    let at = w.updated_at(id).await;
    let (a, b, c) = tokio::join!(
        w.change_at(id, "op", act("claim"), at.clone()),
        w.change_at(id, "peer", act("claim"), at.clone()),
        w.change_at(id, "lead", act("claim"), at.clone()),
    );
    let mut codes = [a.status.as_u16(), b.status.as_u16(), c.status.as_u16()];
    codes.sort();
    assert_eq!(codes, [200, 409, 409]);
    assert_eq!(w.events(id).await.len(), 1);

    let general = w.seed("owner", "queued", None, None).await;
    let at = w.updated_at(general).await;
    let (a, b) = tokio::join!(
        w.change_at(general, "outsider", w.dispatch(w.desk), at.clone()),
        w.change_at(general, "op", w.dispatch(w.network), at.clone()),
    );
    let ok = [&a, &b].iter().filter(|r| r.status == 200).count();
    assert_eq!(ok, 1, "{} {}", a.body, b.body);
    for r in [&a, &b] {
        assert!([200, 404, 409].contains(&r.status.as_u16()), "{}", r.body);
    }
    assert_eq!(w.events(general).await.len(), 1);
}

/// A change based on an older load is refused and writes nothing; a retry with
/// the current `updated_at` succeeds.
#[tokio::test]
async fn stale_requests_conflict_and_retries_succeed() {
    let Some(w) = world().await else { return };
    let id = w.seed("owner", "queued", None, None).await;
    let old = w.updated_at(id).await;
    sqlx::query("UPDATE tickets SET updated_at = clock_timestamp() WHERE id = $1")
        .bind(id)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = w.change_at(id, "op", w.dispatch(w.desk), old.clone()).await;
    assert_eq!(r.status, 409);
    assert_eq!(r.code(), "conflict");
    assert_eq!(w.placement(id).await, (None, None));
    assert!(w.events(id).await.is_empty());
    let r = w.change(id, "op", w.dispatch(w.desk)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    // Replaying the first, now stale, request does not dispatch twice.
    assert_eq!(
        w.change_at(id, "op", w.dispatch(w.network), old)
            .await
            .status,
        409
    );
    assert_eq!(w.events(id).await.len(), 1);
}

/// Staff never dispatch, claim, or manage a ticket they submitted; a separate
/// Admin account of the same person (shared email) still may.
#[tokio::test]
async fn self_handling_is_refused_by_account_id() {
    let Some(w) = world().await else { return };
    let own_general = w.seed("op", "queued", None, None).await;
    assert_eq!(
        w.change(own_general, "op", w.dispatch(w.desk)).await.status,
        403
    );
    let own = w.seed("lead", "queued", Some(w.desk), None).await;
    assert_eq!(w.change(own, "lead", act("claim")).await.status, 403);
    assert_eq!(w.change(own, "lead", w.assign("op")).await.status, 403);
    assert_eq!(
        w.change(own, "lead", act("return_to_general")).await.status,
        403
    );
    // Another account (same shared email) handles it; the requester is never a target.
    let r = w.change(own, "admin", w.assign("lead")).await;
    assert_eq!(r.status, 422);
    let r = w.change(own, "admin", w.assign("op")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = w.change(own_general, "outsider", w.dispatch(w.desk)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(w.change(own_general, "op", act("claim")).await.status, 403);
}

/// AI-solved and finished records never enter queues and cannot be assigned,
/// dispatched, or reopened through assignment.
#[tokio::test]
async fn ai_solved_and_finished_records_are_excluded() {
    let Some(w) = world().await else { return };
    let solved = w.seed("owner", "ai_solved", None, None).await;
    let closed = w.seed("owner", "closed", Some(w.desk), Some("op")).await;
    for view in ["general", "team", "all"] {
        assert!(!w.queue("admin", view).await.contains(&solved), "{view}");
    }
    assert_eq!(w.change(solved, "op", w.dispatch(w.desk)).await.status, 404);
    for action in [w.dispatch(w.desk), w.assign("op"), act("unassign")] {
        assert_eq!(w.change(solved, "admin", action).await.status, 403);
    }
    for (who, action) in [
        ("op", w.dispatch(w.network)),
        ("op", act("return_to_general")),
        ("lead", w.assign("peer")),
        ("admin", act("unassign")),
    ] {
        assert_eq!(w.change(closed, who, action).await.status, 403, "{who}");
    }
    assert_eq!(w.placement(solved).await, (None, None));
    assert_eq!(w.placement(closed).await, (Some(w.desk), Some(w.ids["op"])));
    let access = &w.detail(closed, "lead").await.body["staff_access"];
    assert_eq!(access["can_dispatch"], false);
    assert_eq!(access["can_assign"], false);
    // The database refuses it too.
    let r = sqlx::query("UPDATE tickets SET team_id = $1 WHERE id = $2")
        .bind(w.desk)
        .bind(solved)
        .execute(&w.db.pool)
        .await;
    assert!(r.is_err());
}
