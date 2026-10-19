//! Staff ticket handling over HTTP: status work, proposals, reopening, priority,
//! stale writes, claimed-ticket rules, self-handling, and team isolation.
use axum::Router;
use serde_json::{Value, json};
use uuid::Uuid;

mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

struct World {
    db: TestDb,
    app: Router,
    owner: Uuid,
    op: Uuid,
    category: Uuid,
    normal: Uuid,
    urgent: Uuid,
    team: Uuid,
    cookies: std::collections::HashMap<&'static str, String>,
}

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    fixtures::bootstrap_admin(p).await;
    let owner = fixtures::account(p, Role::User, "owner").await;
    let op = fixtures::account(p, Role::Operator, "op").await;
    let peer = fixtures::account(p, Role::Specialist, "peer").await;
    let lead = fixtures::account(p, Role::TeamLead, "lead").await;
    fixtures::account(p, Role::Operator, "outsider").await;
    let other_lead = fixtures::account(p, Role::TeamLead, "other_lead").await;
    let team = fixtures::team(p, "Desk", lead).await.unwrap();
    fixtures::team(p, "Network", other_lead).await.unwrap();
    fixtures::member(p, team, op).await.unwrap();
    fixtures::member(p, team, peer).await.unwrap();
    let id = |sql: &'static str| sqlx::query_scalar(sql).fetch_one(p);
    let category = id("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
        .await
        .unwrap();
    let normal = id("INSERT INTO priorities (name, sort_order) VALUES ('Normal', 2) RETURNING id")
        .await
        .unwrap();
    let urgent = id("INSERT INTO priorities (name, sort_order) VALUES ('Urgent', 1) RETURNING id")
        .await
        .unwrap();
    let app = test_app(p);
    let mut cookies = std::collections::HashMap::new();
    for name in [
        "owner",
        "op",
        "peer",
        "lead",
        "outsider",
        "other_lead",
        "admin",
    ] {
        cookies.insert(name, login(&app, name, PASSWORD).await.session());
    }
    Some(World {
        db,
        app,
        owner,
        op,
        category,
        normal,
        urgent,
        team,
        cookies,
    })
}

