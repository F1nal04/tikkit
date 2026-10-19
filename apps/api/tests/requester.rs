//! Requester HTTP decisions, history filters, and final AI history against PostgreSQL.
use axum::Router;
use serde_json::{Value, json};
use tikkit_api::lifecycle::{self, By, Transition};
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
    other: Uuid,
    op: Uuid,
    team: Uuid,
    category: Uuid,
    priority: Uuid,
    cookie: String,
    other_cookie: String,
}
async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    fixtures::bootstrap_admin(p).await;
    let owner = fixtures::account(p, Role::User, "owner").await;
    let other = fixtures::account(p, Role::User, "other").await;
    let op = fixtures::account(p, Role::Operator, "op").await;
    let lead = fixtures::account(p, Role::TeamLead, "lead").await;
    let team = fixtures::team(p, "Desk", lead).await.unwrap();
    fixtures::member(p, team, op).await.unwrap();
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
    let app = test_app(p);
    let cookie = login(&app, "owner", PASSWORD).await.session();
    let other_cookie = login(&app, "other", PASSWORD).await.session();
    Some(World {
        db,
        app,
        owner,
        other,
        op,
        team,
        category,
        priority,
        cookie,
        other_cookie,
    })
}
impl World {
    async fn seed(&self, owner: Uuid, status: &str) -> Uuid {
        sqlx::query_scalar("INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description, status, team_id, assignee_account_id, proposal_version, confirmation_deadline, resolution_source, closure_reason, ai_solution)
          VALUES ($1,$2,$3,'Original problem','Frozen description',$4,
          CASE WHEN $4 IN ('in_progress','awaiting_confirmation','closed') THEN $5 END,
          CASE WHEN $4 IN ('in_progress','awaiting_confirmation','closed') THEN $6 END,
          CASE WHEN $4 IN ('awaiting_confirmation','closed') THEN 1 ELSE 0 END,
          CASE WHEN $4 = 'awaiting_confirmation' THEN now() + interval '168 hours' END,
          CASE $4 WHEN 'closed' THEN 'operator' WHEN 'ai_solved' THEN 'ai' END,
          CASE WHEN $4 IN ('closed','ai_solved') THEN 'requester_confirmed' END,
          CASE WHEN $4 = 'ai_solved' THEN 'Restart the printer. <script>literal</script>' END) RETURNING id")
            .bind(owner).bind(self.category).bind(self.priority).bind(status).bind(self.team).bind(self.op).fetch_one(&self.db.pool).await.unwrap()
    }
    async fn action(&self, id: Uuid, body: Value, cookie: &str) -> support::Reply {
        call(
            &self.app,
            "POST",
            &format!("/api/tickets/{id}/actions"),
            Some(body),
            Some(cookie),
        )
        .await
    }
    async fn transition(&self, id: Uuid, actor: Uuid, t: Transition) {
        let mut tx = self.db.pool.begin().await.unwrap();
        lifecycle::transition(&mut tx, id, By::Account(actor), &t)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    async fn detail(&self, id: Uuid, cookie: &str) -> support::Reply {
        call(
            &self.app,
            "GET",
            &format!("/api/tickets/{id}"),
            None,
            Some(cookie),
        )
        .await
    }
}

#[tokio::test]
async fn requester_decisions_are_versioned_owned_and_preserve_support_workflow() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "in_progress").await;
    w.transition(
        id,
        w.op,
        Transition::ProposeResolution {
            summary: "Reinstalled the driver".into(),
        },
    )
    .await;
    for action in [
        json!({"action":"accept_resolution","proposal_version":1}),
        json!({"action":"reject_resolution","proposal_version":1}),
        json!({"action":"cancel","reason":"No longer needed"}),
        json!({"action":"reopen"}),
    ] {
        assert_eq!(w.action(id, action, &w.other_cookie).await.status, 404);
    }
    assert_eq!(
        w.action(
            id,
            json!({"action":"accept_resolution","proposal_version":0}),
            &w.cookie
        )
        .await
        .status,
        409
    );
    let rejected = w
        .action(
            id,
            json!({"action":"reject_resolution","proposal_version":1}),
            &w.cookie,
        )
        .await;
    assert_eq!(rejected.body, json!({"status":"in_progress"}));
    let d = w.detail(id, &w.cookie).await.body;
    assert_eq!(
        d["assignment"],
        json!({"team_name":"Desk","assignee_display_name":"op"})
    );
    assert_eq!(
        d["transitions"][0]["payload"]["summary"],
        "Reinstalled the driver"
    );
    assert!(
        d["transitions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["actor_account_id"].is_null())
    );
    assert_eq!(
        w.action(
            id,
            json!({"action":"reject_resolution","proposal_version":1}),
            &w.cookie
        )
        .await
        .status,
        409
    );
    w.transition(
        id,
        w.op,
        Transition::ProposeResolution {
            summary: "Replaced cable".into(),
        },
    )
    .await;
    assert_eq!(
        w.action(
            id,
            json!({"action":"accept_resolution","proposal_version":1}),
            &w.cookie
        )
        .await
        .status,
        409
    );
    assert_eq!(
        w.action(
            id,
            json!({"action":"accept_resolution","proposal_version":2}),
            &w.cookie
        )
        .await
        .body["status"],
        "closed"
    );
    let d = w.detail(id, &w.cookie).await.body;
    assert_eq!(d["closure_reason"], "requester_confirmed");
    assert_eq!(
        w.action(id, json!({"action":"reopen"}), &w.cookie)
            .await
            .status,
        403
    );
    sqlx::query("UPDATE organization SET allow_reopening = true")
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(
        w.action(id, json!({"action":"reopen"}), &w.cookie)
            .await
            .body["status"],
        "in_progress"
    );
    assert_eq!(
        w.action(id, json!({"action":"reopen"}), &w.cookie)
            .await
            .status,
        409
    );
}

#[tokio::test]
async fn cancellation_policy_is_rechecked_and_uses_permanent_work_history() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "queued").await;
    for reason in ["", "   ", &"x".repeat(1001)] {
        assert_eq!(
            w.action(id, json!({"action":"cancel","reason":reason}), &w.cookie)
                .await
                .status,
            422
        );
    }
    sqlx::query("UPDATE organization SET requester_cancellation_policy = 'disabled'")
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(
        w.action(id, json!({"action":"cancel","reason":"Done"}), &w.cookie)
            .await
            .status,
        403
    );
    sqlx::query("UPDATE organization SET requester_cancellation_policy = 'before_work_starts'")
        .execute(&w.db.pool)
        .await
        .unwrap();
    let started = w.seed(w.owner, "in_progress").await;
    // Actual transitions, then unassign: work history still prevents cancellation.
    w.transition(started, w.op, Transition::RequestInformation)
        .await;
    w.transition(started, w.owner, Transition::RequesterResponded)
        .await;
    sqlx::query("UPDATE tickets SET assignee_account_id = NULL WHERE id = $1")
        .bind(started)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(
        w.action(
            started,
            json!({"action":"cancel","reason":"Done"}),
            &w.cookie
        )
        .await
        .status,
        403
    );
    assert_eq!(
        w.action(
            id,
            json!({"action":"cancel","reason":" No longer needed "}),
            &w.cookie
        )
        .await
        .body["status"],
        "cancelled"
    );
    let d = w.detail(id, &w.cookie).await.body;
    assert_eq!(d["transitions"][0]["payload"]["reason"], "No longer needed");
    assert_eq!(
        w.action(id, json!({"action":"cancel","reason":"Again"}), &w.cookie)
            .await
            .status,
        409
    );
    sqlx::query("UPDATE organization SET requester_cancellation_policy = 'anytime_while_open'")
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(
        w.action(
            started,
            json!({"action":"cancel","reason":"Done"}),
            &w.cookie
        )
        .await
        .body["status"],
        "cancelled"
    );
}

