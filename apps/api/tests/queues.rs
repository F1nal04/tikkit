//! Operator queues against real PostgreSQL (skipped without TEST_DATABASE_URL):
//! the SQL visibility predicate agrees with `authz::ticket_view`, and every
//! queue, filter, count, and direct-access path shows the same tickets.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::StatusCode;
use sqlx::PgPool;
use tikkit_api::{
    api::{ApiError, Role as ApiRole, TicketStatus},
    authz::{self, Actor, TicketScope},
    lifecycle::{By, Transition, transition},
};
use uuid::Uuid;

mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

use TicketStatus as S;

const STATUSES: [TicketStatus; 10] = [
    S::Draft,
    S::AiProcessing,
    S::AwaitingAiDecision,
    S::Queued,
    S::InProgress,
    S::WaitingForRequester,
    S::AwaitingConfirmation,
    S::Closed,
    S::Cancelled,
    S::AiSolved,
];

struct World {
    t: TestDb,
    /// Account IDs by username.
    ids: BTreeMap<&'static str, Uuid>,
    desk: Uuid,
    network: Uuid,
    software: Uuid,
    access: Uuid,
    normal: Uuid,
    urgent: Uuid,
    /// Ticket IDs by name.
    tickets: BTreeMap<&'static str, Uuid>,
}

impl World {
    fn pool(&self) -> &PgPool {
        &self.t.pool
    }
    fn names(&self, ids: impl IntoIterator<Item = Uuid>) -> BTreeSet<&'static str> {
        ids.into_iter()
            .map(|id| *self.tickets.iter().find(|(_, v)| **v == id).unwrap().0)
            .collect()
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: String) -> T {
    serde_json::from_value(serde_json::Value::String(value)).unwrap()
}

fn status_text(status: TicketStatus) -> String {
    serde_json::to_value(status)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

async fn seed(
    w: &World,
    requester: Uuid,
    status: TicketStatus,
    team: Option<Uuid>,
    assignee: Option<Uuid>,
) -> Uuid {
    seed_in(w, w.software, requester, status, team, assignee).await
}

async fn seed_in(
    w: &World,
    category: Uuid,
    requester: Uuid,
    status: TicketStatus,
    team: Option<Uuid>,
    assignee: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, status, team_id,
                              assignee_account_id, subject, description, proposal_version,
                              confirmation_deadline, resolution_source, closure_reason)
         VALUES ($1, $2, $3, $4, $5, $6, 'Printer', 'Jammed',
                 CASE WHEN $4 IN ('awaiting_confirmation', 'closed') THEN 1 ELSE 0 END,
                 CASE WHEN $4 = 'awaiting_confirmation' THEN now() + interval '1 day' END,
                 CASE $4 WHEN 'closed' THEN 'operator' WHEN 'ai_solved' THEN 'ai' END,
                 CASE WHEN $4 IN ('closed', 'ai_solved') THEN 'requester_confirmed' END)
         RETURNING id",
    )
    .bind(requester)
    .bind(category)
    .bind(w.normal)
    .bind(status_text(status))
    .bind(team)
    .bind(assignee)
    .fetch_one(w.pool())
    .await
    .unwrap()
}

