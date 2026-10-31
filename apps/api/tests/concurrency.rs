//! Concurrent staff writes and operator activity refresh (docs/lifecycle.md
//! "Refreshing", docs/queues.md "Counts"): parallel status and assignment changes
//! have one winner and one controlled `409`; change markers re-check visibility on
//! every poll, so team removal, transfer, or a revoked session stops delivery and
//! hidden tickets never move another team's marker.
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

struct World {
    db: TestDb,
    app: Router,
    ids: HashMap<&'static str, Uuid>,
    cookies: HashMap<&'static str, String>,
    category: Uuid,
    normal: Uuid,
    urgent: Uuid,
    desk: Uuid,
    network: Uuid,
}

const PEOPLE: [(&str, Role); 7] = [
    ("owner", Role::User),
    ("op", Role::Operator),
    ("peer", Role::Specialist),
    ("lead", Role::TeamLead),
    ("outsider", Role::Operator),
    ("net_lead", Role::TeamLead),
    ("net_op", Role::Operator),
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
    for name in ["op", "peer"] {
        fixtures::member(p, desk, ids[name]).await.unwrap();
    }
    fixtures::member(p, network, ids["net_op"]).await.unwrap();
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
    let mut cookies = HashMap::new();
    for name in ids.keys().copied().collect::<Vec<_>>() {
        cookies.insert(name, login(&app, name, PASSWORD).await.session());
    }
    Some(World {
        db,
        app,
        ids,
        cookies,
        category,
        normal,
        urgent,
        desk,
        network,
    })
}

impl World {
    async fn seed(&self, status: &str, team: Option<Uuid>, assignee: Option<&str>) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description,
                status, team_id, assignee_account_id)
             VALUES ($1, $2, $3, 'Printer', 'Jams', $4, $5, $6) RETURNING id",
        )
        .bind(self.ids["owner"])
        .bind(self.category)
        .bind(self.normal)
        .bind(status)
        .bind(team)
        .bind(assignee.map(|a| self.ids[a]))
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
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
    async fn post(&self, who: &str, path: String, body: Value) -> Reply {
        call(
            &self.app,
            "POST",
            &path,
            Some(body),
            Some(&self.cookies[who]),
        )
        .await
    }
    async fn get(&self, who: &str, path: String) -> Reply {
        call(&self.app, "GET", &path, None, Some(&self.cookies[who])).await
    }
    /// `/staff-actions` or `/assignment` with the given `expected_updated_at`.
    async fn write(
        &self,
        id: Uuid,
        who: &str,
        endpoint: &str,
        mut body: Value,
        at: &Value,
    ) -> Reply {
        body["expected_updated_at"] = at.clone();
        self.post(who, format!("/api/tickets/{id}/{endpoint}"), body)
            .await
    }
    async fn message(&self, id: Uuid, who: &str, kind: &str) -> Reply {
        let body = json!({ "idempotency_key": Uuid::new_v4(), "body": "Checked the cable" });
        self.post(who, format!("/api/tickets/{id}/{kind}"), body)
            .await
    }
    async fn events(&self, id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM ticket_events WHERE ticket_id = $1")
            .bind(id)
            .fetch_one(&self.db.pool)
            .await
            .unwrap()
    }
    /// The ticket page's change marker, or the error status.
    async fn activity(&self, id: Uuid, who: &str) -> Result<String, u16> {
        let r = self.get(who, format!("/api/tickets/{id}/activity")).await;
        match r.status.as_u16() {
            200 => Ok(r.body["version"].as_str().unwrap().to_owned()),
            status => Err(status),
        }
    }
    async fn queue_version(&self, who: &str) -> String {
        let r = self.get(who, "/api/queue/counts".into()).await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["version"].as_str().unwrap().to_owned()
    }
    async fn exec(&self, sql: &str, id: Uuid) {
        sqlx::query(sql)
            .bind(id)
            .execute(&self.db.pool)
            .await
            .unwrap();
    }
}

fn sorted(replies: &[&Reply]) -> Vec<u16> {
    let mut codes: Vec<u16> = replies.iter().map(|r| r.status.as_u16()).collect();
    codes.sort();
    codes
}

