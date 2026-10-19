//! Durable notifications: outbox atomicity, retries, deduplication, concurrent
//! workers, delivery-time access checks, and content limits. Email goes to the
//! in-process SMTP server shared with the SMTP tests.

mod support;

use std::{sync::Arc, time::Duration};

use axum::Router;
use serde_json::{Value, json};
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login,
    smtp::{FakeSmtp, Fault, Listen, SECRET, USER, decode_mail, listen, wait_messages},
    test_app_with,
};
use tikkit_api::{
    auth::Settings,
    lifecycle::{self, By, Transition},
    notifications::{MAX_ATTEMPTS, process_one},
};
use uuid::Uuid;

struct World {
    db: TestDb,
    app: Router,
    settings: Settings,
    smtp: FakeSmtp,
    admin: Uuid,
    requester: Uuid,
    op: Uuid,
    peer: Uuid,
    lead: Uuid,
    other: Uuid,
    desk: Uuid,
    network: Uuid,
    category: Uuid,
    priority: Uuid,
}

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    let admin = fixtures::bootstrap_admin(p).await;
    let requester = fixtures::account(p, Role::User, "requester").await;
    let op = fixtures::account(p, Role::Operator, "op").await;
    let peer = fixtures::account(p, Role::Specialist, "peer").await;
    let lead = fixtures::account(p, Role::TeamLead, "lead").await;
    let other = fixtures::account(p, Role::TeamLead, "other").await;
    let desk = fixtures::team(p, "Desk", lead).await.unwrap();
    let network = fixtures::team(p, "Network", other).await.unwrap();
    fixtures::member(p, desk, op).await.unwrap();
    fixtures::member(p, desk, peer).await.unwrap();
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
    let smtp = listen(Listen::StartTls, Fault::None).await;
    let settings = Settings {
        smtp_trust_root_pem: Some(smtp.ca_pem.clone().into_bytes()),
        ..Settings::default()
    };
    let app = test_app_with(p, settings.clone());
    let w = World {
        db,
        app,
        settings,
        smtp,
        admin,
        requester,
        op,
        peer,
        lead,
        other,
        desk,
        network,
        category,
        priority,
    };
    w.configure_smtp(w.smtp.port).await;
    Some(w)
}

impl World {
    async fn cookie(&self, name: &str) -> String {
        login(&self.app, name, PASSWORD).await.session()
    }

    async fn configure_smtp(&self, port: u16) {
        let body = json!({
            "host": "127.0.0.1", "port": port, "security": "starttls",
            "sender_name": "Tikkit Help", "sender_address": "help@example.com",
            "username": USER, "password": SECRET, "enabled": true,
        });
        let admin = self.cookie("admin").await;
        let r = call(
            &self.app,
            "PUT",
            "/api/settings/smtp",
            Some(body),
            Some(&admin),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }

    async fn ticket(&self, status: &str, team: Option<Uuid>, assignee: Option<Uuid>) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO tickets (requester_account_id, category_id, priority_id, status, team_id,
                assignee_account_id, subject, description, proposal_version, confirmation_deadline,
                resolution_source, closure_reason)
             VALUES ($1, $2, $3, $4, $5, $6, 'Printer subject', 'Jammed description',
               CASE WHEN $4 IN ('closed', 'awaiting_confirmation') THEN 1 ELSE 0 END,
               CASE WHEN $4 = 'awaiting_confirmation' THEN now() + interval '1 hour' END,
               CASE $4 WHEN 'closed' THEN 'operator' WHEN 'ai_solved' THEN 'ai' END,
               CASE WHEN $4 IN ('closed', 'ai_solved') THEN 'requester_confirmed' END)
             RETURNING id",
        )
        .bind(self.requester)
        .bind(self.category)
        .bind(self.priority)
        .bind(status)
        .bind(team)
        .bind(assignee)
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
    }

    /// Writes an event the way the writers do; the trigger enqueues in the same transaction.
    async fn event(&self, ticket: Uuid, actor: Option<Uuid>, kind: &str, payload: Value) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, payload)
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(ticket)
        .bind(actor)
        .bind(kind)
        .bind(payload)
        .fetch_one(&self.db.pool)
        .await
        .unwrap()
    }

    async fn transition(&self, ticket: Uuid, by: By, t: Transition) {
        let mut tx = self.db.pool.begin().await.unwrap();
        lifecycle::transition(&mut tx, ticket, by, &t)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn reply(&self, ticket: Uuid, who: &str, kind: &str, text: &str) -> support::Reply {
        let body = json!({ "idempotency_key": Uuid::new_v4(), "body": text });
        let cookie = self.cookie(who).await;
        call(
            &self.app,
            "POST",
            &format!("/api/tickets/{ticket}/{kind}"),
            Some(body),
            Some(&cookie),
        )
        .await
    }

    /// Runs the worker until nothing is due.
    async fn drain(&self) {
        while process_one(&self.db.pool, &self.settings).await.unwrap() {}
    }

    async fn rows(&self, account: Uuid, channel: &str) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT o.status, e.event_type FROM notification_outbox o JOIN ticket_events e ON e.id = o.event_id
             WHERE o.account_id = $1 AND o.channel = $2 ORDER BY o.created_at, o.id",
        )
        .bind(account)
        .bind(channel)
        .fetch_all(&self.db.pool)
        .await
        .unwrap()
    }

    async fn inbox(&self, who: &str) -> Vec<Value> {
        let cookie = self.cookie(who).await;
        let r = call(&self.app, "GET", "/api/notifications", None, Some(&cookie)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["items"].as_array().unwrap().clone()
    }

    fn mails(&self) -> Vec<String> {
        self.smtp
            .messages
            .lock()
            .unwrap()
            .iter()
            .map(|m| decode_mail(m))
            .collect()
    }
}