impl World {
    /// A Desk ticket in `status`, claimed by `op` when `claimed`.
    async fn seed(&self, requester: Uuid, status: &str, claimed: bool) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description,
                status, team_id, assignee_account_id, proposal_version, confirmation_deadline,
                resolution_source, closure_reason)
             VALUES ($1,$2,$3,'Printer','Jams',$4,$5,CASE WHEN $6 THEN $7 END,
               CASE WHEN $4 IN ('awaiting_confirmation','closed') THEN 1 ELSE 0 END,
               CASE WHEN $4 = 'awaiting_confirmation' THEN now() + interval '168 hours' END,
               CASE WHEN $4 = 'closed' THEN 'operator' END,
               CASE WHEN $4 = 'closed' THEN 'requester_confirmed' END)
             RETURNING id",
        )
        .bind(requester)
        .bind(self.category)
        .bind(self.normal)
        .bind(status)
        .bind(self.team)
        .bind(claimed)
        .bind(self.op)
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
    }
    async fn detail(&self, id: Uuid, who: &str) -> support::Reply {
        call(
            &self.app,
            "GET",
            &format!("/api/tickets/{id}"),
            None,
            Some(&self.cookies[who]),
        )
        .await
    }
    /// Staff action with the ticket's current `updated_at`, as a freshly loaded page sends it.
    async fn act(&self, id: Uuid, who: &str, action: Value) -> support::Reply {
        let loaded = self.detail(id, who).await;
        let at = loaded.body["updated_at"].clone();
        self.act_at(id, who, action, at).await
    }
    async fn act_at(&self, id: Uuid, who: &str, mut action: Value, at: Value) -> support::Reply {
        action["expected_updated_at"] = at;
        call(
            &self.app,
            "POST",
            &format!("/api/tickets/{id}/staff-actions"),
            Some(action),
            Some(&self.cookies[who]),
        )
        .await
    }
    async fn status(&self, id: Uuid) -> String {
        sqlx::query_scalar("SELECT status::text FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_one(&self.db.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn staff_work_wait_propose_and_supersede_without_closing() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "queued", false).await;
    let d = w.detail(id, "op").await.body;
    assert_eq!(d["view"], "staff");
    assert_eq!(
        d["staff_access"],
        json!({"can_handle": true, "can_reopen": false})
    );
    assert_eq!(d["requester"]["username"], "owner");

    let r = w.act(id, "op", json!({"action":"start_work"})).await;
    assert_eq!(r.body, json!({"status":"in_progress"}));
    let r = w
        .act(id, "op", json!({"action":"request_information"}))
        .await;
    assert_eq!(r.body["status"], "waiting_for_requester");
    assert_eq!(
        w.act(id, "op", json!({"action":"start_work"})).await.body["status"],
        "in_progress"
    );

    for summary in ["", "   ", &"x".repeat(5001)] {
        let r = w
            .act(
                id,
                "op",
                json!({"action":"propose_resolution","summary":summary}),
            )
            .await;
        assert_eq!(r.status, 422, "{summary:?}");
        assert_eq!(w.status(id).await, "in_progress");
    }
    let r = w
        .act(
            id,
            "op",
            json!({"action":"propose_resolution","summary":"Replaced the roller"}),
        )
        .await;
    assert_eq!(r.body["status"], "awaiting_confirmation");
    let d = w.detail(id, "op").await.body;
    assert_eq!(d["proposal_version"], 1);
    assert!(d["confirmation_deadline"].is_string());
    let last = d["transitions"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["event_type"], "propose_resolution");
    assert_eq!(last["actor_display_name"], "op");
    assert_eq!(last["payload"]["summary"], "Replaced the roller");

    // A new proposal supersedes the pending one with its own version.
    w.act(
        id,
        "peer",
        json!({"action":"propose_resolution","summary":"Different fix"}),
    )
    .await;
    assert_eq!(w.detail(id, "op").await.body["proposal_version"], 2);

    // No staff path closes the ticket or fakes the requester's acceptance.
    for action in ["accept_resolution", "close", "reject_resolution"] {
        let r = w.act(id, "op", json!({"action": action})).await;
        assert!(r.status.is_client_error(), "{action}: {}", r.status);
    }
    assert_eq!(w.status(id).await, "awaiting_confirmation");
    // The requester's view hides staff identities.
    let d = w.detail(id, "owner").await.body;
    assert_eq!(d["view"], "requester");
    assert!(d["staff_access"].is_null());
    assert!(d["transitions"][0]["actor_display_name"].is_null());
}

#[tokio::test]
async fn stale_writes_conflict_and_leave_state_untouched() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "in_progress", false).await;
    let stale = w.detail(id, "op").await.body["updated_at"].clone();
    // A newer public reply moves the ticket on; priority changes do too.
    let reply = call(
        &w.app,
        "POST",
        &format!("/api/tickets/{id}/replies"),
        Some(json!({"idempotency_key": Uuid::new_v4(), "body": "Any news?"})),
        Some(&w.cookies["owner"]),
    )
    .await;
    assert_eq!(reply.status, 201);
    for action in [
        json!({"action":"request_information"}),
        json!({"action":"propose_resolution","summary":"Done"}),
        json!({"action":"set_priority","priority_id": w.urgent}),
    ] {
        let r = w.act_at(id, "op", action, stale.clone()).await;
        assert_eq!((r.status.as_u16(), r.code()), (409, "conflict"));
    }
    assert_eq!(w.status(id).await, "in_progress");
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ticket_events WHERE ticket_id = $1 AND event_type IN ('priority_changed','request_information','propose_resolution')",
    )
    .bind(id)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(events, 0);
    // Two staff acting on the same loaded page: the second is stale.
    let loaded = w.detail(id, "op").await.body["updated_at"].clone();
    let first = w
        .act_at(
            id,
            "op",
            json!({"action":"request_information"}),
            loaded.clone(),
        )
        .await;
    assert_eq!(first.status, 200);
    let second = w
        .act_at(id, "peer", json!({"action":"start_work"}), loaded)
        .await;
    assert_eq!(second.status, 409);
    assert_eq!(w.status(id).await, "waiting_for_requester");
}