/// Every status in the general queue and in two teams, plus tickets submitted
/// by staff themselves.
async fn world() -> Option<World> {
    let t = TestDb::new().await?;
    let pool = &t.pool;
    let mut ids = BTreeMap::new();
    ids.insert("admin", fixtures::bootstrap_admin(pool).await);
    for (name, role) in [
        ("requester", Role::User),
        ("lead", Role::TeamLead),
        ("op", Role::Operator),
        ("peer", Role::Specialist),
        ("other_lead", Role::TeamLead),
        ("outsider", Role::Operator),
    ] {
        ids.insert(name, fixtures::account(pool, role, name).await);
    }
    let desk = fixtures::team(pool, "Desk", ids["lead"]).await.unwrap();
    fixtures::member(pool, desk, ids["op"]).await.unwrap();
    fixtures::member(pool, desk, ids["peer"]).await.unwrap();
    let network = fixtures::team(pool, "Network", ids["other_lead"])
        .await
        .unwrap();
    let insert = |sql: &'static str| async move {
        sqlx::query_scalar::<_, Uuid>(sql)
            .fetch_one(pool)
            .await
            .unwrap()
    };
    let software = insert("INSERT INTO categories (name) VALUES ('Software') RETURNING id").await;
    let access = insert("INSERT INTO categories (name) VALUES ('Access') RETURNING id").await;
    let urgent =
        insert("INSERT INTO priorities (name, sort_order) VALUES ('Urgent', 0) RETURNING id").await;
    let normal =
        insert("INSERT INTO priorities (name, sort_order) VALUES ('Normal', 1) RETURNING id").await;
    let mut w = World {
        t,
        ids,
        desk,
        network,
        software,
        access,
        normal,
        urgent,
        tickets: BTreeMap::new(),
    };
    let (req, op, peer, other_lead) = (
        w.ids["requester"],
        w.ids["op"],
        w.ids["peer"],
        w.ids["other_lead"],
    );
    let (desk, network) = (w.desk, w.network);
    let mut add = Vec::new();
    // Pre-triage, AI-solved and abandoned-during-AI records: never in a queue.
    for (name, status) in [
        ("draft", S::Draft),
        ("ai_processing", S::AiProcessing),
        ("awaiting_ai", S::AwaitingAiDecision),
        ("ai_solved", S::AiSolved),
        ("cancelled_in_ai", S::Cancelled),
    ] {
        add.push((name, req, status, None, None));
    }
    // General intake, and the staff members' own submissions.
    add.push(("general_1", req, S::Queued, None, None));
    add.push(("general_2", req, S::Queued, None, None));
    add.push(("op_own_general", op, S::Queued, None, None));
    add.push(("op_own_desk", op, S::Queued, Some(desk), None));
    // Desk: unclaimed, claimed by op and peer, each open status, then finished.
    add.push(("desk_queued", req, S::Queued, Some(desk), None));
    add.push(("desk_progress", req, S::InProgress, Some(desk), Some(op)));
    add.push((
        "desk_waiting",
        req,
        S::WaitingForRequester,
        Some(desk),
        Some(peer),
    ));
    add.push((
        "desk_confirm",
        req,
        S::AwaitingConfirmation,
        Some(desk),
        Some(op),
    ));
    add.push(("desk_closed", req, S::Closed, Some(desk), Some(op)));
    add.push(("desk_cancelled", req, S::Cancelled, Some(desk), None));
    // Network.
    add.push(("net_queued", req, S::Queued, Some(network), None));
    add.push((
        "net_progress",
        req,
        S::InProgress,
        Some(network),
        Some(other_lead),
    ));
    for (name, requester, status, team, assignee) in add {
        // One ticket stands out by category and priority.
        let category = if name == "desk_waiting" {
            w.access
        } else {
            w.software
        };
        let id = seed_in(&w, category, requester, status, team, assignee).await;
        w.tickets.insert(name, id);
    }
    sqlx::query("UPDATE tickets SET priority_id = $1 WHERE id = $2")
        .bind(w.urgent)
        .bind(w.tickets["desk_waiting"])
        .execute(w.pool())
        .await
        .unwrap();
    Some(w)
}

async fn session(app: &axum::Router, name: &str) -> String {
    login(app, name, PASSWORD).await.session()
}

async fn ids_of(app: &axum::Router, cookie: &str, query: &str) -> Vec<Uuid> {
    let r = call(
        app,
        "GET",
        &format!("/api/queue?{query}"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.body);
    r.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().parse().unwrap())
        .collect()
}

async fn names(w: &World, app: &axum::Router, cookie: &str, query: &str) -> BTreeSet<&'static str> {
    w.names(ids_of(app, cookie, query).await)
}

