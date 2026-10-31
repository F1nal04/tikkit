//! Auto-close of unconfirmed staff resolutions: the pure deadline rule and the
//! durable worker against real PostgreSQL (skipped without TEST_DATABASE_URL).
//! The worker has no in-memory state, so "restart" and "catch-up" are simply
//! overdue rows found by a fresh `close_one`; deadlines are backdated in SQL
//! instead of sleeping.

mod support;

use serde_json::{Value, json};
use sqlx::PgPool;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};
use tikkit_api::{
    api::{ApiError, Role as ApiRole, TicketStatus},
    authz::{Actor, TeamScope, TicketScope},
    autoclose::{close_due, close_one},
    lifecycle::{
        By, ClosureReason, Policy, TicketState, Transition, decide, transition, transition_with,
    },
};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use TicketStatus as S;

struct World {
    t: TestDb,
    requester: Uuid,
    op: Uuid,
    team: Uuid,
    category: Uuid,
    priority: Uuid,
}

async fn world() -> Option<World> {
    let t = TestDb::new().await?;
    let p = &t.pool;
    fixtures::bootstrap_admin(p).await;
    let requester = fixtures::account(p, Role::User, "requester").await;
    let lead = fixtures::account(p, Role::TeamLead, "lead").await;
    let op = fixtures::account(p, Role::Operator, "op").await;
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
    Some(World {
        t,
        requester,
        op,
        team,
        category,
        priority,
    })
}

impl World {
    fn pool(&self) -> &PgPool {
        &self.t.pool
    }

    /// A claimed team ticket in `status`. Awaiting tickets hold proposal 1 due `due_in`.
    async fn seed(&self, status: &str, due_in: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, status, team_id,
                                  assignee_account_id, subject, description, proposal_version,
                                  confirmation_deadline)
             VALUES ($1, $2, $3, $4,
                     CASE WHEN $4 IN ('draft', 'ai_processing', 'awaiting_ai_decision') THEN NULL ELSE $5 END,
                     CASE WHEN $4 IN ('draft', 'ai_processing', 'awaiting_ai_decision') THEN NULL ELSE $6 END,
                     'Printer', 'Jammed',
                     CASE WHEN $4 = 'awaiting_confirmation' THEN 1 ELSE 0 END,
                     CASE WHEN $4 = 'awaiting_confirmation' THEN now() + $7::interval END)
             RETURNING id",
        )
        .bind(self.requester)
        .bind(self.category)
        .bind(self.priority)
        .bind(status)
        .bind(self.team)
        .bind(self.op)
        .bind(due_in)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn overdue(&self) -> Uuid {
        self.seed("awaiting_confirmation", "-1 minute").await
    }

    async fn row(
        &self,
        id: Uuid,
    ) -> (
        String,
        Option<String>,
        Option<String>,
        Option<Uuid>,
        Option<Uuid>,
    ) {
        sqlx::query_as(
            "SELECT status::text, resolution_source, closure_reason, team_id, assignee_account_id
             FROM tickets WHERE id = $1",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn events(&self, id: Uuid, kind: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM ticket_events WHERE ticket_id = $1 AND event_type = $2",
        )
        .bind(id)
        .bind(kind)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn outbox(&self, id: Uuid, kind: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM notification_outbox o JOIN ticket_events e ON e.id = o.event_id
             WHERE e.ticket_id = $1 AND e.event_type = $2",
        )
        .bind(id)
        .bind(kind)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn apply(&self, id: Uuid, by: By, t: Transition) -> Result<TicketStatus, ApiError> {
        let mut tx = self.pool().begin().await.unwrap();
        let r = transition(&mut tx, id, by, &t).await;
        if r.is_ok() {
            tx.commit().await.unwrap();
        }
        r
    }
}

fn at(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap()
}