#[tokio::test]
async fn claimed_tickets_are_writable_only_by_assignee_lead_and_admin() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "in_progress", true).await;
    let d = w.detail(id, "peer").await.body;
    assert_eq!(d["staff_access"]["can_handle"], false);
    assert_eq!(d["assignment"]["assignee_display_name"], "op");
    for action in [
        json!({"action":"request_information"}),
        json!({"action":"propose_resolution","summary":"Fixed"}),
        json!({"action":"set_priority","priority_id": w.urgent}),
    ] {
        assert_eq!(w.act(id, "peer", action).await.status, 403);
    }
    assert_eq!(w.status(id).await, "in_progress");
    for who in ["op", "lead", "admin"] {
        assert_eq!(
            w.detail(id, who).await.body["staff_access"]["can_handle"],
            true,
            "{who}"
        );
    }
    assert_eq!(
        w.act(id, "lead", json!({"action":"request_information"}))
            .await
            .status,
        200
    );
    assert_eq!(
        w.act(id, "admin", json!({"action":"start_work"}))
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn isolation_general_queue_and_finished_tickets() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "in_progress", false).await;
    for who in ["outsider", "other_lead"] {
        // Dispatched to Desk: another team's staff cannot learn it exists.
        assert_eq!(w.detail(id, who).await.status, 404, "{who}");
        let r = call(
            &w.app,
            "POST",
            &format!("/api/tickets/{id}/staff-actions"),
            Some(json!({"action":"request_information","expected_updated_at":"2026-01-01T00:00:00Z"})),
            Some(&w.cookies[who]),
        )
        .await;
        assert_eq!(r.status, 404, "{who}");
    }
    // Shared intake is dispatch-only: no handling there, for any role.
    let general: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description, status)
         VALUES ($1,$2,$3,'Intake','x','queued') RETURNING id",
    )
    .bind(w.owner)
    .bind(w.category)
    .bind(w.normal)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    for who in ["op", "admin"] {
        let d = w.detail(general, who).await.body;
        assert_eq!(d["staff_access"]["can_handle"], false, "{who}");
        assert_eq!(
            w.act(general, who, json!({"action":"start_work"}))
                .await
                .status,
            403
        );
        assert_eq!(
            w.act(
                general,
                who,
                json!({"action":"set_priority","priority_id": w.urgent})
            )
            .await
            .status,
            403
        );
    }
    // Finished tickets accept no status work or priority change.
    let closed = w.seed(w.owner, "closed", false).await;
    assert_eq!(
        w.act(closed, "op", json!({"action":"start_work"}))
            .await
            .status,
        403
    );
    assert_eq!(
        w.act(
            closed,
            "op",
            json!({"action":"set_priority","priority_id": w.urgent})
        )
        .await
        .status,
        403
    );
}

#[tokio::test]
async fn self_handling_is_denied_by_account_id_only() {
    let Some(w) = world().await else { return };
    // `op` submitted this ticket to its own team: requester view, no staff handling.
    let own = w.seed(w.op, "in_progress", false).await;
    let d = w.detail(own, "op").await.body;
    assert_eq!(d["view"], "requester");
    assert!(d["staff_access"].is_null());
    assert!(d["requester"].is_null());
    for action in [
        json!({"action":"request_information"}),
        json!({"action":"propose_resolution","summary":"Self-resolved"}),
        json!({"action":"set_priority","priority_id": w.urgent}),
    ] {
        assert_eq!(w.act(own, "op", action).await.status, 403);
    }
    assert_eq!(w.status(own).await, "in_progress");
    // Another account (the lead, the Admin, a peer) handles it; email plays no part.
    for (who, level) in [("peer", w.urgent), ("lead", w.normal), ("admin", w.urgent)] {
        let action = json!({"action":"set_priority","priority_id": level});
        assert_eq!(w.act(own, who, action).await.status, 200, "{who}");
    }
    // The requester of an ordinary ticket cannot use staff actions either.
    let theirs = w.seed(w.owner, "in_progress", false).await;
    assert_eq!(
        w.act(theirs, "owner", json!({"action":"request_information"}))
            .await
            .status,
        403
    );
}