#[tokio::test]
async fn ai_history_is_private_final_and_cannot_change_counts_or_resolution_provenance() {
    let Some(w) = world().await else { return };
    let id = w.seed(w.owner, "ai_solved").await;
    sqlx::query("UPDATE organization SET allow_reopening = true")
        .execute(&w.db.pool)
        .await
        .unwrap();
    let before = w.detail(id, &w.cookie).await.body;
    assert!(
        before["ai_solution"]
            .as_str()
            .unwrap()
            .contains("Restart the printer")
    );
    assert_eq!(w.detail(id, &w.other_cookie).await.status, 404);
    let op_cookie = login(&w.app, "op", PASSWORD).await.session();
    assert_eq!(w.detail(id, &op_cookie).await.status, 404);
    let counts = call(&w.app, "GET", "/api/queue/counts", None, Some(&op_cookie))
        .await
        .body;
    for action in [
        json!({"action":"reopen"}),
        json!({"action":"cancel","reason":"Escalate"}),
        json!({"action":"accept_resolution","proposal_version":1}),
        json!({"action":"reject_resolution","proposal_version":1}),
    ] {
        assert_eq!(w.action(id, action, &w.cookie).await.status, 409);
    }
    assert_eq!(
        w.action(id, json!({"action":"ai_not_solved"}), &w.cookie)
            .await
            .status,
        422
    );
    let reply = call(
        &w.app,
        "POST",
        &format!("/api/tickets/{id}/replies"),
        Some(json!({"idempotency_key":Uuid::new_v4(),"body":"Escalate"})),
        Some(&w.cookie),
    )
    .await;
    assert_eq!(reply.status, 409);
    assert_eq!(w.detail(id, &w.cookie).await.body, before);
    assert_eq!(
        call(&w.app, "GET", "/api/queue/counts", None, Some(&op_cookie))
            .await
            .body,
        counts
    );
    assert!(
        sqlx::query("UPDATE tickets SET ai_solution = 'Replacement' WHERE id = $1")
            .bind(id)
            .execute(&w.db.pool)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn history_filters_apply_ownership_dates_and_bounded_pagination_in_sql() {
    let Some(w) = world().await else { return };
    let a = w.seed(w.owner, "ai_solved").await;
    w.seed(w.owner, "queued").await;
    w.seed(w.owner, "ai_solved").await;
    w.seed(w.other, "ai_solved").await;
    sqlx::query("UPDATE tickets SET created_at = '2020-01-01T00:00:00Z' WHERE id = $1")
        .bind(a)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let page = call(
        &w.app,
        "GET",
        "/api/tickets?status=ai_solved&limit=1",
        None,
        Some(&w.cookie),
    )
    .await
    .body;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let cursor = page["next_cursor"].as_str().unwrap();
    let page = call(
        &w.app,
        "GET",
        &format!("/api/tickets?status=ai_solved&limit=1&cursor={cursor}"),
        None,
        Some(&w.cookie),
    )
    .await
    .body;
    assert_eq!(page["items"][0]["id"], a.to_string());
    assert!(page["next_cursor"].is_null());
    let page = call(&w.app, "GET", "/api/tickets?status=ai_solved&created_from=2020-01-01T00%3A00%3A00Z&created_before=2020-01-02T00%3A00%3A00Z", None, Some(&w.cookie)).await.body;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["id"], a.to_string());
    for query in [
        "status=invalid",
        "created_from=bad",
        "created_from=2026-01-01T00%3A00%3A00Z&created_before=2020-01-01T00%3A00%3A00Z",
    ] {
        assert_eq!(
            call(
                &w.app,
                "GET",
                &format!("/api/tickets?{query}"),
                None,
                Some(&w.cookie)
            )
            .await
            .status,
            400
        );
    }
}

#[tokio::test]
async fn stored_ai_solution_can_be_written_once_during_ai_processing_only() {
    let Some(w) = world().await else { return };
    let ai = w.seed(w.owner, "ai_processing").await;
    sqlx::query("UPDATE tickets SET ai_solution = 'Restart the printer' WHERE id = $1")
        .bind(ai)
        .execute(&w.db.pool)
        .await
        .unwrap();
    for replacement in [Some("Replacement"), None] {
        assert!(
            sqlx::query("UPDATE tickets SET ai_solution = $2 WHERE id = $1")
                .bind(ai)
                .bind(replacement)
                .execute(&w.db.pool)
                .await
                .is_err()
        );
    }
    assert_eq!(
        w.detail(ai, &w.cookie).await.body["ai_solution"],
        "Restart the printer"
    );
    for status in ["queued", "closed", "cancelled"] {
        let id = w.seed(w.owner, status).await;
        assert!(
            sqlx::query("UPDATE tickets SET ai_solution = 'Late suggestion' WHERE id = $1")
                .bind(id)
                .execute(&w.db.pool)
                .await
                .is_err()
        );
    }
}