fn state(status: TicketStatus, deadline: Option<OffsetDateTime>) -> (TicketState, Actor) {
    let (team, op) = (Uuid::new_v4(), Uuid::new_v4());
    let scope = TicketScope {
        requester_account_id: Uuid::new_v4(),
        team_id: Some(team),
        assignee_account_id: Some(op),
        status,
    };
    let state = TicketState {
        scope,
        proposal_version: 1,
        confirmation_deadline: deadline,
        work_started: true,
        category_skip_ai: false,
    };
    let actor = Actor {
        id: op,
        role: ApiRole::Operator,
        team: Some(TeamScope {
            team_id: team,
            is_lead: false,
        }),
    };
    (state, actor)
}

#[test]
fn deadline_is_elapsed_utc_hours_even_across_dst() {
    // Europe/Berlin springs forward on 2026-03-29: 168 elapsed hours is still exactly
    // seven UTC days, one local hour later than "same wall-clock time next week".
    let now = at("2026-03-25T12:00:00Z");
    let (ticket, op) = state(S::InProgress, None);
    let propose = Transition::ProposeResolution {
        summary: "Done".into(),
    };
    let out = decide(&ticket, Some(&op), &propose, &Policy::default(), now).unwrap();
    assert_eq!(out.confirmation_deadline, Some(at("2026-04-01T12:00:00Z")));
    assert_eq!(out.payload["timeout_hours"], 168);
    assert_eq!(out.payload["deadline"], "2026-04-01T12:00:00Z");
    // An Admin override changes the duration, still in elapsed hours.
    let short = Policy {
        confirmation_timeout_hours: 36,
        ..Policy::default()
    };
    let out = decide(&ticket, Some(&op), &propose, &short, now).unwrap();
    assert_eq!(out.confirmation_deadline, Some(at("2026-03-27T00:00:00Z")));
}

#[test]
fn timeout_is_rejected_before_and_accepted_at_and_after_the_deadline() {
    let due = at("2026-04-01T12:00:00Z");
    let (ticket, _) = state(S::AwaitingConfirmation, Some(due));
    let timeout = |v| Transition::ConfirmationTimeout {
        proposal_version: v,
    };
    let run = |now, v| decide(&ticket, None, &timeout(v), &Policy::default(), now);
    assert!(matches!(
        run(due - Duration::nanoseconds(1), 1),
        Err(ApiError::Conflict)
    ));
    for now in [due, due + Duration::seconds(1)] {
        let out = run(now, 1).unwrap();
        assert_eq!(out.to, S::Closed);
        let (_, reason) = out.resolution.unwrap();
        assert_eq!(reason, ClosureReason::ConfirmationTimeout);
    }
    // A timer for an older proposal never closes the newer one.
    assert!(matches!(run(due, 0), Err(ApiError::Conflict)));
    // Only the system may time out; staff and requester cannot.
    let (_, op) = state(S::AwaitingConfirmation, Some(due));
    assert!(matches!(
        decide(&ticket, Some(&op), &timeout(1), &Policy::default(), due),
        Err(ApiError::Forbidden | ApiError::NotFound)
    ));
}

#[tokio::test]
async fn overdue_proposal_closes_once_with_the_timeout_reason() {
    let Some(w) = world().await else { return };
    let id = w.overdue().await;
    assert!(close_one(w.pool()).await.unwrap());
    assert!(!close_one(w.pool()).await.unwrap());
    assert!(!close_one(w.pool()).await.unwrap());

    // Same ticket, team, and assignee; operator provenance; not a requester acceptance.
    assert_eq!(
        w.row(id).await,
        (
            "closed".into(),
            Some("operator".into()),
            Some("confirmation_timeout".into()),
            Some(w.team),
            Some(w.op)
        )
    );
    assert_eq!(w.events(id, "confirmation_timeout").await, 1);
    assert_eq!(w.events(id, "accept_resolution").await, 0);
    let (actor, payload): (Option<Uuid>, Value) = sqlx::query_as(
        "SELECT actor_account_id, payload FROM ticket_events WHERE ticket_id = $1 AND event_type = 'confirmation_timeout'",
    )
    .bind(id)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(actor, None);
    assert_eq!(payload["closure_reason"], "confirmation_timeout");
    assert_eq!(payload["resolution_source"], "operator");
    // One closure receipt for the requester, queued in the event's transaction.
    assert_eq!(w.outbox(id, "confirmation_timeout").await, 1);
}