#[tokio::test]
async fn priority_changes_validate_and_keep_history() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "in_progress", false).await;
    let bad = [
        Uuid::new_v4(),
        w.normal, // unchanged
    ];
    for priority_id in bad {
        let r = w
            .act(
                id,
                "op",
                json!({"action":"set_priority","priority_id":priority_id}),
            )
            .await;
        assert_eq!(r.status, 422, "{priority_id}");
        assert_eq!(
            r.body["error"]["details"]["fields"]["priority_id"][0],
            "invalid"
        );
    }
    let archived: Uuid = sqlx::query_scalar(
        "INSERT INTO priorities (name, archived_at) VALUES ('Old', now()) RETURNING id",
    )
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(
        w.act(
            id,
            "op",
            json!({"action":"set_priority","priority_id":archived})
        )
        .await
        .status,
        422
    );
    let before = w.detail(id, "op").await.body["updated_at"].clone();
    let r = w
        .act(
            id,
            "op",
            json!({"action":"set_priority","priority_id":w.urgent}),
        )
        .await;
    assert_eq!(r.body["status"], "in_progress");
    let r = w
        .act(
            id,
            "op",
            json!({"action":"set_priority","priority_id":w.normal}),
        )
        .await;
    assert_eq!(r.status, 200);
    let d = w.detail(id, "lead").await.body;
    assert_ne!(d["updated_at"], before);
    assert_eq!(d["priority_name"], "Normal");
    let changes = d["priority_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["previous_priority_name"], "Normal");
    assert_eq!(changes[0]["priority_name"], "Urgent");
    assert_eq!(changes[0]["actor_display_name"], "op");
    assert_eq!(changes[1]["priority_name"], "Normal");
    // Requesters see the effective priority and its history, never who changed it.
    let d = w.detail(id, "owner").await.body;
    assert_eq!(d["priority_name"], "Normal");
    assert_eq!(d["priority_changes"].as_array().unwrap().len(), 2);
    assert!(d["priority_changes"][0]["actor_account_id"].is_null());
    assert!(d["priority_changes"][0]["actor_display_name"].is_null());
    // The requester's original choice stays frozen elsewhere; the event log is append-only.
    let rewrite = sqlx::query(
        "UPDATE ticket_events SET payload = '{}' WHERE event_type = 'priority_changed'",
    )
    .execute(&w.db.pool)
    .await;
    assert!(rewrite.is_err());
}

#[tokio::test]
async fn reopening_follows_the_admin_toggle_and_never_touches_ai_records() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "closed", true).await;
    assert_eq!(
        w.detail(id, "op").await.body["staff_access"]["can_reopen"],
        false
    );
    assert_eq!(
        w.act(id, "op", json!({"action":"reopen"})).await.status,
        403
    );
    sqlx::query("UPDATE organization SET allow_reopening = true")
        .execute(&w.db.pool)
        .await
        .unwrap();
    // A claimed ticket is reopened only by its assignee, the lead, or an Admin.
    assert_eq!(
        w.detail(id, "peer").await.body["staff_access"]["can_reopen"],
        false
    );
    assert_eq!(
        w.act(id, "peer", json!({"action":"reopen"})).await.status,
        403
    );
    assert_eq!(
        w.detail(id, "op").await.body["staff_access"]["can_reopen"],
        true
    );
    // A page loaded while reopening was on is safe after revocation: the write is denied.
    let loaded = w.detail(id, "op").await.body["updated_at"].clone();
    sqlx::query("UPDATE organization SET allow_reopening = false")
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(
        w.act_at(id, "op", json!({"action":"reopen"}), loaded)
            .await
            .status,
        403
    );
    sqlx::query("UPDATE organization SET allow_reopening = true")
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = w.act(id, "op", json!({"action":"reopen"})).await;
    assert_eq!(r.body["status"], "in_progress");
    assert_eq!(
        w.act(id, "op", json!({"action":"reopen"})).await.status,
        409
    );

    // AI-solved records: invisible to operators, final for everyone including Admins.
    let ai: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description,
            status, resolution_source, closure_reason)
         VALUES ($1,$2,$3,'Solved by AI','x','ai_solved','ai','requester_confirmed') RETURNING id",
    )
    .bind(w.owner)
    .bind(w.category)
    .bind(w.normal)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(w.detail(ai, "op").await.status, 404);
    assert_eq!(
        w.act_at(
            ai,
            "op",
            json!({"action":"reopen"}),
            json!("2026-01-01T00:00:00Z")
        )
        .await
        .status,
        404
    );
    assert_eq!(
        w.act(ai, "admin", json!({"action":"reopen"})).await.status,
        403
    );
    assert_eq!(w.status(ai).await, "ai_solved");
}

#[tokio::test]
async fn cancellation_reason_actor_and_time_are_visible_to_staff() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "in_progress", true).await;
    let cancel = call(
        &w.app,
        "POST",
        &format!("/api/tickets/{id}/actions"),
        Some(json!({"action":"cancel","reason":"Fixed myself"})),
        Some(&w.cookies["owner"]),
    )
    .await;
    assert_eq!(cancel.status, 200);
    let d = w.detail(id, "peer").await.body;
    assert_eq!(d["status"], "cancelled");
    assert_eq!(d["staff_access"]["can_handle"], false);
    let last = d["transitions"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["event_type"], "cancel");
    assert_eq!(last["actor_display_name"], "owner");
    assert_eq!(last["payload"]["reason"], "Fixed myself");
    assert!(last["created_at"].is_string());
    assert_eq!(
        w.act(id, "op", json!({"action":"start_work"})).await.status,
        403
    );
}