fn set(names: &[&'static str]) -> BTreeSet<&'static str> {
    names.iter().copied().collect()
}

/// `staff_visible_sql` is the SQL twin of `ticket_view`: same tickets for every
/// actor over every status, team, and requester.
#[tokio::test]
async fn sql_visibility_matches_ticket_view_for_every_actor_and_ticket() {
    let Some(w) = world().await else { return };
    let pool = w.pool();
    // Also a user-role actor (never staff) and an Admin.
    let mut actors = Vec::new();
    for name in [
        "admin",
        "requester",
        "lead",
        "op",
        "peer",
        "other_lead",
        "outsider",
    ] {
        let id = w.ids[name];
        let role: ApiRole = parse(
            sqlx::query_scalar::<_, String>("SELECT role::text FROM accounts WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap(),
        );
        let team = authz::team_of(pool, id).await.unwrap();
        actors.push((name, Actor { id, role, team }));
    }
    // A former member keeps no access; an archived team grants none.
    let former = fixtures::account(pool, Role::Operator, "former").await;
    fixtures::member(pool, w.desk, former).await.unwrap();
    sqlx::query("DELETE FROM team_memberships WHERE account_id = $1")
        .bind(former)
        .execute(pool)
        .await
        .unwrap();
    actors.push((
        "former",
        Actor {
            id: former,
            role: Role::Operator,
            team: authz::team_of(pool, former).await.unwrap(),
        },
    ));
    assert_eq!(actors[2].1.team.map(|t| t.is_lead), Some(true));

    #[derive(sqlx::FromRow)]
    struct Scope {
        id: Uuid,
        requester_account_id: Uuid,
        team_id: Option<Uuid>,
        assignee_account_id: Option<Uuid>,
        status: String,
    }
    let all: Vec<Scope> = sqlx::query_as(
        "SELECT id, requester_account_id, team_id, assignee_account_id, status::text AS status
         FROM tickets",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(all.len(), w.tickets.len());
    let sql = format!(
        "SELECT t.id FROM tickets t WHERE {}",
        authz::staff_visible_sql(1)
    );
    for (name, actor) in &actors {
        let (id, role, team) = actor.visibility_binds();
        let from_sql: BTreeSet<Uuid> = sqlx::query_scalar(&sql)
            .bind(id)
            .bind(role)
            .bind(team)
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .collect();
        let from_policy: BTreeSet<Uuid> = all
            .iter()
            .filter(|t| {
                let scope = TicketScope {
                    requester_account_id: t.requester_account_id,
                    team_id: t.team_id,
                    assignee_account_id: t.assignee_account_id,
                    status: parse(t.status.clone()),
                };
                authz::ticket_view(actor, &scope) == Some(authz::TicketView::Staff)
            })
            .map(|t| t.id)
            .collect();
        assert_eq!(
            w.names(from_sql.iter().copied()),
            w.names(from_policy.iter().copied()),
            "{name}"
        );
    }
    // And the table above exercised every status.
    let statuses: BTreeSet<String> = all.iter().map(|t| t.status.clone()).collect();
    assert_eq!(statuses.len(), STATUSES.len());
}

#[tokio::test]
async fn every_queue_shows_exactly_what_the_actor_may_work_on() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    let (admin, lead, op, peer, other_lead, outsider, requester) = (
        session(&app, "admin").await,
        session(&app, "lead").await,
        session(&app, "op").await,
        session(&app, "peer").await,
        session(&app, "other_lead").await,
        session(&app, "outsider").await,
        session(&app, "requester").await,
    );
    let desk_open = [
        "desk_queued",
        "desk_progress",
        "desk_waiting",
        "desk_confirm",
    ];

    // General intake: every operator, never pre-triage/AI-solved/cancelled-in-AI,
    // never your own submission (you are its requester, not its handler).
    assert_eq!(
        names(&w, &app, &outsider, "").await,
        set(&["general_1", "general_2", "op_own_general"])
    );
    for who in [&lead, &peer, &other_lead, &admin] {
        assert_eq!(
            names(&w, &app, who, "view=general").await,
            set(&["general_1", "general_2", "op_own_general"])
        );
    }
    assert_eq!(
        names(&w, &app, &op, "view=general").await,
        set(&["general_1", "general_2"])
    );

    // Team: only the caller's own team, open work including unclaimed tickets.
    let mut desk = set(&desk_open);
    desk.insert("op_own_desk");
    assert_eq!(names(&w, &app, &peer, "view=team").await, desk);
    assert_eq!(names(&w, &app, &lead, "view=team").await, desk);
    desk.remove("op_own_desk");
    assert_eq!(names(&w, &app, &op, "view=team").await, desk);
    assert_eq!(
        names(&w, &app, &other_lead, "view=team").await,
        set(&["net_queued", "net_progress"])
    );
    assert!(names(&w, &app, &outsider, "view=team").await.is_empty());
    // Admin: every dispatched ticket, or one team.
    let mut dispatched = desk.clone();
    dispatched.extend(["op_own_desk", "net_queued", "net_progress"]);
    assert_eq!(names(&w, &app, &admin, "view=team").await, dispatched);
    assert_eq!(
        names(
            &w,
            &app,
            &admin,
            &format!("view=team&team_id={}", w.network)
        )
        .await,
        set(&["net_queued", "net_progress"])
    );
    // Non-members cannot discover another team, even by naming it.
    let other = format!("/api/queue?view=team&team_id={}", w.network);
    assert_eq!(
        call(&app, "GET", &other, None, Some(&op)).await.code(),
        "not_found"
    );
    assert_eq!(
        call(&app, "GET", &other, None, Some(&outsider))
            .await
            .code(),
        "not_found"
    );
    let own = names(&w, &app, &op, &format!("view=team&team_id={}", w.desk)).await;
    assert_eq!(own, desk);

    // Assigned to me.
    assert_eq!(
        names(&w, &app, &op, "view=mine").await,
        set(&["desk_progress", "desk_confirm"])
    );
    assert_eq!(
        names(&w, &app, &peer, "view=mine").await,
        set(&["desk_waiting"])
    );
    assert!(names(&w, &app, &outsider, "view=mine").await.is_empty());
    assert!(names(&w, &app, &admin, "view=mine").await.is_empty());

    // Admin overview: intake plus every team; operators are refused.
    let mut all = dispatched.clone();
    all.extend(["general_1", "general_2", "op_own_general"]);
    assert_eq!(names(&w, &app, &admin, "view=all").await, all);
    let r = call(&app, "GET", "/api/queue?view=all", None, Some(&op)).await;
    assert_eq!(r.code(), "forbidden");

    // Capability and input errors.
    let r = call(&app, "GET", "/api/queue", None, Some(&requester)).await;
    assert_eq!(r.code(), "forbidden");
    assert_eq!(
        call(&app, "GET", "/api/queue", None, None).await.code(),
        "unauthorized"
    );
    for bad in [
        "status=ai_solved",
        "status=draft",
        "status=ai_processing",
        "status=awaiting_ai_decision",
        "status=nope",
        "view=nope",
        "sort=nope",
        "view=general&team_id=00000000-0000-0000-0000-000000000000",
    ] {
        let r = call(
            &app,
            "GET",
            &format!("/api/queue?{bad}"),
            None,
            Some(&admin),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }

    let r = call(&app, "GET", "/api/queue?cursor=nope", None, Some(&admin)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Counts equal the open-work list sizes of the same views.
    let counts = |who: &str| {
        let (app, who) = (app.clone(), who.to_owned());
        async move {
            let r = call(&app, "GET", "/api/queue/counts", None, Some(&who)).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.body);
            r.body
        }
    };
    for (who, team_view, mine, general) in [
        (&op, desk.len(), 2, 2),
        (&peer, desk.len() + 1, 1, 3),
        (&lead, desk.len() + 1, 0, 3),
        (&other_lead, 2, 1, 3),
        (&outsider, 0, 0, 3),
    ] {
        let c = counts(who).await;
        assert_eq!(c["general"], general);
        assert_eq!(c["team"], team_view);
        assert_eq!(c["mine"], mine);
        assert!(c["all"].is_null());
    }
    let c = counts(&admin).await;
    assert_eq!(
        (c["general"].as_i64(), c["team"].as_i64()),
        (Some(3), Some(dispatched.len() as i64))
    );
    assert_eq!(c["all"], all.len());
    assert_eq!(c["mine"], 0);
    let r = call(&app, "GET", "/api/queue/counts", None, Some(&requester)).await;
    assert_eq!(r.code(), "forbidden");

    // Finished tickets appear only when asked for, still under team isolation.
    assert_eq!(
        names(&w, &app, &op, "view=team&status=closed").await,
        set(&["desk_closed"])
    );
    assert_eq!(
        names(&w, &app, &peer, "view=team&status=cancelled").await,
        set(&["desk_cancelled"])
    );
    assert!(
        names(&w, &app, &outsider, "view=team&status=closed")
            .await
            .is_empty()
    );
    assert!(
        names(&w, &app, &other_lead, "view=team&status=closed")
            .await
            .is_empty()
    );
    assert!(
        names(&w, &app, &admin, "view=general&status=cancelled")
            .await
            .is_empty()
    );

    // Filters.
    assert_eq!(
        names(&w, &app, &peer, "view=team&status=queued").await,
        set(&["desk_queued", "op_own_desk"])
    );
    assert_eq!(
        names(&w, &app, &op, "view=team&unassigned=true").await,
        set(&["desk_queued"])
    );
    let by_category = format!("view=team&category_id={}", w.access);
    assert_eq!(
        names(&w, &app, &op, &by_category).await,
        set(&["desk_waiting"])
    );
    let by_priority = format!("view=team&priority_id={}", w.urgent);
    assert_eq!(
        names(&w, &app, &op, &by_priority).await,
        set(&["desk_waiting"])
    );

    // Direct access agrees: every listed ticket opens as staff; everything the
    // list hides from this caller is 404 (or the requester's own view).
    for (who, name) in [
        (&op, "op"),
        (&outsider, "outsider"),
        (&other_lead, "other_lead"),
        (&admin, "admin"),
    ] {
        let mut listed = BTreeSet::new();
        for view in ["general", "team", "mine"] {
            for status in [
                "queued",
                "in_progress",
                "waiting_for_requester",
                "awaiting_confirmation",
                "closed",
                "cancelled",
            ] {
                listed.extend(names(&w, &app, who, &format!("view={view}&status={status}")).await);
            }
        }
        for (ticket, id) in &w.tickets {
            let r = call(&app, "GET", &format!("/api/tickets/{id}"), None, Some(who)).await;
            let staff = r.status == StatusCode::OK && r.body["view"] == "staff";
            assert_eq!(
                staff,
                listed.contains(ticket)
                    || (name == "admin"
                        && matches!(
                            *ticket,
                            "ai_processing" | "awaiting_ai" | "ai_solved" | "cancelled_in_ai"
                        )),
                "{name} {ticket}"
            );
        }
    }
}

#[tokio::test]
async fn pages_are_stable_in_every_order_and_rows_carry_the_columns() {
    let Some(w) = world().await else { return };
    // Many tickets in one queue, with distinct priorities.
    for i in 0..9 {
        let id = seed(&w, w.ids["requester"], S::Queued, None, None).await;
        let priority = if i % 3 == 0 { w.urgent } else { w.normal };
        sqlx::query("UPDATE tickets SET priority_id = $1 WHERE id = $2")
            .bind(priority)
            .bind(id)
            .execute(w.pool())
            .await
            .unwrap();
    }
    let app = test_app(w.pool());
    let cookie = session(&app, "outsider").await;
    for sort in ["newest", "oldest", "priority"] {
        let all: Vec<Uuid> = {
            let r = call(
                &app,
                "GET",
                &format!("/api/queue?limit=100&sort={sort}"),
                None,
                Some(&cookie),
            )
            .await;
            r.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["id"].as_str().unwrap().parse().unwrap())
                .collect()
        };
        assert_eq!(all.len(), 2 + 1 + 9, "{sort}");
        let mut paged = Vec::new();
        let mut cursor = String::new();
        loop {
            let r = call(
                &app,
                "GET",
                &format!("/api/queue?limit=4&sort={sort}{cursor}"),
                None,
                Some(&cookie),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.body);
            let items = r.body["items"].as_array().unwrap();
            assert!(items.len() <= 4);
            paged.extend(
                items
                    .iter()
                    .map(|i| i["id"].as_str().unwrap().parse::<Uuid>().unwrap()),
            );
            match r.body["next_cursor"].as_str() {
                Some(next) => cursor = format!("&cursor={next}"),
                None => break,
            }
        }
        assert_eq!(
            paged, all,
            "{sort}: pages concatenate to the full ordered list"
        );
    }
    let r = call(
        &app,
        "GET",
        "/api/queue?sort=priority&limit=100",
        None,
        Some(&cookie),
    )
    .await;
    let priorities: Vec<&str> = r.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["priority_name"].as_str().unwrap())
        .collect();
    let first_normal = priorities.iter().position(|p| *p == "Normal").unwrap();
    assert!(priorities[..first_normal].iter().all(|p| *p == "Urgent"));
    assert!(priorities[first_normal..].iter().all(|p| *p == "Normal"));

    let row = &r.body["items"][0];
    for key in [
        "id",
        "reference",
        "status",
        "subject",
        "category_name",
        "priority_id",
        "priority_name",
        "created_at",
        "updated_at",
    ] {
        assert!(!row[key].is_null(), "{key}");
    }
    assert_eq!(row["requester"]["username"], "requester");
    assert!(row["assignment"]["team_name"].is_null());
    assert!(row["assignment"]["assignee_display_name"].is_null());

    let mine = call(
        &app,
        "GET",
        "/api/queue?view=team&limit=100",
        None,
        Some(&session(&app, "peer").await),
    )
    .await;
    let waiting = mine.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == w.tickets["desk_waiting"].to_string())
        .unwrap();
    assert_eq!(waiting["assignment"]["team_name"], "Desk");
    assert_eq!(waiting["assignment"]["assignee_display_name"], "peer");
    assert_eq!(waiting["status"], "waiting_for_requester");
}

#[tokio::test]
async fn effective_priority_drives_filters_and_ordering() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    let cookie = session(&app, "outsider").await;
    let general_1 = w.tickets["general_1"];
    let urgent = format!("view=general&priority_id={}", w.urgent);
    assert!(ids_of(&app, &cookie, &urgent).await.is_empty());
    sqlx::query("UPDATE tickets SET priority_id = $1 WHERE id = $2")
        .bind(w.urgent)
        .bind(general_1)
        .execute(w.pool())
        .await
        .unwrap();
    assert_eq!(ids_of(&app, &cookie, &urgent).await, vec![general_1]);
    let r = call(&app, "GET", "/api/queue?sort=priority", None, Some(&cookie)).await;
    assert_eq!(r.body["items"][0]["id"], general_1.to_string());
    assert_eq!(r.body["items"][0]["priority_name"], "Urgent");
    let normal = format!("view=general&priority_id={}", w.normal);
    assert!(!ids_of(&app, &cookie, &normal).await.contains(&general_1));
}

