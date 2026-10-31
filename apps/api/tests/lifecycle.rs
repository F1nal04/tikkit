//! The ticket lifecycle state machine and ticket visibility against real
//! PostgreSQL transactions (skipped without TEST_DATABASE_URL).

use axum::http::StatusCode;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use tikkit_api::{
    api::{ApiError, TicketStatus},
    lifecycle::{By, CancellationPolicy, Policy, Transition, transition, transition_with},
};
use uuid::Uuid;

mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

use TicketStatus as S;

/// Every toggle on, so the table below sees each transition's widest rule.
const PERMISSIVE: Policy = Policy {
    ai_available: true,
    allow_requester_skip: true,
    allow_reopen: true,
    cancellation: CancellationPolicy::AnytimeWhileOpen,
    confirmation_timeout_hours: 168,
};

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
    requester: Uuid,
    /// Member of `team`; the assignee of seeded team tickets.
    op: Uuid,
    /// Another member of `team`.
    peer: Uuid,
    lead: Uuid,
    team: Uuid,
    other_lead: Uuid,
    /// Operator without a team.
    outsider: Uuid,
    admin: Uuid,
    category: Uuid,
    exempt: Uuid,
    priority: Uuid,
}

impl World {
    fn pool(&self) -> &PgPool {
        &self.t.pool
    }
}

async fn world() -> Option<World> {
    let t = TestDb::new().await?;
    let pool = &t.pool;
    let admin = fixtures::bootstrap_admin(pool).await;
    let requester = fixtures::account(pool, Role::User, "requester").await;
    let lead = fixtures::account(pool, Role::TeamLead, "lead").await;
    let op = fixtures::account(pool, Role::Operator, "op").await;
    let peer = fixtures::account(pool, Role::Specialist, "peer").await;
    let other_lead = fixtures::account(pool, Role::TeamLead, "other_lead").await;
    let outsider = fixtures::account(pool, Role::Operator, "outsider").await;
    let team = fixtures::team(pool, "Desk", lead).await.unwrap();
    fixtures::member(pool, team, op).await.unwrap();
    fixtures::member(pool, team, peer).await.unwrap();
    fixtures::team(pool, "Network", other_lead).await.unwrap();
    let insert = |sql: &'static str| async move {
        sqlx::query_scalar::<_, Uuid>(sql)
            .fetch_one(pool)
            .await
            .unwrap()
    };
    let category = insert("INSERT INTO categories (name) VALUES ('Software') RETURNING id").await;
    let exempt =
        insert("INSERT INTO categories (name, skip_ai) VALUES ('Access', true) RETURNING id").await;
    let priority = insert("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id").await;
    Some(World {
        t,
        requester,
        op,
        peer,
        lead,
        team,
        other_lead,
        outsider,
        admin,
        category,
        exempt,
        priority,
    })
}

fn ident(status: TicketStatus) -> String {
    serde_json::to_value(status)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

/// A ticket inserted directly in `status`, with consistent proposal/resolution
/// columns. Pending proposals (version 1) are already past their deadline.
async fn seed_as(
    w: &World,
    requester: Uuid,
    category: Uuid,
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
                 CASE WHEN $4 = 'awaiting_confirmation' THEN now() - interval '1 hour' END,
                 CASE $4 WHEN 'closed' THEN 'operator' WHEN 'ai_solved' THEN 'ai' END,
                 CASE WHEN $4 IN ('closed', 'ai_solved') THEN 'requester_confirmed' END)
         RETURNING id",
    )
    .bind(requester)
    .bind(category)
    .bind(w.priority)
    .bind(ident(status))
    .bind(team)
    .bind(assignee)
    .fetch_one(w.pool())
    .await
    .unwrap()
}

/// The requester's ticket in `status`: pre-triage and AI-solved tickets have no
/// team; queued tickets wait unclaimed in `team`; later ones are claimed by `op`.
async fn seed(w: &World, status: TicketStatus) -> Uuid {
    let (team, assignee) = match status {
        S::Draft | S::AiProcessing | S::AwaitingAiDecision | S::AiSolved => (None, None),
        S::Queued => (Some(w.team), None),
        _ => (Some(w.team), Some(w.op)),
    };
    seed_as(w, w.requester, w.category, status, team, assignee).await
}

fn code(result: &Result<TicketStatus, ApiError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(ApiError::NotFound) => "not_found",
        Err(ApiError::Forbidden) => "forbidden",
        Err(ApiError::Conflict) => "conflict",
        Err(ApiError::Validation(_)) => "validation_failed",
        Err(e) => panic!("{e:?}"),
    }
}

