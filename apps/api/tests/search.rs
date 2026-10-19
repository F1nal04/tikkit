//! Staff search, saved views, and bulk actions over HTTP: mixed-role and mixed-state
//! fixtures (hidden, AI-solved, other team, own ticket), injection-shaped input,
//! stale saved filters, partial bulk failures, and query plans (docs/queues.md).
use std::collections::{BTreeSet, HashMap};

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
    high: Uuid,
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
    let high = id("INSERT INTO priorities (name, sort_order) VALUES ('High', 1) RETURNING id")
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
        high,
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
        subject: &str,
    ) -> Uuid {
        self.seed_text(
            requester,
            status,
            team,
            assignee,
            subject,
            "Something is wrong",
            None,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn seed_text(
        &self,
        requester: &str,
        status: &str,
        team: Option<Uuid>,
        assignee: Option<&str>,
        subject: &str,
        description: &str,
        snapshot: Option<Value>,
    ) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description,
                status, team_id, assignee_account_id, proposal_version, resolution_source, closure_reason,
                intake_snapshot)
             VALUES ($1, $2, $3, $7, $8, $4, $5, $6,
               CASE WHEN $4 = 'closed' THEN 1 ELSE 0 END,
               CASE $4 WHEN 'closed' THEN 'operator' WHEN 'ai_solved' THEN 'ai' END,
               CASE WHEN $4 IN ('closed', 'ai_solved') THEN 'requester_confirmed' END, $9)
             RETURNING id",
        )
        .bind(self.ids[requester])
        .bind(self.category)
        .bind(self.normal)
        .bind(status)
        .bind(team)
        .bind(assignee.map(|a| self.ids[a]))
        .bind(subject)
        .bind(description)
        .bind(snapshot)
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
    }
    async fn message(&self, ticket: Uuid, author: &str, kind: &str, body: &str) {
        let author_kind = if author == "owner" {
            "requester"
        } else {
            "staff"
        };
        sqlx::query(
            "INSERT INTO ticket_messages (ticket_id, author_account_id, author_kind, kind, body)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(ticket)
        .bind(self.ids[author])
        .bind(author_kind)
        .bind(kind)
        .bind(body)
        .execute(&self.db.pool)
        .await
        .unwrap();
    }
    async fn get(&self, who: &str, path: &str) -> Reply {
        call(&self.app, "GET", path, None, Some(&self.cookies[who])).await
    }
    async fn post(&self, who: &str, path: &str, body: Value) -> Reply {
        call(
            &self.app,
            "POST",
            path,
            Some(body),
            Some(&self.cookies[who]),
        )
        .await
    }
    /// Every ticket ID `who` finds for `q` in `view`, walking all pages.
    async fn search(&self, who: &str, view: &str, q: &str, extra: &str) -> Vec<Uuid> {
        let mut found = vec![];
        let mut cursor = String::new();
        loop {
            let path = format!(
                "/api/queue?view={view}&limit=2&q={}{extra}{cursor}",
                urlencode(q)
            );
            let r = self.get(who, &path).await;
            assert_eq!(r.status, 200, "{who} {view} {q}: {}", r.body);
            for t in r.body["items"].as_array().unwrap() {
                found.push(t["id"].as_str().unwrap().parse().unwrap());
            }
            match r.body["next_cursor"].as_str() {
                Some(c) => cursor = format!("&cursor={}", urlencode(c)),
                None => return found,
            }
        }
    }
    async fn reference(&self, id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT reference FROM tickets WHERE id = $1")
            .bind(id)
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
    async fn placement(&self, id: Uuid) -> (Option<Uuid>, Option<Uuid>, String, Uuid) {
        sqlx::query_as(
            "SELECT team_id, assignee_account_id, status::text, priority_id FROM tickets WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
    }
    /// A bulk change over `tickets` at their current `updated_at`; returns per-item codes.
    async fn bulk(&self, who: &str, change: Value, tickets: &[Uuid]) -> Vec<Option<String>> {
        let mut items = vec![];
        for id in tickets {
            items.push(json!({ "id": id, "expected_updated_at": self.updated_at(*id).await }));
        }
        self.bulk_items(who, change, items).await
    }
    async fn bulk_items(&self, who: &str, change: Value, items: Vec<Value>) -> Vec<Option<String>> {
        let ids: Vec<Value> = items.iter().map(|i| i["id"].clone()).collect();
        let body = json!({ "change": change, "tickets": items });
        let r = self.post(who, "/api/tickets/bulk", body).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let results = r.body["results"].as_array().unwrap();
        assert_eq!(results.len(), ids.len());
        results
            .iter()
            .zip(ids)
            .map(|(r, id)| {
                assert_eq!(r["id"], id, "results follow request order");
                r["error"].as_str().map(String::from)
            })
            .collect()
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn set(ids: impl IntoIterator<Item = Uuid>) -> BTreeSet<Uuid> {
    ids.into_iter().collect()
}

#[tokio::test]
async fn search_matches_every_field_under_staff_visibility() {
    let Some(w) = world().await else { return };
    let desk = Some(w.desk);
    let subject = w.seed("owner", "queued", None, None, "Zebra printer").await;
    let described = w
        .seed_text(
            "owner",
            "in_progress",
            desk,
            None,
            "Plain",
            "the zebra label jams",
            None,
        )
        .await;
    let snapshot = json!({ "catalog": { "category_name": "Zebra hardware" } });
    let category = w
        .seed_text(
            "owner",
            "queued",
            desk,
            None,
            "Plain",
            "Plain",
            Some(snapshot),
        )
        .await;
    let replied = w
        .seed("owner", "waiting_for_requester", desk, Some("op"), "Plain")
        .await;
    w.message(replied, "owner", "public_reply", "My zebra is back")
        .await;
    let noted = w.seed("owner", "in_progress", desk, None, "Plain").await;
    w.message(noted, "op", "internal_note", "zebra driver suspected")
        .await;
    // A note on a ticket returned to general intake stays Admin-only.
    let general_note = w.seed("owner", "queued", None, None, "Plain").await;
    w.message(general_note, "op", "internal_note", "zebra again")
        .await;
    let closed = w.seed("owner", "closed", desk, None, "Zebra closed").await;
    // Never found by operators: other team, AI stages, AI-solved, drafts, teamless
    // cancellation, and the caller's own ticket.
    let network = w
        .seed("owner", "queued", Some(w.network), None, "Zebra net")
        .await;
    let mut hidden = vec![network];
    for status in [
        "draft",
        "ai_processing",
        "awaiting_ai_decision",
        "ai_solved",
        "cancelled",
    ] {
        hidden.push(w.seed("owner", status, None, None, "Zebra hidden").await);
    }
    let own = w.seed("op", "queued", None, None, "Zebra mine").await;
    let own_team = w
        .seed("op", "in_progress", desk, None, "Zebra mine too")
        .await;
    // Requester search by display name.
    sqlx::query("UPDATE accounts SET display_name = 'Zebra Fan' WHERE id = $1")
        .bind(w.ids["peer"])
        .execute(&w.db.pool)
        .await
        .unwrap();
    let by_name = w.seed("peer", "queued", None, None, "Plain").await;

    let general = set(w.search("op", "general", "zebra", "").await);
    assert_eq!(
        general,
        set([subject, by_name]),
        "general: no notes, no own"
    );
    let team = set(w.search("op", "team", "ZEBRA", "").await);
    assert_eq!(team, set([described, category, replied, noted]));
    let closed_hits = set(w.search("op", "team", "zebra", "&status=closed").await);
    assert_eq!(closed_hits, set([closed]));
    // Another team sees none of Desk's tickets; the dispatcher has no special access.
    let outsider = set(w.search("outsider", "team", "zebra", "").await);
    assert!(outsider.is_empty(), "{outsider:?}");
    let net = set(w.search("net_op", "team", "zebra", "").await);
    assert_eq!(net, set([network]));
    // Admin: every team plus Admin-only notes, but never AI stages, drafts, or
    // AI-solved records (queues are operator work, not Admin reporting).
    let all = set(w.search("admin", "all", "zebra", "").await);
    assert_eq!(
        all,
        set([
            subject,
            described,
            category,
            replied,
            noted,
            general_note,
            network,
            own,
            own_team,
            by_name
        ])
    );
    for id in hidden.iter().skip(1) {
        assert!(!all.contains(id));
    }
    // The own ticket stays out for its requester, even in its own team.
    let lead_all = set(w.search("lead", "team", "zebra", "").await);
    assert!(lead_all.contains(&own_team));
    let op_everything = set([
        w.search("op", "general", "zebra", "").await,
        w.search("op", "team", "zebra", "").await,
        w.search("op", "mine", "zebra", "").await,
    ]
    .concat());
    for id in hidden.iter().chain([&own, &own_team, &general_note]) {
        assert!(!op_everything.contains(id), "{id}");
    }

    // References: `123`, `#123`; also short.
    let reference = w.reference(subject).await;
    for q in [reference.to_string(), format!("#{reference}")] {
        assert_eq!(w.search("op", "general", &q, "").await, vec![subject]);
    }
    let hidden_ref = w.reference(network).await;
    assert!(
        w.search("op", "general", &hidden_ref.to_string(), "")
            .await
            .is_empty()
    );
    assert!(
        w.search("op", "team", &format!("#{hidden_ref}"), "")
            .await
            .is_empty()
    );

    // Stable paging: every match exactly once at limit 2 (checked by `search`) and
    // the same order as one big page.
    let one_page = w
        .get("admin", "/api/queue?view=all&limit=100&q=zebra")
        .await
        .body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().parse().unwrap())
        .collect::<Vec<Uuid>>();
    assert_eq!(w.search("admin", "all", "zebra", "").await, one_page);
    let sorted = w.search("admin", "all", "zebra", "&sort=priority").await;
    assert_eq!(set(sorted.clone()), set(one_page));
    assert_eq!(sorted.len(), 10);
}

#[tokio::test]
async fn search_text_is_bounded_and_always_literal() {
    let Some(w) = world().await else { return };
    let plain = w.seed("owner", "queued", None, None, "abc printer").await;
    let percent = w
        .seed("owner", "queued", None, None, "100% broken_disk")
        .await;
    let r = w.get("op", "/api/queue?q=ab").await;
    assert_eq!((r.status.as_u16(), r.code()), (422, "validation_failed"));
    assert_eq!(r.body["error"]["details"]["fields"]["q"][0], "too_short");
    let long = "x".repeat(201);
    let r = w.get("op", &format!("/api/queue?q={long}")).await;
    assert_eq!(r.body["error"]["details"]["fields"]["q"][0], "too_long");
    // Blank means no search.
    assert_eq!(w.search("op", "general", "   ", "").await.len(), 2);
    // Wildcards and quotes are literal text, never SQL or patterns.
    for q in [
        "%%%",
        "___",
        "a_c",
        "\\\\\\",
        "' OR '1'='1",
        "'; DROP TABLE tickets; --",
        "$1::text",
    ] {
        assert!(w.search("op", "general", q, "").await.is_empty(), "{q}");
    }
    assert_eq!(w.search("op", "general", "0% b", "").await, vec![percent]);
    assert_eq!(w.search("op", "general", "n_d", "").await, vec![percent]);
    assert_eq!(w.search("op", "general", "abc", "").await, vec![plain]);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn search_branches_use_their_indexes() {
    let Some(w) = world().await else { return };
    for i in 0..30 {
        let id = w
            .seed("owner", "queued", None, None, &format!("Ticket {i}"))
            .await;
        w.message(id, "owner", "public_reply", &format!("message {i}"))
            .await;
    }
    let mut tx = w.db.pool.begin().await.unwrap();
    // Tiny tables favour sequential scans; this proves each branch *can* use its index.
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let sql = format!(
        "EXPLAIN (FORMAT JSON) SELECT t.id FROM tickets t WHERE {}",
        tikkit_api::queues::search_sql(1, 2, 3)
    );
    let (plan,): (Value,) = sqlx::query_as(&sql)
        .bind("%printer%")
        .bind(7_i64)
        .bind("operator")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let plan = plan.to_string();
    for index in [
        "tickets_search_trgm_idx",
        "tickets_reference_key",
        "accounts_search_trgm_idx",
        "ticket_messages_body_trgm_idx",
    ] {
        assert!(plan.contains(index), "{index} missing from {plan}");
    }
}

#[tokio::test]
async fn saved_views_are_owner_only_and_report_stale_filters() {
    let Some(w) = world().await else { return };
    let r = w
        .post(
            "owner",
            "/api/saved-views",
            json!({ "name": "x", "filters": {} }),
        )
        .await;
    assert_eq!(r.status, 403);
    let filters = json!({
        "view": "team", "team_id": w.desk, "status": "queued", "category_id": w.category,
        "priority_id": w.high, "unassigned": true, "sort": "priority", "q": "  printer "
    });
    let r = w
        .post(
            "op",
            "/api/saved-views",
            json!({ "name": " Mine ", "filters": filters }),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.body["name"], "Mine");
    assert_eq!(r.body["filters"]["q"], "printer");
    let id = r.body["id"].as_str().unwrap().to_owned();
    // Same name again, unknown field, and filters that do not apply now.
    let r = w
        .post(
            "op",
            "/api/saved-views",
            json!({ "name": "Mine", "filters": {} }),
        )
        .await;
    assert_eq!(r.body["error"]["details"]["fields"]["name"][0], "taken");
    let r = w
        .post(
            "op",
            "/api/saved-views",
            json!({ "name": "X", "filters": { "limit": 5 } }),
        )
        .await;
    assert_eq!(r.code(), "unknown_field");
    let bad = json!({ "view": "all", "team_id": w.network, "status": "ai_solved",
                      "category_id": Uuid::new_v4(), "q": "ab" });
    let r = w
        .post(
            "op",
            "/api/saved-views",
            json!({ "name": "Bad", "filters": bad }),
        )
        .await;
    assert_eq!(r.status, 422);
    let fields = r.body["error"]["details"]["fields"].as_object().unwrap();
    let names: BTreeSet<&str> = fields.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "filters.view",
            "filters.team_id",
            "filters.status",
            "filters.category_id",
            "filters.q"
        ])
    );

    // Owner only: others neither list nor delete it.
    let r = w.get("peer", "/api/saved-views").await;
    assert_eq!(r.body, json!([]));
    let path = format!("/api/saved-views/{id}");
    let r = call(&w.app, "DELETE", &path, None, Some(&w.cookies["peer"])).await;
    assert_eq!(r.status, 404);
    let r = w.get("op", "/api/saved-views").await;
    assert_eq!(r.body[0]["stale"], json!([]));

    // Catalog and team changes make filters stale, not the view broken.
    sqlx::query("UPDATE categories SET archived_at = now() WHERE id = $1")
        .bind(w.category)
        .execute(&w.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE priorities SET archived_at = now() WHERE id = $1")
        .bind(w.high)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = w.get("op", "/api/saved-views").await;
    assert_eq!(r.body[0]["stale"], json!(["category_id", "priority_id"]));
    sqlx::query("DELETE FROM team_memberships WHERE account_id = $1")
        .bind(w.ids["op"])
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = w.get("op", "/api/saved-views").await;
    assert_eq!(
        r.body[0]["stale"],
        json!(["team_id", "category_id", "priority_id"])
    );
    assert_eq!(
        r.body[0]["filters"]["team_id"],
        json!(w.desk),
        "kept as saved"
    );

    let r = call(&w.app, "DELETE", &path, None, Some(&w.cookies["op"])).await;
    assert_eq!(r.status, 204);
    let r = w.get("op", "/api/saved-views").await;
    assert_eq!(r.body, json!([]));
}

#[tokio::test]
async fn bulk_claim_applies_per_ticket_and_reports_partial_failures() {
    let Some(w) = world().await else { return };
    let desk = Some(w.desk);
    let free = w.seed("owner", "queued", desk, None, "A").await;
    let free2 = w.seed("owner", "in_progress", desk, None, "B").await;
    let peers = w
        .seed("owner", "in_progress", desk, Some("peer"), "C")
        .await;
    let other_team = w.seed("owner", "queued", Some(w.network), None, "D").await;
    let ai_solved = w.seed("owner", "ai_solved", None, None, "E").await;
    let own = w.seed("op", "queued", desk, None, "F").await;
    let closed = w.seed("owner", "closed", desk, None, "G").await;
    let stale = w.seed("owner", "queued", desk, None, "H").await;
    let mut items = vec![];
    for id in [free, free2, peers, other_team, ai_solved, own, closed] {
        items.push(json!({ "id": id, "expected_updated_at": w.updated_at(id).await }));
    }
    items.push(json!({ "id": Uuid::new_v4(), "expected_updated_at": "2000-01-01T00:00:00Z" }));
    items.push(json!({ "id": stale, "expected_updated_at": "2000-01-01T00:00:00Z" }));
    let before: Vec<_> = {
        let mut v = vec![];
        for id in [peers, other_team, own, closed, stale] {
            v.push(w.placement(id).await);
        }
        v
    };
    let results = w
        .bulk_items("op", json!({ "action": "claim" }), items.clone())
        .await;
    let codes: Vec<Option<&str>> = results.iter().map(|c| c.as_deref()).collect();
    assert_eq!(
        codes,
        [
            None,
            None,
            Some("forbidden"),
            Some("not_found"),
            Some("not_found"),
            Some("forbidden"),
            Some("forbidden"),
            Some("not_found"),
            Some("conflict"),
        ]
    );
    assert_eq!(w.placement(free).await.1, Some(w.ids["op"]));
    assert_eq!(w.placement(free2).await.1, Some(w.ids["op"]));
    let mut after = vec![];
    for id in [peers, other_team, own, closed, stale] {
        after.push(w.placement(id).await);
    }
    assert_eq!(before, after, "failed items write nothing");
    // Replaying the same request never applies twice.
    let again = w
        .bulk_items("op", json!({ "action": "claim" }), items[..2].to_vec())
        .await;
    assert_eq!(again, [Some("conflict".into()), Some("conflict".into())]);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ticket_events WHERE event_type = 'assignment_changed'",
    )
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(events, 2);
}

#[tokio::test]
async fn bulk_management_is_limited_to_the_lead_of_each_team_or_admin() {
    let Some(w) = world().await else { return };
    let desk = Some(w.desk);
    let mine = w.seed("owner", "in_progress", desk, Some("op"), "A").await;
    let peers = w
        .seed("owner", "in_progress", desk, Some("peer"), "B")
        .await;
    let net = w
        .seed("owner", "in_progress", Some(w.network), Some("net_op"), "C")
        .await;
    let tickets = [mine, peers, net];
    let assign_op = json!({ "action": "assign", "assignee_account_id": w.ids["op"] });
    // Operator: only their own claimed ticket is writable; management is the Lead's.
    let r = w
        .bulk("op", json!({ "action": "unassign" }), &tickets)
        .await;
    assert_eq!(
        r,
        [
            Some("forbidden".into()),
            Some("forbidden".into()),
            Some("not_found".into())
        ]
    );
    let r = w
        .bulk(
            "op",
            json!({ "action": "set_priority", "priority_id": w.high }),
            &tickets,
        )
        .await;
    assert_eq!(
        r,
        [None, Some("forbidden".into()), Some("not_found".into())]
    );
    // Lead: their team only, item by item.
    let r = w.bulk("lead", assign_op.clone(), &tickets).await;
    assert_eq!(
        r,
        [
            Some("validation_failed".into()),
            None,
            Some("not_found".into())
        ],
        "already assigned to op; peer's reassigned; other team invisible"
    );
    assert_eq!(w.placement(peers).await.1, Some(w.ids["op"]));
    // Admin: across teams, still validating each target.
    let r = w.bulk("admin", assign_op, &[net]).await;
    assert_eq!(r, [Some("validation_failed".into())]);
    let r = w
        .bulk("admin", json!({ "action": "unassign" }), &tickets)
        .await;
    assert_eq!(r, [None, None, None]);
    let r = w
        .bulk(
            "admin",
            json!({ "action": "dispatch", "team_id": w.network }),
            &[mine, net],
        )
        .await;
    assert_eq!(r, [None, Some("validation_failed".into())]);
    assert_eq!(w.placement(mine).await.0, Some(w.network));
    // Status work runs the lifecycle per ticket: op lost `mine` with the transfer,
    // and may work the now unclaimed Desk ticket.
    let r = w
        .bulk(
            "op",
            json!({ "action": "request_information" }),
            &[mine, peers],
        )
        .await;
    assert_eq!(r, [Some("not_found".into()), None]);
    assert_eq!(w.placement(peers).await.2, "waiting_for_requester");
}

#[tokio::test]
async fn bulk_requests_are_bounded_and_staff_only() {
    let Some(w) = world().await else { return };
    let t = w.seed("owner", "queued", None, None, "A").await;
    let item = json!({ "id": t, "expected_updated_at": w.updated_at(t).await });
    let cases = [
        (json!([]), "required"),
        (json!([item, item]), "duplicate"),
        (
            Value::Array(
                (0..51)
                    .map(|_| json!({ "id": Uuid::new_v4(), "expected_updated_at": item["expected_updated_at"] }))
                    .collect(),
            ),
            "too_many",
        ),
    ];
    for (tickets, code) in cases {
        let body = json!({ "change": { "action": "claim" }, "tickets": tickets });
        let r = w.post("op", "/api/tickets/bulk", body).await;
        assert_eq!(r.status, 422);
        assert_eq!(r.body["error"]["details"]["fields"]["tickets"][0], code);
    }
    let body = json!({ "change": { "action": "claim" }, "tickets": [item] });
    let r = w.post("owner", "/api/tickets/bulk", body).await;
    assert_eq!(r.status, 403);
    let body = json!({ "change": { "action": "close" }, "tickets": [item] });
    let r = w.post("op", "/api/tickets/bulk", body).await;
    assert_eq!(r.status, 422);

    // Options: teams for everyone on staff, assignees only for leads (own team) and Admins.
    let r = w.get("op", "/api/queue/bulk-options").await;
    assert_eq!(r.body["teams"].as_array().unwrap().len(), 2);
    assert_eq!(r.body["assignees"], json!([]));
    let r = w.get("lead", "/api/queue/bulk-options").await;
    let names: BTreeSet<&str> = r.body["assignees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["display_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, BTreeSet::from(["lead", "op", "peer"]));
    let r = w.get("admin", "/api/queue/bulk-options").await;
    assert_eq!(r.body["assignees"].as_array().unwrap().len(), 5);
    let r = w.get("owner", "/api/queue/bulk-options").await;
    assert_eq!(r.status, 403);
}