async fn count(w: &World, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(&w.db.pool).await.unwrap()
}

#[tokio::test]
async fn rolled_back_changes_enqueue_and_send_nothing() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    let mut tx = w.db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, payload) VALUES ($1, $2, 'public_reply_added', '{}')")
        .bind(ticket)
        .bind(w.op)
        .execute(&mut *tx)
        .await
        .unwrap();
    let inside: i64 = sqlx::query_scalar("SELECT count(*) FROM notification_outbox")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        inside, 2,
        "email and in-app rows are written with the event"
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        count(&w, "SELECT count(*) FROM notification_outbox").await,
        0
    );
    assert!(!process_one(&w.db.pool, &w.settings).await.unwrap());

    // A rejected API write (stale/forbidden) leaves nothing behind either.
    let r = w
        .reply(ticket, "peer", "replies", "denied: claimed by someone else")
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(
        count(&w, "SELECT count(*) FROM notification_outbox").await,
        0
    );
    assert!(w.mails().is_empty());
}

#[tokio::test]
async fn public_reply_reaches_the_requester_in_their_language_without_content() {
    let Some(w) = world().await else { return };
    sqlx::query("UPDATE accounts SET language = 'de' WHERE id = $1")
        .bind(w.requester)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let ticket = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    let r = w
        .reply(ticket, "op", "replies", "TOP-SECRET-REPLY-TEXT")
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    w.drain().await;

    wait_messages(&w.smtp.messages, 1).await;
    let mails = w.mails();
    assert_eq!(
        mails.len(),
        1,
        "the author is not notified of their own reply"
    );
    let mail = &mails[0];
    assert!(
        mail.contains("Antworten per E-Mail werden nicht verarbeitet")
            && !mail.contains("replies by email are not processed"),
        "{mail}"
    );
    assert!(
        mail.contains(&format!("http://localhost:3000/tickets/{ticket}")),
        "{mail}"
    );
    assert!(
        !mail.contains("TOP-SECRET")
            && !mail.contains("Printer subject")
            && !mail.contains("Jammed"),
        "{mail}"
    );
    assert!(mail.contains("Portal"), "{mail}");

    let items = w.inbox("requester").await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Neue öffentliche Antwort");
    assert_eq!(items[0]["href"], format!("/tickets/{ticket}"));
    assert!(!items[0].to_string().contains("TOP-SECRET"));
    assert!(w.inbox("op").await.is_empty());

    // Read state is per notification and idempotent; the count follows it.
    let cookie = w.cookie("requester").await;
    let unread = call(
        &w.app,
        "GET",
        "/api/notifications/unread",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(unread.body["count"], 1);
    let id = items[0]["id"].as_str().unwrap();
    for _ in 0..2 {
        let r = call(
            &w.app,
            "POST",
            &format!("/api/notifications/{id}/read"),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(r.status, 204);
    }
    let unread = call(
        &w.app,
        "GET",
        "/api/notifications/unread",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(unread.body["count"], 0);
    let someone_else = w.cookie("op").await;
    let r = call(
        &w.app,
        "POST",
        &format!("/api/notifications/{id}/read"),
        None,
        Some(&someone_else),
    )
    .await;
    assert_eq!(r.status, 404);
}

#[tokio::test]
async fn private_notes_never_notify_requesters() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    let r = w.reply(ticket, "op", "notes", "PRIVATE-NOTE-TEXT").await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(
        count(&w, "SELECT count(*) FROM notification_outbox").await,
        0
    );
    // Even a hand-written outbox row for a note is suppressed at delivery.
    let note = w
        .event(ticket, Some(w.op), "internal_note_added", json!({}))
        .await;
    sqlx::query("INSERT INTO notification_outbox (source_key, event_id, account_id, audience, channel) VALUES ('forced', $1, $2, 'requester', 'in_app')")
        .bind(note)
        .bind(w.requester)
        .execute(&w.db.pool)
        .await
        .unwrap();
    w.drain().await;
    assert!(w.inbox("requester").await.is_empty());
    assert!(w.mails().is_empty());
}

#[tokio::test]
async fn failed_deliveries_retry_with_backoff_and_an_admin_retry_sends_exactly_once() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    // A relay that is down: the portal write succeeds, the email fails observably.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    w.configure_smtp(closed).await;
    let r = w.reply(ticket, "op", "replies", "hello").await;
    assert_eq!(r.status, 201, "{}", r.body);

    w.drain().await;
    let (status, attempts, error): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT status, attempts, last_error FROM notification_outbox WHERE channel = 'email'",
    )
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!((status.as_str(), attempts), ("pending", 1));
    assert!(error.unwrap().starts_with("smtp_"));
    // The in-app notification was delivered independently of the email failure.
    assert_eq!(w.inbox("requester").await.len(), 1);
    // Not due again until the backoff has passed.
    w.drain().await;
    assert_eq!(
        count(
            &w,
            "SELECT attempts::bigint FROM notification_outbox WHERE channel = 'email'"
        )
        .await,
        1
    );

    // Retries are bounded: after MAX_ATTEMPTS the row is failed, not retried forever.
    for _ in 1..MAX_ATTEMPTS {
        sqlx::query("UPDATE notification_outbox SET available_at = now() WHERE status = 'pending'")
            .execute(&w.db.pool)
            .await
            .unwrap();
        w.drain().await;
    }
    let (status, attempts): (String, i32) =
        sqlx::query_as("SELECT status, attempts FROM notification_outbox WHERE channel = 'email'")
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
    assert_eq!((status.as_str(), attempts), ("failed", MAX_ATTEMPTS));
    assert!(w.mails().is_empty());

    let admin = w.cookie("admin").await;
    let stats = call(
        &w.app,
        "GET",
        "/api/admin/notifications",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(stats.body["failed"], 1);
    assert_eq!(stats.body["sent"], 1);
    assert_eq!(stats.body["failures"][0]["count"], 1);
    for who in ["requester", "op", "lead"] {
        let cookie = w.cookie(who).await;
        assert_eq!(
            call(
                &w.app,
                "GET",
                "/api/admin/notifications",
                None,
                Some(&cookie)
            )
            .await
            .status,
            403
        );
        assert_eq!(
            call(
                &w.app,
                "POST",
                "/api/admin/notifications/retry",
                None,
                Some(&cookie)
            )
            .await
            .status,
            403
        );
    }

    // The Admin repairs the relay and retries: delivered once, no duplicate inbox entry.
    w.configure_smtp(w.smtp.port).await;
    let r = call(
        &w.app,
        "POST",
        "/api/admin/notifications/retry",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(r.body["retried"], 1);
    w.drain().await;
    wait_messages(&w.smtp.messages, 1).await;
    w.drain().await;
    assert_eq!(w.mails().len(), 1);
    assert_eq!(w.inbox("requester").await.len(), 1);
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM notification_outbox WHERE status <> 'sent'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM audit_events WHERE event_type = 'notifications_retried'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn duplicate_events_and_redelivery_never_send_twice() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    // The same logical reply recorded twice names the same message: one notification.
    let message = Uuid::new_v4();
    for _ in 0..2 {
        w.event(
            ticket,
            Some(w.op),
            "public_reply_added",
            json!({ "message_id": message }),
        )
        .await;
    }
    assert_eq!(w.rows(w.requester, "email").await.len(), 1);
    assert_eq!(w.rows(w.requester, "in_app").await.len(), 1);

    // Crash after the relay accepted the message but before the outbox was updated:
    // the idempotency key makes the retry a no-op that is recorded as sent.
    let id: Uuid = sqlx::query_scalar("SELECT id FROM notification_outbox WHERE channel = 'email'")
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO smtp_deliveries (idempotency_key) VALUES ($1)")
        .bind(format!("notification:{id}"))
        .execute(&w.db.pool)
        .await
        .unwrap();
    w.drain().await;
    assert!(w.mails().is_empty());
    assert_eq!(w.rows(w.requester, "email").await[0].0, "sent");
    assert_eq!(w.inbox("requester").await.len(), 1);
    w.drain().await;
    assert_eq!(w.inbox("requester").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_workers_deliver_each_notification_once() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    for n in 0..12 {
        w.event(
            ticket,
            Some(w.op),
            "public_reply_added",
            json!({ "message_id": Uuid::new_v4(), "n": n }),
        )
        .await;
    }
    let (pool, settings) = (w.db.pool.clone(), Arc::new(w.settings.clone()));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (pool, settings) = (pool.clone(), settings.clone());
            tokio::spawn(async move {
                let mut handled = 0;
                while process_one(&pool, &settings).await.unwrap() {
                    handled += 1;
                }
                handled
            })
        })
        .collect();
    let mut handled = 0;
    for worker in workers {
        handled += worker.await.unwrap();
    }
    assert_eq!(handled, 24, "every row handled by exactly one worker");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(w.mails().len(), 12);
    assert_eq!(count(&w, "SELECT count(*) FROM notifications").await, 12);
    assert_eq!(count(&w, "SELECT count(*) FROM smtp_deliveries").await, 12);
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM notification_outbox WHERE status <> 'sent' OR attempts <> 1"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn access_is_rechecked_when_delivered() {
    let Some(w) = world().await else { return };

    // Requester replies on an unclaimed Desk ticket: every Desk member is queued.
    let ticket = w.ticket("queued", Some(w.desk), None).await;
    assert_eq!(
        w.reply(ticket, "requester", "replies", "still broken")
            .await
            .status,
        201
    );
    for who in [w.op, w.peer, w.lead] {
        assert_eq!(w.rows(who, "in_app").await.len(), 1);
    }
    assert!(
        w.rows(w.other, "in_app").await.is_empty() && w.rows(w.admin, "in_app").await.is_empty()
    );
    // Before delivery: the ticket moves to Network, one member leaves, one is deactivated.
    sqlx::query("UPDATE tickets SET team_id = $2 WHERE id = $1")
        .bind(ticket)
        .bind(w.network)
        .execute(&w.db.pool)
        .await
        .unwrap();
    w.drain().await;
    for who in [w.op, w.peer, w.lead] {
        let rows = w.rows(who, "email").await;
        assert_eq!(rows[0].0, "suppressed", "former team members get nothing");
    }
    assert!(w.mails().is_empty());
    assert!(w.inbox("op").await.is_empty() && w.inbox("lead").await.is_empty());

    // Delivered while allowed, then access is lost: the entry disappears from the inbox.
    let second = w.ticket("queued", Some(w.desk), None).await;
    assert_eq!(
        w.reply(second, "requester", "replies", "again")
            .await
            .status,
        201
    );
    w.drain().await;
    assert_eq!(w.inbox("op").await.len(), 1);
    let id = w.inbox("op").await[0]["id"].as_str().unwrap().to_owned();
    sqlx::query("DELETE FROM team_memberships WHERE account_id = $1")
        .bind(w.op)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert!(w.inbox("op").await.is_empty());
    let cookie = w.cookie("op").await;
    assert_eq!(
        call(
            &w.app,
            "GET",
            "/api/notifications/unread",
            None,
            Some(&cookie)
        )
        .await
        .body["count"],
        0
    );
    assert_eq!(
        call(
            &w.app,
            "POST",
            &format!("/api/notifications/{id}/read"),
            None,
            Some(&cookie)
        )
        .await
        .status,
        404
    );

    // A deactivated account is skipped, and a claim by someone else narrows nothing already sent.
    let third = w.ticket("queued", Some(w.desk), None).await;
    w.event(
        third,
        Some(w.requester),
        "public_reply_added",
        json!({ "message_id": Uuid::new_v4() }),
    )
    .await;
    sqlx::query("UPDATE accounts SET is_active = false WHERE id = $1")
        .bind(w.peer)
        .execute(&w.db.pool)
        .await
        .unwrap();
    w.drain().await;
    assert_eq!(
        w.rows(w.peer, "in_app").await.last().unwrap().0,
        "suppressed"
    );
}

#[tokio::test]
async fn ai_solved_records_only_notify_the_requesters_own_confirmation() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("ai_solved", None, None).await;
    w.event(ticket, Some(w.requester), "submit", json!({}))
        .await;
    w.event(ticket, Some(w.requester), "ai_solved", json!({}))
        .await;
    for who in [w.op, w.peer, w.lead, w.other, w.admin] {
        assert!(w.rows(who, "in_app").await.is_empty() && w.rows(who, "email").await.is_empty());
    }
    // A staff row that predates the solve (forced here) is dropped by the delivery check too.
    let queued = w.ticket("queued", None, None).await;
    w.event(queued, Some(w.requester), "ai_failed", json!({}))
        .await;
    for who in [w.op, w.peer, w.lead, w.other, w.admin] {
        assert_eq!(
            w.rows(who, "in_app").await.len(),
            1,
            "intake of the shared queue reaches staff"
        );
    }
    assert!(
        w.rows(w.requester, "in_app")
            .await
            .iter()
            .all(|(_, kind)| kind != "ai_failed")
    );
    sqlx::query("UPDATE tickets SET status = 'ai_solved', resolution_source = 'ai', closure_reason = 'requester_confirmed' WHERE id = $1")
        .bind(queued)
        .execute(&w.db.pool)
        .await
        .unwrap();
    w.drain().await;

    let requester = w.inbox("requester").await;
    assert_eq!(
        requester.len(),
        1,
        "only the confirmation receipt: {requester:?}"
    );
    assert_eq!(
        requester[0]["title"],
        "Receipt: you confirmed the AI solution"
    );
    for who in ["op", "peer", "lead", "other", "admin"] {
        assert!(w.inbox(who).await.is_empty(), "{who}");
    }
    assert_eq!(w.mails().len(), 1);
    assert!(w.mails()[0].contains("requester"));
}