/// Parallel status, priority, and assignment writes from one loaded version:
/// exactly one commits, the other gets `409 conflict` and writes nothing.
#[tokio::test]
async fn parallel_staff_writes_have_one_winner() {
    let Some(w) = world().await else { return };
    let start = json!({ "action": "start_work" });
    let claim = json!({ "action": "claim" });
    let priority = json!({ "action": "set_priority", "priority_id": w.urgent });
    let propose = json!({ "action": "propose_resolution", "summary": "Replaced the drum" });
    let ask = json!({ "action": "request_information" });

    // Two members start work on the same unclaimed ticket.
    let id = w.seed("queued", Some(w.desk), None).await;
    let at = w.updated_at(id).await;
    let (a, b) = tokio::join!(
        w.write(id, "op", "staff-actions", start.clone(), &at),
        w.write(id, "peer", "staff-actions", start.clone(), &at),
    );
    assert_eq!(sorted(&[&a, &b]), [200, 409], "{} {}", a.body, b.body);
    assert_eq!(
        [&a, &b].iter().find(|r| r.status == 409).unwrap().code(),
        "conflict"
    );
    assert_eq!(w.events(id).await, 1);

    // A claim races a priority change and a status change on the same version.
    let id = w.seed("in_progress", Some(w.desk), None).await;
    let at = w.updated_at(id).await;
    let (a, b, c) = tokio::join!(
        w.write(id, "op", "assignment", claim.clone(), &at),
        w.write(id, "peer", "staff-actions", priority.clone(), &at),
        w.write(id, "lead", "staff-actions", ask.clone(), &at),
    );
    assert_eq!(
        sorted(&[&a, &b, &c]),
        [200, 409, 409],
        "{} {} {}",
        a.body,
        b.body,
        c.body
    );
    assert_eq!(w.events(id).await, 1);

    // The assignee and their Lead act at once on a claimed ticket.
    let id = w.seed("in_progress", Some(w.desk), Some("op")).await;
    let at = w.updated_at(id).await;
    let (a, b) = tokio::join!(
        w.write(id, "op", "staff-actions", ask.clone(), &at),
        w.write(id, "lead", "staff-actions", propose.clone(), &at),
    );
    assert_eq!(sorted(&[&a, &b]), [200, 409], "{} {}", a.body, b.body);
    assert_eq!(w.events(id).await, 1);
    let status: String = sqlx::query_scalar("SELECT status::text FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    let winner = if a.status == 200 {
        "waiting_for_requester"
    } else {
        "awaiting_confirmation"
    };
    assert_eq!(status, winner);
}

/// A stale write is a `409` even when the newer state would also deny it (a former
/// candidate after someone else claimed): the client reloads instead of seeing an
/// unexplained `403`. With the current version the policy answers.
#[tokio::test]
async fn stale_writes_conflict_before_the_policy_and_write_nothing() {
    let Some(w) = world().await else { return };
    let id = w.seed("queued", Some(w.desk), None).await;
    let old = w.updated_at(id).await;
    let r = w
        .write(id, "op", "assignment", json!({ "action": "claim" }), &old)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    for body in [
        json!({ "action": "start_work" }),
        json!({ "action": "set_priority", "priority_id": w.urgent }),
    ] {
        let r = w
            .write(id, "peer", "staff-actions", body.clone(), &old)
            .await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (409, "conflict"),
            "{}",
            r.body
        );
        let now = w.updated_at(id).await;
        let r = w.write(id, "peer", "staff-actions", body, &now).await;
        assert_eq!(r.status, 403, "{}", r.body);
    }
    assert_eq!(w.events(id).await, 1);
    // Invisible tickets stay `404`, whatever version is sent.
    let r = w
        .write(
            id,
            "net_op",
            "staff-actions",
            json!({ "action": "start_work" }),
            &old,
        )
        .await;
    assert_eq!(r.status, 404);
}

/// The ticket's requester, including a staff member who submitted it, is `403`
/// before the stale check. A wrong timestamp must not say the ticket moved, and
/// another team's wrong timestamp stays `404`.
#[tokio::test]
async fn self_handling_is_forbidden_before_the_stale_check() {
    let Some(w) = world().await else { return };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description,
            status, team_id)
         VALUES ($1, $2, $3, 'Mine', 'Own ticket', 'queued', $4) RETURNING id",
    )
    .bind(w.ids["op"])
    .bind(w.category)
    .bind(w.normal)
    .bind(w.desk)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    let wrong = json!("2000-01-01T00:00:00Z");
    let start = json!({ "action": "start_work" });
    for at in [wrong, w.updated_at(id).await] {
        let r = w.write(id, "op", "staff-actions", start.clone(), &at).await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (403, "forbidden"),
            "{}",
            r.body
        );
        let r = w
            .write(id, "op", "assignment", json!({ "action": "claim" }), &at)
            .await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (403, "forbidden"),
            "{}",
            r.body
        );
    }
    let r = w
        .write(
            id,
            "net_op",
            "staff-actions",
            start,
            &json!("2000-01-01T00:00:00Z"),
        )
        .await;
    assert_eq!(r.status, 404);
    assert_eq!(w.events(id).await, 0);
}