#[tokio::test]
async fn nothing_else_is_ever_auto_closed() {
    let Some(w) = world().await else { return };
    let mut untouched = vec![w.seed("awaiting_confirmation", "1 hour").await];
    // One second before the deadline is still pending.
    untouched.push(w.seed("awaiting_confirmation", "1 second").await);
    for status in [
        "in_progress",
        "waiting_for_requester",
        "queued",
        "ai_processing",
        "awaiting_ai_decision",
        "draft",
    ] {
        untouched.push(w.seed(status, "-1 hour").await);
    }
    let (team, assignee) = (None::<Uuid>, None::<Uuid>);
    let solved: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, status, team_id,
                              assignee_account_id, subject, description, resolution_source, closure_reason)
         VALUES ($1, $2, $3, 'ai_solved', $4, $5, 'Printer', 'Jammed', 'ai', 'requester_confirmed')
         RETURNING id",
    )
    .bind(w.requester)
    .bind(w.category)
    .bind(w.priority)
    .bind(team)
    .bind(assignee)
    .fetch_one(w.pool())
    .await
    .unwrap();
    untouched.push(solved);
    let before: Vec<_> = rows(&w, &untouched).await;
    assert!(!close_one(w.pool()).await.unwrap());
    assert_eq!(rows(&w, &untouched).await, before);
    assert_eq!(w.row(solved).await.0, "ai_solved");
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ticket_events WHERE event_type = 'confirmation_timeout'",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(total, 0);
}

async fn rows(
    w: &World,
    ids: &[Uuid],
) -> Vec<(
    String,
    Option<String>,
    Option<String>,
    Option<Uuid>,
    Option<Uuid>,
)> {
    let mut rows = vec![];
    for id in ids {
        rows.push(w.row(*id).await);
    }
    rows
}

#[tokio::test]
async fn overdue_backlog_is_caught_up_once_by_concurrent_workers() {
    let Some(w) = world().await else { return };
    let mut ids = vec![];
    for _ in 0..12 {
        ids.push(w.overdue().await);
    }
    let pending = w.seed("awaiting_confirmation", "1 hour").await;
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let pool = w.pool().clone();
            tokio::spawn(async move {
                let mut closed = 0;
                while close_one(&pool).await.unwrap() {
                    closed += 1;
                }
                closed
            })
        })
        .collect();
    let mut total = 0;
    for worker in workers {
        total += worker.await.unwrap();
    }
    assert_eq!(total, 12);
    for id in ids {
        assert_eq!(w.row(id).await.0, "closed");
        assert_eq!(w.events(id, "confirmation_timeout").await, 1);
        assert_eq!(w.outbox(id, "confirmation_timeout").await, 1);
    }
    assert_eq!(w.row(pending).await.0, "awaiting_confirmation");
}