#[tokio::test]
async fn assignment_resolution_and_cancellation_notify_the_right_accounts() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("queued", None, None).await;

    // Dispatch: the destination team hears about it, not the dispatcher or another team.
    let admin = w.cookie("admin").await;
    let at = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{ticket}"),
        None,
        Some(&admin),
    )
    .await
    .body["updated_at"]
        .clone();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/tickets/{ticket}/assignment"),
        Some(json!({"action": "dispatch", "team_id": w.desk, "expected_updated_at": at})),
        Some(&admin),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    for who in [w.op, w.peer, w.lead] {
        assert_eq!(
            w.rows(who, "in_app").await.last().unwrap().1,
            "assignment_changed"
        );
    }
    assert!(
        w.rows(w.other, "in_app")
            .await
            .iter()
            .all(|(_, kind)| kind != "assignment_changed")
    );
    // Assigning to op tells op only.
    let at = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{ticket}"),
        None,
        Some(&admin),
    )
    .await
    .body["updated_at"]
        .clone();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/tickets/{ticket}/assignment"),
        Some(json!({"action": "assign", "assignee_account_id": w.op, "expected_updated_at": at})),
        Some(&admin),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        w.rows(w.op, "in_app")
            .await
            .iter()
            .filter(|(_, k)| k == "assignment_changed")
            .count(),
        2
    );
    assert_eq!(
        w.rows(w.peer, "in_app")
            .await
            .iter()
            .filter(|(_, k)| k == "assignment_changed")
            .count(),
        1
    );

    // Resolution: proposal -> requester; rejection -> assignee only; acceptance -> closure receipt.
    w.transition(ticket, By::Account(w.op), Transition::StartWork)
        .await;
    w.transition(
        ticket,
        By::Account(w.op),
        Transition::ProposeResolution {
            summary: "SUMMARY-TEXT".into(),
        },
    )
    .await;
    w.transition(
        ticket,
        By::Account(w.requester),
        Transition::RejectResolution {
            proposal_version: 1,
        },
    )
    .await;
    w.transition(
        ticket,
        By::Account(w.op),
        Transition::ProposeResolution {
            summary: "again".into(),
        },
    )
    .await;
    w.transition(
        ticket,
        By::Account(w.requester),
        Transition::AcceptResolution {
            proposal_version: 2,
        },
    )
    .await;
    w.drain().await;
    let titles: Vec<String> = w
        .inbox("requester")
        .await
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        titles.iter().any(|t| t.starts_with("Resolution proposed")),
        "{titles:?}"
    );
    assert!(
        titles
            .iter()
            .any(|t| t == "Ticket closed: you confirmed the resolution"),
        "{titles:?}"
    );
    let op_titles: Vec<String> = w
        .inbox("op")
        .await
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        op_titles
            .iter()
            .any(|t| t.starts_with("Resolution rejected")),
        "{op_titles:?}"
    );
    assert!(w.inbox("peer").await.iter().all(|n| {
        !n["title"]
            .as_str()
            .unwrap()
            .starts_with("Resolution rejected")
    }));
    let proposal = w
        .mails()
        .into_iter()
        .find(|m| m.contains("Resolution proposed"))
        .expect("proposal mail");
    assert!(
        proposal.contains("silence is not acceptance") && proposal.contains("UTC"),
        "{proposal}"
    );
    assert!(!w.mails().iter().any(|m| m.contains("SUMMARY-TEXT")));

    // Timeout closure is identified as such, and is not acceptance.
    let timed = w
        .ticket("awaiting_confirmation", Some(w.desk), Some(w.op))
        .await;
    sqlx::query(
        "UPDATE tickets SET confirmation_deadline = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(timed)
    .execute(&w.db.pool)
    .await
    .unwrap();
    w.transition(
        timed,
        By::System,
        Transition::ConfirmationTimeout {
            proposal_version: 1,
        },
    )
    .await;
    w.drain().await;
    assert!(
        w.inbox("requester")
            .await
            .iter()
            .any(|n| n["title"].as_str().unwrap().contains("not acceptance"))
    );

    // Cancellation: receipt to the requester and the assignee, with the safe reason; nobody else.
    let cancelled = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    w.transition(
        cancelled,
        By::Account(w.requester),
        Transition::Cancel {
            reason: "Solved it myself".into(),
        },
    )
    .await;
    w.drain().await;
    let mine = w.inbox("requester").await;
    assert!(mine.iter().any(|n| n["title"] == "Ticket cancelled"
        && n["body"].as_str().unwrap().contains("Solved it myself")));
    assert!(w.inbox("op").await.iter().any(|n| n["title"] == "Ticket cancelled" && n["href"] == format!("/queue/{cancelled}")));
    assert!(
        w.inbox("peer")
            .await
            .iter()
            .all(|n| n["title"] != "Ticket cancelled")
    );
    assert_eq!(
        w.rows(w.lead, "in_app")
            .await
            .iter()
            .filter(|(_, k)| k == "cancel")
            .count(),
        0
    );

    // A request cancelled before triage never produces an operator notification.
    let pre = w.ticket("ai_processing", None, None).await;
    w.transition(
        pre,
        By::Account(w.requester),
        Transition::Cancel {
            reason: "oops".into(),
        },
    )
    .await;
    for who in [w.op, w.peer, w.lead, w.other, w.admin] {
        let rows = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_outbox o JOIN ticket_events e ON e.id = o.event_id WHERE o.account_id = $1 AND e.ticket_id = $2")
            .bind(who).bind(pre).fetch_one(&w.db.pool).await.unwrap();
        assert_eq!(rows, 0);
    }
}