/// The ticket marker moves with every change the caller can see and only then;
/// polling again (a reconnect) returns the same marker, so nothing is re-delivered.
#[tokio::test]
async fn ticket_activity_follows_visible_changes() {
    let Some(w) = world().await else { return };
    let id = w.seed("in_progress", Some(w.desk), None).await;
    let staff = w.activity(id, "op").await.unwrap();
    let requester = w.activity(id, "owner").await.unwrap();
    // A digest, not a counter: the event total is not readable from either marker.
    for version in [&staff, &requester] {
        assert_eq!(version.len(), 32, "{version}");
        assert!(version.bytes().all(|b| b.is_ascii_hexdigit()), "{version}");
    }
    assert_eq!(w.activity(id, "op").await.unwrap(), staff);

    // An internal note: staff refresh, the requester's marker reveals nothing.
    assert_eq!(w.message(id, "op", "notes").await.status, 201);
    let staff2 = w.activity(id, "op").await.unwrap();
    assert_ne!(staff2, staff);
    assert_eq!(w.activity(id, "owner").await.unwrap(), requester);

    // A public reply and an assignment change move both.
    assert_eq!(w.message(id, "op", "replies").await.status, 201);
    let staff3 = w.activity(id, "op").await.unwrap();
    assert_ne!(staff3, staff2);
    let requester2 = w.activity(id, "owner").await.unwrap();
    assert_ne!(requester2, requester);
    let at = w.updated_at(id).await;
    let r = w
        .write(id, "op", "assignment", json!({ "action": "claim" }), &at)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_ne!(w.activity(id, "peer").await.unwrap(), staff3);
    assert_ne!(w.activity(id, "owner").await.unwrap(), requester2);

    // A role change alone refreshes capabilities on an open page.
    let general = w.seed("queued", None, None).await;
    let before = w.activity(general, "outsider").await.unwrap();
    w.exec(
        "UPDATE accounts SET role = 'specialist' WHERE id = $1",
        w.ids["outsider"],
    )
    .await;
    assert_ne!(w.activity(general, "outsider").await.unwrap(), before);
}

/// Delivery is re-authorized on every poll: other teams never get the ticket, a
/// transfer or team removal stops it at once, and a revoked session gets `401`.
#[tokio::test]
async fn activity_stops_for_lost_access_and_revoked_sessions() {
    let Some(w) = world().await else { return };
    let id = w.seed("in_progress", Some(w.desk), None).await;
    assert_eq!(w.activity(id, "net_op").await, Err(404));
    assert_eq!(w.activity(id, "outsider").await, Err(404));
    assert!(w.activity(id, "peer").await.is_ok());

    // Membership removal with the page still open.
    w.exec(
        "DELETE FROM team_memberships WHERE account_id = $1",
        w.ids["peer"],
    )
    .await;
    assert_eq!(w.activity(id, "peer").await, Err(404));

    // Transfer: the dispatcher's open page stops, the destination team starts.
    let at = w.updated_at(id).await;
    let r = w
        .write(
            id,
            "op",
            "assignment",
            json!({ "action": "dispatch", "team_id": w.network }),
            &at,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(w.activity(id, "op").await, Err(404));
    assert!(w.activity(id, "net_op").await.is_ok());

    // Revoked session: no more data, whatever the ticket.
    let r = w.post("net_op", "/api/auth/logout".into(), json!({})).await;
    assert!(r.status.is_success(), "{}", r.body);
    assert_eq!(w.activity(id, "net_op").await, Err(401));
    assert_eq!(
        w.get("net_op", "/api/queue/counts".into()).await.status,
        401
    );
}

/// The queue marker only reflects tickets the caller may see: work on another
/// team's ticket never moves it, while shared intake and own-team changes do.
#[tokio::test]
async fn queue_version_never_reflects_hidden_tickets() {
    let Some(w) = world().await else { return };
    let desk_ticket = w.seed("in_progress", Some(w.desk), None).await;
    let net = w.queue_version("net_op").await;
    let desk = w.queue_version("op").await;
    assert_eq!(w.queue_version("net_op").await, net);

    assert_eq!(w.message(desk_ticket, "op", "replies").await.status, 201);
    let at = w.updated_at(desk_ticket).await;
    let priority = json!({ "action": "set_priority", "priority_id": w.urgent });
    let r = w
        .write(desk_ticket, "op", "staff-actions", priority, &at)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(w.queue_version("net_op").await, net);
    let desk2 = w.queue_version("op").await;
    assert_ne!(desk2, desk);

    // A new ticket in shared intake moves every operator's marker.
    let general = w.seed("queued", None, None).await;
    let net2 = w.queue_version("net_op").await;
    assert_ne!(net2, net);
    // Dispatched to Desk: it leaves Network's view too, then Desk work is hidden again.
    let at = w.updated_at(general).await;
    let r = w
        .write(
            general,
            "op",
            "assignment",
            json!({ "action": "dispatch", "team_id": w.desk }),
            &at,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let net3 = w.queue_version("net_op").await;
    assert_ne!(net3, net2);
    let at = w.updated_at(general).await;
    let r = w
        .write(
            general,
            "peer",
            "assignment",
            json!({ "action": "claim" }),
            &at,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(w.queue_version("net_op").await, net3);
    assert_ne!(w.queue_version("op").await, desk2);

    // Removed from the team: the marker changes and the team's tickets are gone.
    let before = w.queue_version("op").await;
    w.exec(
        "DELETE FROM team_memberships WHERE account_id = $1",
        w.ids["op"],
    )
    .await;
    assert_ne!(w.queue_version("op").await, before);
    let r = w.get("op", "/api/queue?view=team".into()).await;
    assert_eq!(r.body["items"], json!([]), "{}", r.body);
}