/// A submission leaves AI exactly once: absent while in AI, in the general queue
/// once after escalation, however many times the escalation is replayed.
#[tokio::test]
async fn escalated_ai_submissions_appear_once_in_general_intake() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    let cookie = session(&app, "outsider").await;
    let requester = w.ids["requester"];
    let failed = seed(&w, requester, S::AiProcessing, None, None).await;
    let not_solved = seed(&w, requester, S::AwaitingAiDecision, None, None).await;
    let before = ids_of(&app, &cookie, "view=general").await;
    assert!(!before.contains(&failed) && !before.contains(&not_solved));

    let apply = |id: Uuid, by: By, t: Transition| {
        let pool = w.pool().clone();
        async move {
            let mut tx = pool.begin().await.unwrap();
            let result = transition(&mut tx, id, by, &t).await;
            if result.is_ok() {
                tx.commit().await.unwrap();
            }
            result
        }
    };
    apply(failed, By::System, Transition::AiFailed)
        .await
        .unwrap();
    apply(not_solved, By::Account(requester), Transition::AiNotSolved)
        .await
        .unwrap();
    // Replays are refused, so nothing is queued twice.
    for (id, by, replay) in [
        (failed, By::System, Transition::AiFailed),
        (not_solved, By::Account(requester), Transition::AiNotSolved),
    ] {
        assert!(matches!(
            apply(id, by, replay).await,
            Err(ApiError::Conflict)
        ));
    }
    let after = ids_of(&app, &cookie, "view=general").await;
    for id in [failed, not_solved] {
        assert_eq!(after.iter().filter(|i| **i == id).count(), 1);
    }
    assert_eq!(after.len(), before.len() + 2);
}