#[tokio::test]
async fn the_decision_that_commits_first_wins() {
    let Some(w) = world().await else { return };
    let me = By::Account(w.requester);
    let staff = By::Account(w.op);
    let accept = Transition::AcceptResolution {
        proposal_version: 1,
    };
    let reject = Transition::RejectResolution {
        proposal_version: 1,
    };
    let propose = Transition::ProposeResolution {
        summary: "Fixed again".into(),
    };

    // Requester accepts first: the worker finds nothing and nothing is written twice.
    let accepted = w.overdue().await;
    w.apply(accepted, me, accept.clone()).await.unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    assert_eq!(
        w.row(accepted).await.2.as_deref(),
        Some("requester_confirmed")
    );
    assert_eq!(w.events(accepted, "confirmation_timeout").await, 0);

    // Rejection first: back to work, deadline cleared, never reopened by a late timer.
    let rejected = w.overdue().await;
    w.apply(rejected, me, reject.clone()).await.unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    let (status, _, reason, team, assignee) = w.row(rejected).await;
    assert_eq!(
        (status.as_str(), reason, team, assignee),
        ("in_progress", None, Some(w.team), Some(w.op))
    );

    // Staff replace the proposal: the new version's own deadline applies, the old timer is void.
    let replaced = w.overdue().await;
    w.apply(replaced, staff, propose.clone()).await.unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    assert_eq!(w.row(replaced).await.0, "awaiting_confirmation");
    let version: i32 = sqlx::query_scalar("SELECT proposal_version FROM tickets WHERE id = $1")
        .bind(replaced)
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(version, 2);

    // Timeout first: every later decision is refused and writes nothing.
    let timed_out = w.overdue().await;
    assert!(close_one(w.pool()).await.unwrap());
    for (by, t) in [(me, accept), (me, reject), (staff, propose)] {
        let r = w.apply(timed_out, by, t).await;
        // Staff lose their write access on a closed ticket (403); the requester gets 409.
        assert!(
            matches!(r, Err(ApiError::Conflict | ApiError::Forbidden)),
            "{r:?}"
        );
    }
    assert_eq!(
        w.row(timed_out).await.2.as_deref(),
        Some("confirmation_timeout")
    );
    assert_eq!(w.events(timed_out, "confirmation_timeout").await, 1);
}

#[tokio::test]
async fn a_worker_skips_a_ticket_another_transaction_is_deciding() {
    let Some(w) = world().await else { return };
    let id = w.overdue().await;
    // The requester's decision is in flight (row locked, not yet committed).
    let mut tx = w.pool().begin().await.unwrap();
    let reject = Transition::RejectResolution {
        proposal_version: 1,
    };
    transition(&mut tx, id, By::Account(w.requester), &reject)
        .await
        .unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    tx.commit().await.unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    assert_eq!(w.row(id).await.0, "in_progress");

    // Staff replacement in flight, committed with a fresh, later deadline.
    let id = w.overdue().await;
    let mut tx = w.pool().begin().await.unwrap();
    let propose = Transition::ProposeResolution {
        summary: "New".into(),
    };
    transition(&mut tx, id, By::Account(w.op), &propose)
        .await
        .unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    tx.commit().await.unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    assert_eq!(w.row(id).await.0, "awaiting_confirmation");
}

#[tokio::test]
async fn a_cancelled_ticket_is_never_resurrected_or_closed() {
    let Some(w) = world().await else { return };
    let id = w.overdue().await;
    let cancel = Transition::Cancel {
        reason: "Solved it myself".into(),
    };
    w.apply(id, By::Account(w.requester), cancel).await.unwrap();
    assert!(!close_one(w.pool()).await.unwrap());
    assert_eq!(w.row(id).await.0, "cancelled");
    assert_eq!(w.events(id, "cancel").await, 1);
    assert_eq!(w.events(id, "confirmation_timeout").await, 0);
    // A late job that still names the old proposal is a 409 as well.
    let late = Transition::ConfirmationTimeout {
        proposal_version: 1,
    };
    assert!(matches!(
        w.apply(id, By::System, late).await,
        Err(ApiError::Conflict)
    ));
}