#[tokio::test]
async fn preferences_apply_per_account_and_each_account_gets_its_own_mail() {
    let Some(w) = world().await else { return };
    let cookie = w.cookie("requester").await;
    let defaults = call(
        &w.app,
        "GET",
        "/api/notification-preferences",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(defaults.body, json!({"email": true, "in_app": true}));
    assert_eq!(
        call(&w.app, "GET", "/api/notification-preferences", None, None)
            .await
            .status,
        401
    );
    let bad = call(
        &w.app,
        "PUT",
        "/api/notification-preferences",
        Some(json!({"email": false, "in_app": true, "x": 1})),
        Some(&cookie),
    )
    .await;
    assert_eq!(bad.status, 422);
    let r = call(
        &w.app,
        "PUT",
        "/api/notification-preferences",
        Some(json!({"email": false, "in_app": true})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.body, json!({"email": false, "in_app": true}));

    // Requester opted out of email; the Desk members (same shared address) each get their own mail.
    let ticket = w.ticket("queued", Some(w.desk), None).await;
    w.event(ticket, Some(w.op), "request_information", json!({}))
        .await;
    w.event(
        ticket,
        Some(w.requester),
        "public_reply_added",
        json!({ "message_id": Uuid::new_v4() }),
    )
    .await;
    w.drain().await;
    wait_messages(&w.smtp.messages, 3).await;
    let mails = w.mails();
    assert_eq!(mails.len(), 3);
    for name in ["op", "peer", "lead"] {
        let mine = mails
            .iter()
            .filter(|m| m.contains(&format!("For account {name}.")))
            .count();
        assert_eq!(mine, 1, "{name}");
    }
    assert!(
        !mails.iter().any(|m| m.contains("For account requester.")),
        "opted out of email"
    );
    assert_eq!(w.inbox("requester").await.len(), 1, "in-app stays on");
    let off = call(
        &w.app,
        "PUT",
        "/api/notification-preferences",
        Some(json!({"email": true, "in_app": false})),
        Some(&cookie),
    )
    .await;
    assert_eq!(off.status, 200);
    w.event(ticket, Some(w.op), "request_information", json!({}))
        .await;
    w.drain().await;
    assert_eq!(
        w.inbox("requester").await.len(),
        1,
        "in-app opt-out suppresses new entries"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_relay_never_blocks_portal_writes() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket("in_progress", Some(w.desk), Some(w.op)).await;
    // Only the requester's email is due: a relay that accepts and then never answers.
    sqlx::query("UPDATE accounts SET notify_in_app = false WHERE id = $1")
        .bind(w.requester)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    sqlx::query("UPDATE smtp_connection SET port = $1")
        .bind(i32::from(silent.local_addr().unwrap().port()))
        .execute(&w.db.pool)
        .await
        .unwrap();
    w.event(
        ticket,
        Some(w.op),
        "public_reply_added",
        json!({ "message_id": Uuid::new_v4() }),
    )
    .await;
    let worker = tokio::spawn({
        let (pool, settings) = (w.db.pool.clone(), w.settings.clone());
        async move { while process_one(&pool, &settings).await.unwrap() {} }
    });
    // Wait until the email delivery is in flight (the in-app row is suppressed first).
    let (_held, _) = silent.accept().await.unwrap();

    // Writers on the ticket and the recipient's account do not wait for the relay.
    let mut tx = w.db.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM tickets WHERE id = $1 FOR UPDATE NOWAIT")
        .bind(ticket)
        .execute(&mut *tx)
        .await
        .expect("ticket row is not locked during SMTP");
    sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR UPDATE NOWAIT")
        .bind(w.requester)
        .execute(&mut *tx)
        .await
        .expect("account row is not locked during SMTP");
    tx.rollback().await.unwrap();
    worker.abort();
}