/// Dispatching moves a ticket out of general intake into the target team only.
#[tokio::test]
async fn dispatch_moves_a_ticket_from_intake_to_the_target_team_only() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    let (outsider, op, other_lead, admin) = (
        session(&app, "outsider").await,
        session(&app, "op").await,
        session(&app, "other_lead").await,
        session(&app, "admin").await,
    );
    let id = w.tickets["general_1"];
    sqlx::query("UPDATE tickets SET team_id = $1 WHERE id = $2")
        .bind(w.desk)
        .bind(id)
        .execute(w.pool())
        .await
        .unwrap();
    // The dispatcher (an operator outside the team) and other teams lose it everywhere.
    for who in [&outsider, &other_lead] {
        for view in ["general", "team", "mine"] {
            assert!(
                !ids_of(&app, who, &format!("view={view}"))
                    .await
                    .contains(&id)
            );
        }
        let r = call(&app, "GET", &format!("/api/tickets/{id}"), None, Some(who)).await;
        assert_eq!(r.code(), "not_found");
        let c = call(&app, "GET", "/api/queue/counts", None, Some(who)).await;
        assert_eq!(c.body["general"], 2); // general_2, op_own_general
    }
    assert!(ids_of(&app, &op, "view=team").await.contains(&id));
    assert!(!ids_of(&app, &op, "view=general").await.contains(&id));
    assert!(ids_of(&app, &admin, "view=team").await.contains(&id));
    // Still one ticket, same reference, unassigned in the team's queue.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets WHERE id = $1 AND assignee_account_id IS NULL",
    )
    .bind(id)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(n, 1);
}