async fn status_of(db: &mut PgConnection, id: Uuid) -> TicketStatus {
    let s: String = sqlx::query_scalar("SELECT status::text FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap();
    serde_json::from_value(Value::String(s)).unwrap()
}

/// `(event_type, from, to, actor, payload)` of every event, oldest first.
async fn events(
    db: &mut PgConnection,
    id: Uuid,
) -> Vec<(String, String, String, Option<Uuid>, Value)> {
    sqlx::query_as(
        "SELECT event_type, from_status::text, to_status::text, actor_account_id, payload
         FROM ticket_events WHERE ticket_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(db)
    .await
    .unwrap()
}

/// Commits one transition under `policy`.
async fn apply(
    w: &World,
    id: Uuid,
    by: By,
    t: Transition,
    policy: Policy,
) -> Result<TicketStatus, ApiError> {
    let mut tx = w.pool().begin().await.unwrap();
    let result = transition_with(&mut tx, id, by, &t, Some(policy)).await;
    if result.is_ok() {
        tx.commit().await.unwrap();
    }
    result
}

/// Commits one transition under the stored organization settings.
async fn apply_current(
    w: &World,
    id: Uuid,
    by: By,
    t: Transition,
) -> Result<TicketStatus, ApiError> {
    let mut tx = w.pool().begin().await.unwrap();
    let result = transition(&mut tx, id, by, &t).await;
    if result.is_ok() {
        tx.commit().await.unwrap();
    }
    result
}

fn all_transitions() -> Vec<Transition> {
    use Transition::*;
    vec![
        Submit {
            requester_skip: false,
        },
        AiSuggestionReady,
        AiFailed,
        SkipAi,
        AiSolved,
        AiNotSolved,
        StartWork,
        RequestInformation,
        RequesterResponded,
        ProposeResolution {
            summary: "Replaced the toner.".into(),
        },
        AcceptResolution {
            proposal_version: 1,
        },
        RejectResolution {
            proposal_version: 1,
        },
        ConfirmationTimeout {
            proposal_version: 1,
        },
        Reopen,
        Cancel {
            reason: "No longer needed".into(),
        },
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Who {
    Requester,
    /// The assignee of claimed team tickets (any member for unclaimed ones).
    Staff,
    System,
}

/// The lifecycle from spec.md, stated independently of the code: every
/// `(event, from, who) -> to` that is allowed. Everything else is rejected.
#[rustfmt::skip]
const ALLOWED: &[(&str, TicketStatus, Who, TicketStatus)] = &[
    ("submit", S::Draft, Who::Requester, S::AiProcessing),
    ("ai_suggestion_ready", S::AiProcessing, Who::System, S::AwaitingAiDecision),
    ("ai_failed", S::AiProcessing, Who::System, S::Queued),
    ("skip_ai", S::AiProcessing, Who::Requester, S::Queued),
    ("ai_solved", S::AwaitingAiDecision, Who::Requester, S::AiSolved),
    ("ai_not_solved", S::AwaitingAiDecision, Who::Requester, S::Queued),
    ("start_work", S::Queued, Who::Staff, S::InProgress),
    ("start_work", S::WaitingForRequester, Who::Staff, S::InProgress),
    ("request_information", S::InProgress, Who::Staff, S::WaitingForRequester),
    ("requester_responded", S::WaitingForRequester, Who::Requester, S::InProgress),
    ("propose_resolution", S::InProgress, Who::Staff, S::AwaitingConfirmation),
    ("propose_resolution", S::WaitingForRequester, Who::Staff, S::AwaitingConfirmation),
    ("propose_resolution", S::AwaitingConfirmation, Who::Staff, S::AwaitingConfirmation),
    ("accept_resolution", S::AwaitingConfirmation, Who::Requester, S::Closed),
    ("reject_resolution", S::AwaitingConfirmation, Who::Requester, S::InProgress),
    ("confirmation_timeout", S::AwaitingConfirmation, Who::System, S::Closed),
    ("reopen", S::Closed, Who::Requester, S::InProgress),
    ("reopen", S::Closed, Who::Staff, S::InProgress),
    ("cancel", S::AiProcessing, Who::Requester, S::Cancelled),
    ("cancel", S::AwaitingAiDecision, Who::Requester, S::Cancelled),
    ("cancel", S::Queued, Who::Requester, S::Cancelled),
    ("cancel", S::InProgress, Who::Requester, S::Cancelled),
    ("cancel", S::WaitingForRequester, Who::Requester, S::Cancelled),
    ("cancel", S::AwaitingConfirmation, Who::Requester, S::Cancelled),
];

#[tokio::test]
async fn only_allowed_transitions_commit_and_each_records_one_event() {
    let Some(w) = world().await else { return };
    for from in STATUSES {
        let id = seed(&w, from).await;
        for t in all_transitions() {
            for who in [Who::Requester, Who::Staff, Who::System] {
                let by = match who {
                    Who::Requester => By::Account(w.requester),
                    Who::Staff => By::Account(w.op),
                    Who::System => By::System,
                };
                let expected = ALLOWED
                    .iter()
                    .find(|(e, f, p, _)| *e == t.event_type() && *f == from && *p == who)
                    .map(|(.., to)| *to);
                let case = format!("{} from {from:?} by {who:?}", t.event_type());
                let mut tx = w.pool().begin().await.unwrap();
                let result = transition_with(&mut tx, id, by, &t, Some(PERMISSIVE)).await;
                let log = events(&mut tx, id).await;
                match expected {
                    Some(to) => {
                        assert_eq!(code(&result), "ok", "{case}");
                        assert_eq!(result.unwrap(), to, "{case}");
                        assert_eq!(status_of(&mut tx, id).await, to, "{case}");
                        let actor = match by {
                            By::Account(a) => Some(a),
                            By::System => None,
                        };
                        let (event, f, to_status, a, _) = &log[0];
                        assert_eq!(log.len(), 1, "{case}");
                        assert_eq!(
                            (event.as_str(), f.clone(), to_status.clone(), *a),
                            (t.event_type(), ident(from), ident(to), actor),
                            "{case}"
                        );
                    }
                    None => {
                        assert_ne!(code(&result), "ok", "{case}");
                        assert_eq!(status_of(&mut tx, id).await, from, "{case}");
                        assert!(log.is_empty(), "{case}");
                    }
                }
                tx.rollback().await.unwrap();
            }
        }
    }
}

#[tokio::test]
async fn routing_checks_availability_then_exemption_then_skip_permission() {
    let Some(w) = world().await else { return };
    let submit = |requester_skip| Transition::Submit { requester_skip };
    let me = By::Account(w.requester);
    let ai_on = Policy {
        ai_available: true,
        ..Policy::default()
    };
    let cases = [
        // (policy, exempt category, requester skip) -> (code, status, route)
        (
            Policy::default(),
            false,
            false,
            "ok",
            S::Queued,
            "ai_disabled",
        ),
        (
            Policy::default(),
            true,
            true,
            "ok",
            S::Queued,
            "ai_disabled",
        ),
        (ai_on, true, false, "ok", S::Queued, "category_exempt"),
        (ai_on, true, true, "ok", S::Queued, "category_exempt"),
        (ai_on, false, false, "ok", S::AiProcessing, "ai"),
        // Skipping is forbidden by default, even for forged or stale requests.
        (ai_on, false, true, "forbidden", S::Draft, ""),
        (
            Policy {
                allow_requester_skip: true,
                ..ai_on
            },
            false,
            true,
            "ok",
            S::Queued,
            "requester_skip",
        ),
    ];
    for (policy, exempt, skip, expected, status, route) in cases {
        let category = if exempt { w.exempt } else { w.category };
        let id = seed_as(&w, w.requester, category, S::Draft, None, None).await;
        let r = apply(&w, id, me, submit(skip), policy).await;
        let case = format!("{policy:?} exempt={exempt} skip={skip}");
        assert_eq!(code(&r), expected, "{case}");
        let mut db = w.pool().acquire().await.unwrap();
        assert_eq!(status_of(&mut db, id).await, status, "{case}");
        if expected == "ok" {
            assert_eq!(events(&mut db, id).await[0].4["route"], route, "{case}");
            // Every handoff to humans starts in the general queue, unassigned.
            let unrouted: bool = sqlx::query_scalar(
                "SELECT team_id IS NULL AND assignee_account_id IS NULL FROM tickets WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&mut *db)
            .await
            .unwrap();
            assert!(unrouted, "{case}");
        }
    }

    // The production entry point reads the current policy: AI is off by default.
    let id = seed_as(&w, w.requester, w.category, S::Draft, None, None).await;
    let mut tx = w.pool().begin().await.unwrap();
    let r = transition(&mut tx, id, me, &submit(true)).await;
    assert_eq!(r.unwrap(), S::Queued);
    tx.commit().await.unwrap();

    // Skipping a running attempt needs the toggle; "not solved" needs a suggestion first.
    let id = seed(&w, S::AiProcessing).await;
    let r = apply(&w, id, me, Transition::SkipAi, ai_on).await;
    assert_eq!(code(&r), "forbidden");
    let r = apply(&w, id, me, Transition::AiNotSolved, PERMISSIVE).await;
    assert_eq!(code(&r), "conflict");
    // Others cannot submit, skip, or decide for the requester.
    let draft = seed(&w, S::Draft).await;
    for by in [By::Account(w.admin), By::Account(w.op), By::System] {
        assert_ne!(
            code(&apply(&w, draft, by, submit(false), PERMISSIVE).await),
            "ok"
        );
        assert_ne!(
            code(&apply(&w, id, by, Transition::SkipAi, PERMISSIVE).await),
            "ok"
        );
    }
}

#[tokio::test]
async fn duplicate_submissions_and_escalations_happen_exactly_once() {
    let Some(w) = world().await else { return };
    let me = By::Account(w.requester);
    let submit = Transition::Submit {
        requester_skip: false,
    };
    let id = seed(&w, S::Draft).await;
    assert_eq!(
        code(&apply(&w, id, me, submit.clone(), PERMISSIVE).await),
        "ok"
    );
    assert_eq!(
        code(&apply(&w, id, me, submit, PERMISSIVE).await),
        "conflict"
    );
    for t in [Transition::AiFailed, Transition::AiFailed] {
        let _ = apply(&w, id, By::System, t, PERMISSIVE).await;
    }
    // A late suggestion after the handoff is ignored.
    let late = apply(
        &w,
        id,
        By::System,
        Transition::AiSuggestionReady,
        PERMISSIVE,
    )
    .await;
    assert_eq!(code(&late), "conflict");
    let mut db = w.pool().acquire().await.unwrap();
    let log = events(&mut db, id).await;
    let kinds: Vec<_> = log.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(kinds, ["submit", "ai_failed"]);
    assert_eq!(log[1].4["route"], "ai_failed");
}

/// Runs `first` and keeps its row lock while `second` starts in another
/// transaction, then commits `first`. Returns `second`'s result.
async fn race(
    w: &World,
    id: Uuid,
    first: (By, Transition),
    second: (By, Transition),
) -> Result<TicketStatus, ApiError> {
    let mut tx = w.pool().begin().await.unwrap();
    let won = transition_with(&mut tx, id, first.0, &first.1, Some(PERMISSIVE)).await;
    assert_eq!(code(&won), "ok");
    let pool = w.pool().clone();
    let loser = tokio::spawn(async move {
        let mut tx = pool.begin().await.unwrap();
        let r = transition_with(&mut tx, id, second.0, &second.1, Some(PERMISSIVE)).await;
        if r.is_ok() {
            tx.commit().await.unwrap();
        }
        r
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !loser.is_finished(),
        "the second decision waits for the row lock"
    );
    tx.commit().await.unwrap();
    loser.await.unwrap()
}

/// Wait for the actual database lock wait, rather than assuming task scheduling.
async fn wait_until_blocked(pool: &PgPool, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                .bind(pid)
                .fetch_one(pool)
                .await
                .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("transition must wait for the ticket row lock");
}

#[tokio::test]
async fn cancellation_sees_work_started_by_the_ticket_lock_holder() {
    let Some(w) = world().await else { return };
    let id = seed(&w, S::Queued).await;
    let before = Policy {
        cancellation: CancellationPolicy::BeforeWorkStarts,
        ..Policy::default()
    };
    let mut work = w.pool().begin().await.unwrap();
    transition_with(
        &mut work,
        id,
        By::Account(w.op),
        &Transition::StartWork,
        Some(before),
    )
    .await
    .unwrap();

    let mut cancel = w.pool().begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *cancel)
        .await
        .unwrap();
    let requester = w.requester;
    let pending = tokio::spawn(async move {
        let r = transition_with(
            &mut cancel,
            id,
            By::Account(requester),
            &Transition::Cancel {
                reason: "Too late".into(),
            },
            Some(before),
        )
        .await;
        if r.is_ok() {
            cancel.commit().await.unwrap();
        }
        r
    });
    wait_until_blocked(w.pool(), pid).await;
    work.commit().await.unwrap();
    assert_eq!(code(&pending.await.unwrap()), "forbidden");
    let mut db = w.pool().acquire().await.unwrap();
    assert_eq!(status_of(&mut db, id).await, S::InProgress);
    assert_eq!(events(&mut db, id).await.len(), 1);
}

#[tokio::test]
async fn transitions_follow_the_stored_organization_settings() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    let admin = login(&app, "admin", PASSWORD).await.session();
    let settings = |reopen: bool, cancellation: &'static str, hours: i32, skip: bool| {
        let (app, admin) = (app.clone(), admin.clone());
        async move {
            let body = serde_json::json!({
                "name": "Tikkit", "timezone": "UTC", "default_language": "en",
                "allow_reopening": reopen, "requester_cancellation_policy": cancellation,
                "resolution_confirmation_hours": hours, "allow_requester_ai_skip": skip,
            });
            let r = call(&app, "PUT", "/api/admin/settings", Some(body), Some(&admin)).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        }
    };
    let pool = w.pool().clone();
    let deadline_hours = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, f64>(
                "SELECT EXTRACT(EPOCH FROM t.confirmation_deadline - e.created_at)::float8 / 3600
             FROM tickets t JOIN ticket_events e ON e.ticket_id = t.id
             WHERE t.id = $1 AND e.event_type = 'propose_resolution'",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let me = By::Account(w.requester);
    let cancel = || Transition::Cancel {
        reason: "No longer needed".into(),
    };
    let propose = || Transition::ProposeResolution {
        summary: "Replaced the toner.".into(),
    };

    // Spec defaults: no reopening, no requester skip.
    let closed = seed(&w, S::Closed).await;
    let running = seed(&w, S::AiProcessing).await;
    assert_eq!(
        code(&apply_current(&w, closed, me, Transition::Reopen).await),
        "forbidden"
    );
    assert_eq!(
        code(&apply_current(&w, running, me, Transition::SkipAi).await),
        "forbidden"
    );

    settings(true, "disabled", 2, true).await;
    assert_eq!(
        code(&apply_current(&w, closed, me, Transition::Reopen).await),
        "ok"
    );
    assert_eq!(
        code(&apply_current(&w, running, me, Transition::SkipAi).await),
        "ok"
    );
    let queued = seed(&w, S::Queued).await;
    assert_eq!(
        code(&apply_current(&w, queued, me, cancel()).await),
        "forbidden"
    );
    let working = seed(&w, S::InProgress).await;
    let op = By::Account(w.op);
    assert_eq!(code(&apply_current(&w, working, op, propose()).await), "ok");
    assert_eq!(deadline_hours(working).await, 2.0);

    settings(false, "anytime_while_open", 168, false).await;
    assert_eq!(code(&apply_current(&w, queued, me, cancel()).await), "ok");
    let closed = seed(&w, S::Closed).await;
    assert_eq!(
        code(&apply_current(&w, closed, me, Transition::Reopen).await),
        "forbidden"
    );
    // A pending proposal keeps the deadline snapshotted when it was made.
    assert_eq!(deadline_hours(working).await, 2.0);
}

#[tokio::test]
async fn settings_changes_wait_for_transitions_that_read_them() {
    let Some(w) = world().await else { return };
    let id = seed(&w, S::Queued).await;
    let mut work = w.pool().begin().await.unwrap();
    transition(&mut work, id, By::Account(w.op), &Transition::StartWork)
        .await
        .unwrap();
    let mut update = w.pool().begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *update)
        .await
        .unwrap();
    let pending = tokio::spawn(async move {
        sqlx::query("UPDATE organization SET allow_reopening = true WHERE id")
            .execute(&mut *update)
            .await
            .unwrap();
        update.commit().await.unwrap();
    });
    wait_until_blocked(w.pool(), pid).await;
    work.commit().await.unwrap();
    pending.await.unwrap();
}

#[tokio::test]
async fn proposal_deadline_starts_when_proposed_not_when_the_transaction_began() {
    let Some(w) = world().await else { return };
    let id = seed(&w, S::InProgress).await;
    let mut tx = w.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_sleep(0.05)")
        .execute(&mut *tx)
        .await
        .unwrap();
    transition_with(
        &mut tx,
        id,
        By::Account(w.op),
        &Transition::ProposeResolution {
            summary: "Fixed".into(),
        },
        Some(PERMISSIVE),
    )
    .await
    .unwrap();
    let full_timeout: bool = sqlx::query_scalar(
        "SELECT confirmation_deadline > now() + interval '168 hours 0.025 seconds'
         FROM tickets WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(
        full_timeout,
        "the timeout starts at the proposal, after any earlier transaction work"
    );
    let consistent_timestamps: bool = sqlx::query_scalar(
        "SELECT e.created_at = t.updated_at
                AND t.confirmation_deadline = e.created_at + interval '168 hours'
         FROM tickets t JOIN ticket_events e ON e.ticket_id = t.id WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(consistent_timestamps);
}

#[tokio::test]
async fn timeout_uses_the_current_time_after_the_ticket_lock_wait() {
    let Some(w) = world().await else { return };
    let id = seed(&w, S::AwaitingConfirmation).await;
    let mut holder = w.pool().begin().await.unwrap();
    sqlx::query("SELECT id FROM tickets WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *holder)
        .await
        .unwrap();
    // The worker transaction starts before the deadline, then waits beyond it.
    let mut worker = w.pool().begin().await.unwrap();
    sqlx::query("UPDATE tickets SET confirmation_deadline = clock_timestamp() WHERE id = $1")
        .bind(id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *worker)
        .await
        .unwrap();
    let pending = tokio::spawn(async move {
        let r = transition_with(
            &mut worker,
            id,
            By::System,
            &Transition::ConfirmationTimeout {
                proposal_version: 1,
            },
            Some(PERMISSIVE),
        )
        .await;
        if r.is_ok() {
            worker.commit().await.unwrap();
        }
        r
    });
    wait_until_blocked(w.pool(), pid).await;
    holder.commit().await.unwrap();
    assert_eq!(pending.await.unwrap().unwrap(), S::Closed);
}

#[tokio::test]
async fn concurrent_opposite_decisions_have_one_winner() {
    let Some(w) = world().await else { return };
    let me = By::Account(w.requester);
    let accept = Transition::AcceptResolution {
        proposal_version: 1,
    };
    let reject = Transition::RejectResolution {
        proposal_version: 1,
    };
    let timeout = Transition::ConfirmationTimeout {
        proposal_version: 1,
    };
    let cancel = Transition::Cancel {
        reason: "Gone".into(),
    };
    let pairs = [
        (S::AwaitingConfirmation, (me, accept.clone()), (me, reject)),
        (
            S::AwaitingConfirmation,
            (By::System, timeout.clone()),
            (me, accept.clone()),
        ),
        (
            S::AwaitingConfirmation,
            (me, cancel.clone()),
            (By::System, timeout),
        ),
        (
            S::AwaitingAiDecision,
            (me, Transition::AiSolved),
            (me, Transition::AiNotSolved),
        ),
        (
            S::AiProcessing,
            (me, cancel),
            (By::System, Transition::AiFailed),
        ),
        (
            S::AiProcessing,
            (By::System, Transition::AiFailed),
            (me, Transition::SkipAi),
        ),
    ];
    for (from, first, second) in pairs {
        let id = seed(&w, from).await;
        let case = format!("{} vs {}", first.1.event_type(), second.1.event_type());
        let r = race(&w, id, first, second).await;
        assert_eq!(code(&r), "conflict", "{case}");
        let mut db = w.pool().acquire().await.unwrap();
        assert_eq!(events(&mut db, id).await.len(), 1, "{case}");
    }
}

#[tokio::test]
async fn proposals_are_versioned_snapshotted_and_never_confirmed_by_silence() {
    let Some(w) = world().await else { return };
    let me = By::Account(w.requester);
    let staff = By::Account(w.op);
    let propose = |s: &str| Transition::ProposeResolution { summary: s.into() };
    let id = seed(&w, S::InProgress).await;

    // Blank and oversized summaries are rejected before anything else.
    for summary in ["  ".to_owned(), "x".repeat(5001)] {
        let r = apply(&w, id, staff, propose(&summary), PERMISSIVE).await;
        assert_eq!(code(&r), "validation_failed");
    }
    assert_eq!(
        code(&apply(&w, id, staff, propose(" Fixed "), PERMISSIVE).await),
        "ok"
    );
    let short = Policy {
        confirmation_timeout_hours: 24,
        ..PERMISSIVE
    };
    // A superseding proposal snapshots the policy current at that moment.
    assert_eq!(
        code(&apply(&w, id, staff, propose("Fixed again"), short).await),
        "ok"
    );
    let mut db = w.pool().acquire().await.unwrap();
    let log = events(&mut db, id).await;
    assert_eq!(log[0].4["proposal_version"], 1);
    assert_eq!(log[0].4["summary"], "Fixed");
    assert_eq!(log[0].4["timeout_hours"], 168);
    assert_eq!(log[1].4["proposal_version"], 2);
    assert_eq!(log[1].4["timeout_hours"], 24);
    let (version, hours): (i32, f64) = sqlx::query_as(
        "SELECT proposal_version,
                extract(epoch FROM confirmation_deadline - now())::float8 / 3600
         FROM tickets WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *db)
    .await
    .unwrap();
    assert_eq!(version, 2);
    assert!((23.9..=24.0).contains(&hours), "{hours}");

    // Stale versions and an early timeout never close newer work.
    for (by, t) in [
        (
            me,
            Transition::AcceptResolution {
                proposal_version: 1,
            },
        ),
        (
            me,
            Transition::RejectResolution {
                proposal_version: 1,
            },
        ),
        (
            By::System,
            Transition::ConfirmationTimeout {
                proposal_version: 2,
            },
        ),
    ] {
        assert_eq!(code(&apply(&w, id, by, t, PERMISSIVE).await), "conflict");
    }
    // Rejection returns to work with the same team and assignee, even with reopening off.
    let reject = Transition::RejectResolution {
        proposal_version: 2,
    };
    assert_eq!(
        code(&apply(&w, id, me, reject, Policy::default()).await),
        "ok"
    );
    let (status, team, assignee, deadline): (String, Option<Uuid>, Option<Uuid>, bool) =
        sqlx::query_as(
            "SELECT status::text, team_id, assignee_account_id, confirmation_deadline IS NOT NULL
             FROM tickets WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *db)
        .await
        .unwrap();
    assert_eq!(
        (status.as_str(), team, assignee, deadline),
        ("in_progress", Some(w.team), Some(w.op), false)
    );

    // Accepted vs timed out closures are recorded distinctly.
    for (by, t, reason) in [
        (
            me,
            Transition::AcceptResolution {
                proposal_version: 1,
            },
            "requester_confirmed",
        ),
        (
            By::System,
            Transition::ConfirmationTimeout {
                proposal_version: 1,
            },
            "confirmation_timeout",
        ),
    ] {
        let id = seed(&w, S::AwaitingConfirmation).await;
        assert_eq!(code(&apply(&w, id, by, t, Policy::default()).await), "ok");
        let row: (String, String, String) = sqlx::query_as(
            "SELECT status::text, resolution_source, closure_reason FROM tickets WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *db)
        .await
        .unwrap();
        assert_eq!(row, ("closed".into(), "operator".into(), reason.into()));
        let log = events(&mut db, id).await;
        assert_eq!(log[0].4["closure_reason"], reason);
    }
}

#[tokio::test]
async fn ai_solved_is_final_through_every_path() {
    let Some(w) = world().await else { return };
    let id = seed(&w, S::AiSolved).await;
    let actors = [
        (By::Account(w.requester), "requester"),
        (By::Account(w.admin), "admin"),
        (By::Account(w.op), "operator"),
        (By::Account(w.lead), "lead"),
        (By::System, "system"),
    ];
    for t in all_transitions() {
        for (by, name) in actors {
            let r = apply(&w, id, by, t.clone(), PERMISSIVE).await;
            let case = format!("{} by {name}", t.event_type());
            assert_ne!(code(&r), "ok", "{case}");
            if matches!(name, "operator" | "lead") {
                // Operators never learn AI-solved records exist.
                assert_eq!(code(&r), "not_found", "{case}");
            }
        }
    }
    // Direct writes, workers, and dispatch bypassing the lifecycle hit the backstop.
    for sql in [
        "UPDATE tickets SET status = 'queued', resolution_source = NULL, closure_reason = NULL WHERE id = $1",
        "UPDATE tickets SET status = 'cancelled', resolution_source = NULL, closure_reason = NULL WHERE id = $1",
    ] {
        let r = sqlx::query(sql).bind(id).execute(w.pool()).await;
        let error = r.unwrap_err();
        let state = error.as_database_error().unwrap().code().unwrap();
        assert_eq!(state, "23001", "{sql}");
    }
    let dispatch = sqlx::query("UPDATE tickets SET team_id = $2 WHERE id = $1")
        .bind(id)
        .bind(w.team)
        .execute(w.pool())
        .await;
    assert!(dispatch.is_err());
    let mut db = w.pool().acquire().await.unwrap();
    assert!(events(&mut db, id).await.is_empty());
}

#[tokio::test]
async fn reopening_follows_the_admin_toggle_and_write_access() {
    let Some(w) = world().await else { return };
    let on = Policy {
        allow_reopen: true,
        ..Policy::default()
    };
    let me = By::Account(w.requester);
    let id = seed(&w, S::Closed).await;
    // Off by default, for everyone.
    for by in [me, By::Account(w.op), By::Account(w.admin)] {
        let r = apply(&w, id, by, Transition::Reopen, Policy::default()).await;
        assert_eq!(code(&r), "forbidden");
    }
    // Read-only peers, other teams, and jobs never reopen.
    let cases = [
        (By::Account(w.peer), "forbidden"),
        (By::Account(w.other_lead), "not_found"),
        (By::Account(w.outsider), "not_found"),
        (By::System, "forbidden"),
    ];
    for (by, expected) in cases {
        assert_eq!(
            code(&apply(&w, id, by, Transition::Reopen, on).await),
            expected
        );
    }
    for by in [
        me,
        By::Account(w.op),
        By::Account(w.lead),
        By::Account(w.admin),
    ] {
        let id = seed(&w, S::Closed).await;
        assert_eq!(code(&apply(&w, id, by, Transition::Reopen, on).await), "ok");
        let cleared: bool = sqlx::query_scalar(
            "SELECT resolution_source IS NULL AND closure_reason IS NULL FROM tickets WHERE id = $1",
        )
        .bind(id)
        .fetch_one(w.pool())
        .await
        .unwrap();
        assert!(cleared);
    }
    // Cancelled records are not closed tickets.
    let id = seed(&w, S::Cancelled).await;
    assert_eq!(
        code(&apply(&w, id, me, Transition::Reopen, on).await),
        "conflict"
    );
}

#[tokio::test]
async fn cancellation_follows_policy_and_late_work_cannot_resurrect() {
    let Some(w) = world().await else { return };
    let me = By::Account(w.requester);
    let cancel = |r: &str| Transition::Cancel { reason: r.into() };
    let before = Policy {
        cancellation: CancellationPolicy::BeforeWorkStarts,
        ..Policy::default()
    };
    let disabled = Policy {
        cancellation: CancellationPolicy::Disabled,
        ..Policy::default()
    };

    let id = seed(&w, S::Queued).await;
    for reason in [String::new(), " \n ".into(), "x".repeat(1001)] {
        let r = apply(&w, id, me, cancel(&reason), Policy::default()).await;
        assert_eq!(code(&r), "validation_failed");
    }
    assert_eq!(
        code(&apply(&w, id, me, cancel("x"), disabled).await),
        "forbidden"
    );
    // Staff and jobs cannot cancel for the requester.
    for by in [By::Account(w.op), By::Account(w.admin), By::System] {
        assert_eq!(
            code(&apply(&w, id, by, cancel("x"), Policy::default()).await),
            "forbidden"
        );
    }
    // Before work starts: allowed until the first in_progress event, even after it ends.
    assert_eq!(
        code(&apply(&w, id, By::Account(w.op), Transition::StartWork, before).await),
        "ok"
    );
    let info = Transition::RequestInformation;
    assert_eq!(
        code(&apply(&w, id, By::Account(w.op), info, before).await),
        "ok"
    );
    assert_eq!(
        code(&apply(&w, id, me, cancel("x"), before).await),
        "forbidden"
    );
    let fresh = seed(&w, S::Queued).await;
    assert_eq!(
        code(&apply(&w, fresh, me, cancel(" Fixed it myself "), before).await),
        "ok"
    );

    // Anytime while open, including while awaiting confirmation; the timer dies with it.
    let id = seed(&w, S::AwaitingConfirmation).await;
    assert_eq!(
        code(&apply(&w, id, me, cancel("Bought a new one"), Policy::default()).await),
        "ok"
    );
    let (status, team, deadline): (String, Option<Uuid>, bool) = sqlx::query_as(
        "SELECT status::text, team_id, confirmation_deadline IS NOT NULL FROM tickets WHERE id = $1",
    )
    .bind(id)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(
        (status.as_str(), team, deadline),
        ("cancelled", Some(w.team), false)
    );
    let timeout = Transition::ConfirmationTimeout {
        proposal_version: 1,
    };
    assert_eq!(
        code(&apply(&w, id, By::System, timeout, PERMISSIVE).await),
        "conflict"
    );
    let mut db = w.pool().acquire().await.unwrap();
    let log = events(&mut db, id).await;
    assert_eq!(log.len(), 1);
    assert_eq!(
        (log[0].1.as_str(), log[0].4["reason"].as_str()),
        ("awaiting_confirmation", Some("Bought a new one"))
    );

    // Pre-triage cancellation ends the request instead of reaching operators.
    let id = seed(&w, S::AiProcessing).await;
    assert_eq!(
        code(&apply(&w, id, me, cancel("x"), Policy::default()).await),
        "ok"
    );
    for t in [Transition::AiFailed, Transition::AiSuggestionReady] {
        assert_eq!(
            code(&apply(&w, id, By::System, t, PERMISSIVE).await),
            "conflict"
        );
    }
    // Finished records cannot be cancelled.
    for from in [S::Closed, S::Cancelled, S::AiSolved, S::Draft] {
        let id = seed(&w, from).await;
        let r = apply(&w, id, me, cancel("x"), Policy::default()).await;
        assert_eq!(code(&r), "conflict", "{from:?}");
    }
}

#[tokio::test]
async fn staff_writes_recheck_assignee_leadership_and_role_at_commit() {
    let Some(w) = world().await else { return };
    let info = || Transition::RequestInformation;
    let id = seed(&w, S::InProgress).await; // claimed by op
    let cases = [
        (w.peer, "forbidden"), // read-only team member
        (w.other_lead, "not_found"),
        (w.outsider, "not_found"),
    ];
    for (who, expected) in cases {
        let r = apply(&w, id, By::Account(who), info(), PERMISSIVE).await;
        assert_eq!(code(&r), expected);
    }
    for who in [w.lead, w.admin] {
        let id = seed(&w, S::InProgress).await;
        assert_eq!(
            code(&apply(&w, id, By::Account(who), info(), PERMISSIVE).await),
            "ok"
        );
    }

    // Reassignment revokes the previous assignee immediately.
    sqlx::query("UPDATE tickets SET assignee_account_id = $2 WHERE id = $1")
        .bind(id)
        .bind(w.peer)
        .execute(w.pool())
        .await
        .unwrap();
    let r = apply(&w, id, By::Account(w.op), info(), PERMISSIVE).await;
    assert_eq!(code(&r), "forbidden");
    // Leaving the team removes access entirely (assignments are cleared first).
    for sql in [
        "UPDATE tickets SET assignee_account_id = NULL WHERE assignee_account_id = $1",
        "DELETE FROM team_memberships WHERE account_id = $1",
    ] {
        sqlx::query(sql).bind(w.op).execute(w.pool()).await.unwrap();
    }
    let r = apply(&w, id, By::Account(w.op), info(), PERMISSIVE).await;
    assert_eq!(code(&r), "not_found");
    // A demoted Admin loses Admin access; a deactivated account loses everything.
    let second = fixtures::account(w.pool(), Role::Admin, "admin2").await;
    sqlx::query("UPDATE accounts SET role = 'operator' WHERE id = $1")
        .bind(w.admin)
        .execute(w.pool())
        .await
        .unwrap();
    let r = apply(&w, id, By::Account(w.admin), info(), PERMISSIVE).await;
    assert_eq!(code(&r), "not_found");
    sqlx::query("UPDATE accounts SET is_active = false WHERE id = $1")
        .bind(w.peer)
        .execute(w.pool())
        .await
        .unwrap();
    let r = apply(&w, id, By::Account(w.peer), info(), PERMISSIVE).await;
    assert_eq!(code(&r), "forbidden");
    assert_eq!(
        code(&apply(&w, id, By::Account(second), info(), PERMISSIVE).await),
        "ok"
    );

    // A role change waits for an in-flight staff transition instead of racing it.
    let id = seed_as(
        &w,
        w.requester,
        w.category,
        S::InProgress,
        Some(w.team),
        None,
    )
    .await;
    let mut tx = w.pool().begin().await.unwrap();
    let r = transition_with(&mut tx, id, By::Account(w.lead), &info(), Some(PERMISSIVE)).await;
    assert_eq!(code(&r), "ok");
    let pool = w.pool().clone();
    let lead = w.lead;
    let demote = tokio::spawn(async move {
        sqlx::query("UPDATE accounts SET display_name = 'renamed', role = 'operator' WHERE id = $1")
            .bind(lead)
            .execute(&pool)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !demote.is_finished(),
        "the role change waits for the share lock"
    );
    tx.commit().await.unwrap();
    // The demotion then fails on its own rules (a team's lead keeps the role).
    assert!(demote.await.unwrap().is_err());
}

#[tokio::test]
async fn staff_can_leave_while_assigned_to_cancelled_tickets() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    let admin = login(&app, "admin", PASSWORD).await.session();
    let other_team: Uuid = sqlx::query_scalar("SELECT id FROM teams WHERE name = 'Network'")
        .fetch_one(w.pool())
        .await
        .unwrap();
    // A requester cancelled a ticket `peer` (then `op`) had claimed. Moving `peer` to
    // another team and deactivating `op` must still succeed (both used to fail with 500).
    for (who, change) in [
        (
            w.peer,
            serde_json::json!({ "role": "specialist", "team_id": other_team }),
        ),
        (w.op, Value::Null),
    ] {
        let cancelled = seed_as(
            &w,
            w.requester,
            w.category,
            S::Cancelled,
            Some(w.team),
            Some(who),
        )
        .await;
        let open = seed_as(
            &w,
            w.requester,
            w.category,
            S::InProgress,
            Some(w.team),
            Some(who),
        )
        .await;
        let session = login(&app, if who == w.op { "op" } else { "peer" }, PASSWORD)
            .await
            .session();
        let deactivate = change.is_null();
        let reply = if deactivate {
            call(
                &app,
                "POST",
                &format!("/api/accounts/{who}/deactivate"),
                None,
                Some(&admin),
            )
            .await
        } else {
            call(
                &app,
                "PUT",
                &format!("/api/accounts/{who}"),
                Some(change),
                Some(&admin),
            )
            .await
        };
        assert_eq!(reply.status, StatusCode::NO_CONTENT, "{:?}", reply.body);
        let assignee = |id: Uuid| {
            sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT assignee_account_id FROM tickets WHERE id = $1",
            )
            .bind(id)
            .fetch_one(w.pool())
        };
        // Both are unassigned; the cancelled record stays cancelled in its team.
        assert_eq!(assignee(cancelled).await.unwrap(), None);
        assert_eq!(assignee(open).await.unwrap(), None);
        assert_eq!(
            status_of(&mut w.pool().acquire().await.unwrap(), cancelled).await,
            S::Cancelled
        );
        if deactivate {
            // The deactivation committed, so its sessions are gone too.
            let me = call(&app, "GET", "/api/auth/me", None, Some(&session)).await;
            assert_eq!(me.status, StatusCode::UNAUTHORIZED);
        }
    }
    // Clearing the assignee is the only change a final record accepts.
    let cancelled = seed_as(
        &w,
        w.requester,
        w.category,
        S::Cancelled,
        Some(w.team),
        None,
    )
    .await;
    for sql in [
        "UPDATE tickets SET assignee_account_id = $2 WHERE id = $1",
        "UPDATE tickets SET team_id = NULL WHERE id = $1 AND $2 IS NOT NULL",
    ] {
        let r = sqlx::query(sql)
            .bind(cancelled)
            .bind(w.lead)
            .execute(w.pool())
            .await;
        let state = r
            .unwrap_err()
            .as_database_error()
            .unwrap()
            .code()
            .unwrap()
            .into_owned();
        assert_eq!(state, "23001", "{sql}");
    }
}

#[tokio::test]
async fn the_requester_never_handles_their_own_ticket_in_any_role() {
    let Some(w) = world().await else { return };
    // The team lead submitted this ticket; it sits claimed by op in their own team.
    let id = seed_as(
        &w,
        w.lead,
        w.category,
        S::InProgress,
        Some(w.team),
        Some(w.op),
    )
    .await;
    let me = By::Account(w.lead);
    for t in [
        Transition::RequestInformation,
        Transition::ProposeResolution {
            summary: "Self-fixed".into(),
        },
    ] {
        assert_eq!(code(&apply(&w, id, me, t, PERMISSIVE).await), "forbidden");
    }
    // Requester actions stay available, and other staff still handle it.
    assert_eq!(
        code(
            &apply(
                &w,
                id,
                By::Account(w.op),
                Transition::RequestInformation,
                PERMISSIVE
            )
            .await
        ),
        "ok"
    );
    let responded = apply(&w, id, me, Transition::RequesterResponded, PERMISSIVE).await;
    assert_eq!(code(&responded), "ok");
    let cancel = Transition::Cancel {
        reason: "Duplicate".into(),
    };
    assert_eq!(code(&apply(&w, id, me, cancel, PERMISSIVE).await), "ok");
}

#[tokio::test]
async fn database_backstops_reject_inconsistent_lifecycle_rows() {
    let Some(w) = world().await else { return };
    let id = seed(&w, S::Queued).await;
    let draft = seed(&w, S::Draft).await;
    let state = |r: Result<sqlx::postgres::PgQueryResult, sqlx::Error>| {
        r.unwrap_err()
            .as_database_error()
            .unwrap()
            .code()
            .unwrap()
            .into_owned()
    };
    let bad = [
        (
            draft,
            "UPDATE tickets SET team_id = $2 WHERE id = $1",
            "23514",
        ),
        (
            id,
            "UPDATE tickets SET status = 'awaiting_confirmation' WHERE id = $1 AND $2 IS NOT NULL",
            "23514",
        ),
        (
            id,
            "UPDATE tickets SET status = 'closed' WHERE id = $1 AND $2 IS NOT NULL",
            "23514",
        ),
        (
            id,
            "UPDATE tickets SET resolution_source = 'ai' WHERE id = $1 AND $2 IS NOT NULL",
            "23514",
        ),
        (
            id,
            "UPDATE tickets SET subject = 'Edited' WHERE id = $1 AND $2 IS NOT NULL",
            "23001",
        ),
        (
            id,
            "UPDATE tickets SET status = 'draft', team_id = NULL WHERE id = $1 AND $2 IS NOT NULL",
            "23001",
        ),
        (
            id,
            "UPDATE tickets SET requester_account_id = $2 WHERE id = $1",
            "23001",
        ),
    ];
    for (ticket, sql, expected) in bad {
        let r = sqlx::query(sql)
            .bind(ticket)
            .bind(if sql.contains("requester") {
                w.op
            } else {
                w.team
            })
            .execute(w.pool())
            .await;
        assert_eq!(state(r), expected, "{sql}");
    }
    // Drafts stay editable; priority stays changeable after submission.
    sqlx::query("UPDATE tickets SET subject = 'Edited draft' WHERE id = $1")
        .bind(draft)
        .execute(w.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET priority_id = priority_id WHERE id = $1")
        .bind(id)
        .execute(w.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn ticket_detail_shows_requesters_their_own_and_staff_only_what_policy_allows() {
    let Some(w) = world().await else { return };
    let app = test_app(w.pool());
    fixtures::account(w.pool(), Role::User, "other").await;
    let session = |name: &'static str| {
        let app = app.clone();
        async move { login(&app, name, PASSWORD).await.session() }
    };
    let (me, op, outsider, other_lead, admin, other) = (
        session("requester").await,
        session("op").await,
        session("outsider").await,
        session("other_lead").await,
        session("admin").await,
        session("other").await,
    );
    let get = |id: Uuid, s: &str| {
        let (app, s) = (app.clone(), s.to_owned());
        async move { call(&app, "GET", &format!("/api/tickets/{id}"), None, Some(&s)).await }
    };

    for status in STATUSES {
        let id = seed(&w, status).await;
        let r = get(id, &me).await;
        assert_eq!(r.status, StatusCode::OK, "{status:?} {}", r.body);
        assert_eq!(r.body["view"], "requester");
        assert_eq!(r.body["status"], ident(status));
        assert!(r.body["requester"].is_null());
        assert_eq!(get(id, &other).await.code(), "not_found", "{status:?}");
        let pre_triage = matches!(
            status,
            S::Draft | S::AiProcessing | S::AwaitingAiDecision | S::AiSolved
        );
        // Team member: every team ticket; never drafts, AI stages, or AI-solved records.
        let r = get(id, &op).await;
        assert_eq!(r.status == StatusCode::OK, !pre_triage, "{status:?}");
        if !pre_triage {
            assert_eq!(r.body["view"], "staff");
            assert_eq!(r.body["requester"]["username"], "requester");
            assert_eq!(r.body["assignment"]["team_name"], "Desk");
        }
        // Dispatched tickets stay hidden from other teams and the dispatcher outside it.
        assert_eq!(get(id, &other_lead).await.code(), "not_found", "{status:?}");
        assert_eq!(get(id, &outsider).await.code(), "not_found", "{status:?}");
        let r = get(id, &admin).await;
        assert_eq!(r.status == StatusCode::OK, status != S::Draft, "{status:?}");
    }

    // The general queue is open to every operator.
    let general = seed_as(&w, w.requester, w.category, S::Queued, None, None).await;
    let r = get(general, &outsider).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body["assignment"]["team_name"].is_null());
    assert!(r.body["assignment"]["assignee_display_name"].is_null());

    // Transitions: public payloads for the requester, actor IDs for staff only.
    let id = seed(&w, S::InProgress).await;
    let propose = Transition::ProposeResolution {
        summary: "Replaced the toner.".into(),
    };
    apply(&w, id, By::Account(w.op), propose, PERMISSIVE)
        .await
        .unwrap();
    let r = get(id, &me).await;
    let t = &r.body["transitions"][0];
    assert_eq!(t["event_type"], "propose_resolution");
    assert_eq!(t["payload"]["summary"], "Replaced the toner.");
    assert!(t["actor_account_id"].is_null());
    assert_eq!(r.body["proposal_version"], 1);
    assert!(r.body["confirmation_deadline"].is_string());
    assert_eq!(r.body["assignment"]["assignee_display_name"], "op");
    let r = get(id, &op).await;
    assert_eq!(
        r.body["transitions"][0]["actor_account_id"],
        w.op.to_string()
    );

    let r = call(&app, "GET", &format!("/api/tickets/{id}"), None, None).await;
    assert_eq!(r.code(), "unauthorized");
    let r = get(Uuid::new_v4(), &admin).await;
    assert_eq!(r.code(), "not_found");
}