#[tokio::test]
async fn timeout_snapshot_ignores_later_settings_and_reopening_follows_the_toggle() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    let admin = login(&app, "admin", PASSWORD).await.session();
    let requester = login(&app, "requester", PASSWORD).await.session();
    let op = login(&app, "op", PASSWORD).await.session();
    let settings = |hours: i32, reopen: bool| {
        let (app, admin) = (app.clone(), admin.clone());
        async move {
            let body = json!({
                "name": "Tikkit", "timezone": "Europe/Berlin", "default_language": "en",
                "allow_reopening": reopen, "requester_cancellation_policy": "anytime_while_open",
                "resolution_confirmation_hours": hours, "allow_requester_ai_skip": false,
            });
            let r = call(&app, "PUT", "/api/admin/settings", Some(body), Some(&admin)).await;
            assert_eq!(r.status, 200, "{}", r.body);
        }
    };
    let propose = |id: Uuid| {
        let (app, op) = (app.clone(), op.clone());
        async move {
            let path = format!("/api/tickets/{id}");
            let at = call(&app, "GET", &path, None, Some(&op)).await.body["updated_at"].clone();
            let body = json!({"action": "propose_resolution", "summary": "Fixed", "expected_updated_at": at});
            let r = call(
                &app,
                "POST",
                &format!("{path}/staff-actions"),
                Some(body),
                Some(&op),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.body);
        }
    };
    let hours = |id: Uuid| {
        let pool = w.pool().clone();
        async move {
            sqlx::query_scalar::<_, f64>(
                "SELECT EXTRACT(EPOCH FROM t.confirmation_deadline - e.created_at)::float8 / 3600
                 FROM tickets t JOIN ticket_events e ON e.ticket_id = t.id AND e.event_type = 'propose_resolution'
                 WHERE t.id = $1 ORDER BY e.id DESC LIMIT 1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };

    let first = w.seed("in_progress", "0").await;
    propose(first).await;
    assert_eq!(hours(first).await, 168.0);
    settings(24, false).await;
    let second = w.seed("in_progress", "0").await;
    propose(second).await;
    // The edit applies to new proposals only; the pending deadline did not move.
    assert_eq!((hours(first).await, hours(second).await), (168.0, 24.0));

    // The requester sees the deadline while waiting; the worker closes the overdue one.
    let waiting = call(
        &app,
        "GET",
        &format!("/api/tickets/{first}"),
        None,
        Some(&requester),
    )
    .await
    .body;
    assert_eq!(waiting["status"], "awaiting_confirmation");
    assert!(waiting["confirmation_deadline"].is_string());
    sqlx::query(
        "UPDATE tickets SET confirmation_deadline = now() - interval '1 second' WHERE id = $1",
    )
    .bind(first)
    .execute(w.pool())
    .await
    .unwrap();
    assert!(close_one(w.pool()).await.unwrap());
    assert!(!close_one(w.pool()).await.unwrap());
    for session in [&requester, &op] {
        let d = call(
            &app,
            "GET",
            &format!("/api/tickets/{first}"),
            None,
            Some(session),
        )
        .await
        .body;
        assert_eq!(d["status"], "closed");
        assert_eq!(d["resolution_source"], "operator");
        assert_eq!(d["closure_reason"], "confirmation_timeout");
        assert!(d["confirmation_deadline"].is_null());
        let last = d["transitions"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["event_type"], "confirmation_timeout");
    }

    // Reopening an automatic closure needs the Admin toggle, like any human closure.
    let reopen = || {
        let (app, requester) = (app.clone(), requester.clone());
        async move {
            call(
                &app,
                "POST",
                &format!("/api/tickets/{first}/actions"),
                Some(json!({"action": "reopen"})),
                Some(&requester),
            )
            .await
            .status
        }
    };
    assert_eq!(reopen().await, 403);
    settings(24, true).await;
    assert_eq!(reopen().await, 200);
    assert_eq!(w.row(first).await.0, "in_progress");

    // The fixed-policy variant is the same code path, just with explicit settings.
    let mut tx = w.pool().begin().await.unwrap();
    let again = Transition::ProposeResolution {
        summary: "Again".into(),
    };
    let policy = Policy {
        confirmation_timeout_hours: 1,
        ..Policy::default()
    };
    transition_with(&mut tx, first, By::Account(w.op), &again, Some(policy))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(hours(first).await, 1.0);
}

#[tokio::test]
async fn a_failing_transition_does_not_block_the_tickets_behind_it() {
    let Some(w) = world().await else { return };
    let older = w.overdue().await;
    let newer = w.overdue().await;
    sqlx::query(
        "UPDATE tickets SET confirmation_deadline = clock_timestamp() - interval '2 hours' WHERE id = $1",
    )
    .bind(older)
    .execute(w.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE tickets SET confirmation_deadline = clock_timestamp() - interval '1 hour' WHERE id = $1",
    )
    .bind(newer)
    .execute(w.pool())
    .await
    .unwrap();
    // The oldest row's close is rejected. Without a per-cycle skip, every later
    // step would lock this same ticket again and the newer one would stay open.
    let fail = format!(
        "CREATE FUNCTION fail_autoclose() RETURNS trigger LANGUAGE plpgsql AS $fn$
         BEGIN
           IF OLD.id = '{older}' AND NEW.closure_reason = 'confirmation_timeout' THEN
             RAISE EXCEPTION 'forced autoclose failure' USING ERRCODE = 'P0001';
           END IF;
           RETURN NEW;
         END
         $fn$"
    );
    sqlx::query(&fail).execute(w.pool()).await.unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_autoclose BEFORE UPDATE ON tickets
         FOR EACH ROW EXECUTE FUNCTION fail_autoclose()",
    )
    .execute(w.pool())
    .await
    .unwrap();

    let mut skipped = Vec::new();
    assert!(close_due(w.pool(), &mut skipped).await.unwrap());
    assert_eq!(skipped, vec![older]);
    assert_eq!(w.row(older).await.0, "awaiting_confirmation");
    assert_eq!(w.events(older, "confirmation_timeout").await, 0);
    assert_eq!(w.outbox(older, "confirmation_timeout").await, 0);

    assert!(close_due(w.pool(), &mut skipped).await.unwrap());
    assert_eq!(
        w.row(newer).await.2.as_deref(),
        Some("confirmation_timeout")
    );
    assert_eq!(w.events(newer, "confirmation_timeout").await, 1);
    assert_eq!(w.row(older).await.0, "awaiting_confirmation");

    // Cycle exhausted: the skip list is cleared and the worker would sleep.
    assert!(!close_due(w.pool(), &mut skipped).await.unwrap());
    assert!(skipped.is_empty());
    // The next cycle tries the failed ticket once, then idles again. It does not spin.
    assert!(close_due(w.pool(), &mut skipped).await.unwrap());
    assert_eq!(skipped, vec![older]);
    assert!(!close_due(w.pool(), &mut skipped).await.unwrap());
    assert_eq!(w.events(older, "confirmation_timeout").await, 0);

    sqlx::query("DROP TRIGGER fail_autoclose ON tickets")
        .execute(w.pool())
        .await
        .unwrap();
    assert!(close_due(w.pool(), &mut skipped).await.unwrap());
    assert_eq!(
        w.row(older).await.2.as_deref(),
        Some("confirmation_timeout")
    );
    assert_eq!(w.events(older, "confirmation_timeout").await, 1);
    assert_eq!(w.events(newer, "confirmation_timeout").await, 1);
    assert!(!close_due(w.pool(), &mut skipped).await.unwrap());
}

/// The worker spawned in `main.rs`: it drains overdue proposals through the shared
/// `poll_loop` and returns after `stop_workers`, so shutdown can wait for it.
/// (`stop_workers` is process-wide; no other test in this binary runs a loop.)
#[tokio::test]
async fn the_spawned_worker_closes_overdue_proposals_and_stops_on_shutdown() {
    let Some(w) = world().await else { return };
    let id = w.overdue().await;
    let worker = tokio::spawn(tikkit_api::autoclose::worker_loop(w.pool().clone()));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while w.events(id, "confirmation_timeout").await == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the worker closes the overdue proposal");
    tikkit_api::notifications::stop_workers();
    tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .expect("the worker returns after stop_workers")
        .unwrap();
    assert_eq!(w.events(id, "confirmation_timeout").await, 1);
}
