//! Requester draft and submission boundary against real PostgreSQL.
use axum::{Router, http::StatusCode};
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
    cookie: String,
    category: Uuid,
    priority: Uuid,
}

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    fixtures::bootstrap_admin(&db.pool).await;
    let owner = fixtures::account(&db.pool, Role::User, "requester").await;
    let category =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let priority =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let app = test_app(&db.pool);
    let cookie = login(&app, "requester", PASSWORD).await.session();
    Some(World {
        db,
        app,
        owner,
        cookie,
        category,
        priority,
    })
}

fn intake(w: &World) -> Value {
    json!({"category_id": w.category, "priority_id": w.priority,
        "subject": "  Word crashes  ", "description": "  On startup  ", "answers": {}, "screenshot_ids": []})
}

fn submission(w: &World, key: Uuid) -> Value {
    json!({"idempotency_key": key, "intake": intake(w)})
}

async fn send(w: &World, method: &str, path: &str, body: Value) -> support::Reply {
    call(&w.app, method, path, Some(body), Some(&w.cookie)).await
}

async fn draft(w: &World, input: Value) -> Value {
    let r = send(w, "POST", "/api/ticket-drafts", input).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    r.body
}

fn id(value: &Value) -> Uuid {
    value["id"].as_str().unwrap().parse().unwrap()
}

async fn counts(w: &World) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM tickets), (SELECT count(*) FROM ticket_events), (SELECT count(*) FROM ticket_submission_keys)")
        .fetch_one(&w.db.pool).await.unwrap()
}

/// Hold the ticket insert open so a concurrent request must overlap the
/// transaction instead of slipping in after commit.
async fn hold_inserts(w: &World) {
    sqlx::query(
        "CREATE OR REPLACE FUNCTION slow_ticket_insert() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_sleep(0.4); RETURN NEW; END $$",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER slow_ticket_insert BEFORE INSERT ON tickets
         FOR EACH ROW EXECUTE FUNCTION slow_ticket_insert()",
    )
    .execute(&w.db.pool)
    .await
    .unwrap();
}

async fn release_inserts(w: &World) {
    sqlx::query("DROP TRIGGER IF EXISTS slow_ticket_insert ON tickets")
        .execute(&w.db.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn drafts_are_incomplete_owner_only_versioned_and_deletable() {
    let Some(w) = world().await else { return };
    let d = draft(&w, json!({"subject": "unfinished"})).await;
    let path = format!("/api/ticket-drafts/{}", id(&d));
    assert_eq!(d["version"], 1);
    assert_eq!(d["intake"]["subject"], "unfinished");
    assert_eq!(
        counts(&w).await,
        (0, 0, 0),
        "drafts never enter ticket queues"
    );
    let r = call(&w.app, "GET", &path, None, Some(&w.cookie)).await;
    assert_eq!(r.body, d);
    let r = call(
        &w.app,
        "GET",
        "/api/ticket-drafts?limit=1",
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(r.body["items"], json!([d]));
    let other = login(&w.app, "admin", PASSWORD).await.session();
    for (method, body) in [
        ("GET", None),
        ("PATCH", Some(json!({"version": 1, "intake": {}}))),
        ("DELETE", None),
    ] {
        let url = if method == "DELETE" {
            format!("{path}?version=1")
        } else {
            path.clone()
        };
        let r = call(&w.app, method, &url, body, Some(&other)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{method}: {}", r.body);
    }
    let r = send(
        &w,
        "PATCH",
        &path,
        json!({"version": 1, "intake": {"subject": "new text"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["version"], 2);
    let r = send(
        &w,
        "PATCH",
        &path,
        json!({"version": 1, "intake": {"subject": "stale text"}}),
    )
    .await;
    assert_eq!(r.code(), "conflict");
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = call(
        &w.app,
        "DELETE",
        &format!("{path}?version=1"),
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = call(&w.app, "GET", &path, None, Some(&w.cookie)).await;
    assert_eq!(r.body["intake"]["subject"], "new text");
    let r = call(
        &w.app,
        "DELETE",
        &format!("{path}?version=2"),
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let r = call(&w.app, "GET", &path, None, Some(&w.cookie)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM audit_events WHERE event_type LIKE 'draft_%' ORDER BY id",
    )
    .fetch_all(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(events, ["draft_created", "draft_updated", "draft_deleted"]);
}

#[tokio::test]
async fn submission_freezes_intake_and_routes_once_with_ai_disabled() {
    let Some(w) = world().await else { return };
    // Exemption is subordinate to global AI-unavailable routing.
    sqlx::query("UPDATE categories SET skip_ai = true WHERE id = $1")
        .bind(w.category)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let body = submission(&w, Uuid::new_v4());
    let r = send(&w, "POST", "/api/tickets", body.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["status"], "queued");
    let ticket = id(&r.body);
    assert!(r.body["reference"].as_i64().unwrap() > 0);
    let row: (Uuid, Uuid, Option<Uuid>, Option<Uuid>, Value) = sqlx::query_as(
        "SELECT requester_account_id, priority_id, team_id, assignee_account_id, intake_snapshot FROM tickets WHERE id = $1")
        .bind(ticket).fetch_one(&w.db.pool).await.unwrap();
    assert_eq!(
        (row.0, row.1, row.2, row.3),
        (w.owner, w.priority, None, None)
    );
    let snapshot = row.4;
    assert_eq!(snapshot["subject"], "Word crashes");
    assert_eq!(snapshot["description"], "On startup");
    assert_eq!(snapshot["catalog"]["category_name"], "Software");
    assert_eq!(snapshot["catalog"]["priority_id"], json!(w.priority));
    assert_eq!(snapshot["screenshot_ids"], json!([]));
    let event: (Uuid, String, String, Value) = sqlx::query_as(
        "SELECT actor_account_id, from_status::text, to_status::text, payload FROM ticket_events WHERE ticket_id = $1")
        .bind(ticket).fetch_one(&w.db.pool).await.unwrap();
    assert_eq!(
        event,
        (
            w.owner,
            "draft".into(),
            "queued".into(),
            json!({"route": "ai_disabled"})
        )
    );
    assert_eq!(counts(&w).await, (1, 1, 1));
    let repeat = send(&w, "POST", "/api/tickets", body).await;
    assert_eq!(repeat.status, StatusCode::OK, "{}", repeat.body);
    assert_eq!(repeat.body, r.body);
    assert_eq!(counts(&w).await, (1, 1, 1));
    // Catalog renames and effective priority changes never rewrite original intake.
    sqlx::query("UPDATE categories SET name = 'Renamed', archived_at = now() WHERE id = $1")
        .bind(w.category)
        .execute(&w.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE priorities SET name = 'Renamed', archived_at = now() WHERE id = $1")
        .bind(w.priority)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let new_priority: Uuid =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Urgent') RETURNING id")
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
    sqlx::query("UPDATE tickets SET priority_id = $2 WHERE id = $1")
        .bind(ticket)
        .bind(new_priority)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{ticket}"),
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["category_name"], "Software");
    assert_eq!(r.body["priority_id"], json!(new_priority));
    assert_eq!(r.body["intake_snapshot"], snapshot);
    let e = sqlx::query("UPDATE tickets SET intake_snapshot = '{}' WHERE id = $1")
        .bind(ticket)
        .execute(&w.db.pool)
        .await
        .unwrap_err();
    assert_eq!(
        e.as_database_error().unwrap().code().as_deref(),
        Some("23001")
    );
}

#[tokio::test]
async fn submission_validates_intake_and_keeps_failed_drafts_recoverable() {
    let Some(w) = world().await else { return };
    let d = draft(&w, json!({"subject": "recover me"})).await;
    let path = format!("/api/ticket-drafts/{}/submit", id(&d));
    let key = Uuid::new_v4();
    let body = json!({"version": 1, "idempotency_key": key});
    let r = send(&w, "POST", &path, body.clone()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    assert_eq!(
        r.body["error"]["details"]["fields"]["category_id"],
        json!(["required"])
    );
    let r = call(
        &w.app,
        "GET",
        &format!("/api/ticket-drafts/{}", id(&d)),
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(r.body, d);
    assert_eq!(counts(&w).await, (0, 0, 0));
    for (field, value, code) in [
        ("category_id", json!(Uuid::new_v4()), "invalid"),
        ("priority_id", json!(Uuid::new_v4()), "invalid"),
        ("subject", json!("  "), "required"),
        ("description", json!(""), "required"),
        ("subject", json!("x".repeat(201)), "too_long"),
        ("description", json!("x".repeat(20001)), "too_long"),
        ("screenshot_ids", json!([Uuid::new_v4()]), "invalid"),
    ] {
        let mut body = submission(&w, key);
        body["intake"][field] = value;
        let r = send(&w, "POST", "/api/tickets", body).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}: {}",
            r.body
        );
        assert_eq!(r.body["error"]["details"]["fields"][field], json!([code]));
        assert_eq!(counts(&w).await, (0, 0, 0));
    }
    sqlx::query("UPDATE priorities SET archived_at = now() WHERE id = $1")
        .bind(w.priority)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = send(&w, "POST", "/api/tickets", submission(&w, key)).await;
    assert_eq!(
        r.body["error"]["details"]["fields"]["priority_id"],
        json!(["invalid"])
    );
    sqlx::query("UPDATE priorities SET archived_at = NULL WHERE id = $1")
        .bind(w.priority)
        .execute(&w.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE categories SET archived_at = now() WHERE id = $1")
        .bind(w.category)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let r = send(&w, "POST", "/api/tickets", submission(&w, key)).await;
    assert_eq!(
        r.body["error"]["details"]["fields"]["category_id"],
        json!(["invalid"])
    );
}

#[tokio::test]
async fn conditional_answers_are_validated_and_only_visible_answers_are_frozen() {
    let Some(w) = world().await else { return };
    let field: Uuid = sqlx::query_scalar("INSERT INTO intake_fields (label, kind, required, allow_other) VALUES ('Program', 'select', true, true) RETURNING id")
        .fetch_one(&w.db.pool).await.unwrap();
    sqlx::query("INSERT INTO intake_field_categories VALUES ($1, $2)")
        .bind(field)
        .bind(w.category)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let choice: Uuid = sqlx::query_scalar(
        "INSERT INTO intake_field_choices (field_id, label) VALUES ($1, 'Word') RETURNING id",
    )
    .bind(field)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    let hidden: Uuid = sqlx::query_scalar("INSERT INTO intake_fields (label, kind, required) VALUES ('Hidden', 'text', true) RETURNING id")
        .fetch_one(&w.db.pool).await.unwrap();
    let mut body = submission(&w, Uuid::new_v4());
    let r = send(&w, "POST", "/api/tickets", body.clone()).await;
    assert_eq!(
        r.body["error"]["details"]["fields"][format!("answers.{field}")],
        json!(["required"])
    );
    let unknown = Uuid::new_v4();
    body["intake"]["answers"] = json!({unknown.to_string(): {"text": "forged"}});
    let r = send(&w, "POST", "/api/tickets", body.clone()).await;
    assert_eq!(
        r.body["error"]["details"]["fields"][format!("answers.{unknown}")],
        json!(["unknown_field"])
    );
    for answer in [
        json!({"choice_id": choice}),
        json!({"text": "  Custom program  "}),
    ] {
        body["idempotency_key"] = json!(Uuid::new_v4());
        body["intake"]["answers"] = json!({field.to_string(): answer.clone(), hidden.to_string(): {"text": "stale secret"}});
        let r = send(&w, "POST", "/api/tickets", body.clone()).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        let detail = call(
            &w.app,
            "GET",
            &format!("/api/tickets/{}", id(&r.body)),
            None,
            Some(&w.cookie),
        )
        .await;
        let answers = detail.body["intake_snapshot"]["catalog"]["answers"]
            .as_array()
            .unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0]["field_id"], json!(field));
        assert_eq!(
            answers[0]["value"],
            if answer["choice_id"].is_string() {
                "Word"
            } else {
                "Custom program"
            }
        );
        let snapshot: String =
            sqlx::query_scalar("SELECT intake_snapshot::text FROM tickets WHERE id = $1")
                .bind(id(&r.body))
                .fetch_one(&w.db.pool)
                .await
                .unwrap();
        assert!(!snapshot.contains("stale secret"));
    }
}

#[tokio::test]
async fn stale_and_submitted_drafts_cannot_be_edited_deleted_or_resubmitted() {
    let Some(w) = world().await else { return };
    let d = draft(&w, intake(&w)).await;
    let path = format!("/api/ticket-drafts/{}", id(&d));
    let key = Uuid::new_v4();
    let r = send(
        &w,
        "PATCH",
        &path,
        json!({"version": 1, "intake": intake(&w)}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &w,
        "POST",
        &format!("{path}/submit"),
        json!({"version": 1, "idempotency_key": key}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(counts(&w).await, (0, 0, 0));
    let body = json!({"version": 2, "idempotency_key": key});
    let submitted = send(&w, "POST", &format!("{path}/submit"), body.clone()).await;
    assert_eq!(submitted.status, StatusCode::CREATED, "{}", submitted.body);
    let retry = send(&w, "POST", &format!("{path}/submit"), body).await;
    assert_eq!(retry.status, StatusCode::OK);
    assert_eq!(retry.body, submitted.body);
    for (method, url, body) in [
        (
            "PATCH",
            path.clone(),
            Some(json!({"version": 2, "intake": {"subject": "rewrite"}})),
        ),
        ("DELETE", format!("{path}?version=2"), None),
        (
            "POST",
            format!("{path}/submit"),
            Some(json!({"version": 2, "idempotency_key": Uuid::new_v4()})),
        ),
    ] {
        let r = call(&w.app, method, &url, body, Some(&w.cookie)).await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{method}: {}", r.body);
    }
    let r = call(&w.app, "GET", "/api/ticket-drafts", None, Some(&w.cookie)).await;
    assert_eq!(r.body["items"], json!([]));
    assert_eq!(counts(&w).await, (1, 1, 1));
}

#[tokio::test]
async fn duplicate_keys_are_atomic_payload_bound_and_account_scoped() {
    let Some(w) = world().await else { return };
    let key = Uuid::new_v4();
    let body = submission(&w, key);
    // The insert sleeps inside the winner's transaction, so the loser has to
    // block on the account/key advisory lock rather than run after commit.
    hold_inserts(&w).await;
    let (a, b) = tokio::join!(
        send(&w, "POST", "/api/tickets", body.clone()),
        send(&w, "POST", "/api/tickets", body.clone())
    );
    release_inserts(&w).await;
    assert!(a.status.is_success(), "{}", a.body);
    assert!(b.status.is_success(), "{}", b.body);
    assert!(
        (a.status == StatusCode::CREATED && b.status == StatusCode::OK)
            || (a.status == StatusCode::OK && b.status == StatusCode::CREATED),
        "one create and one replay, got {} and {}",
        a.status,
        b.status
    );
    assert_eq!(a.body, b.body);
    assert_eq!(a.body["status"], "queued");
    assert_eq!(counts(&w).await, (1, 1, 1));
    let mut changed = body.clone();
    changed["intake"]["subject"] = json!("different request");
    let r = send(&w, "POST", "/api/tickets", changed).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "conflict");
    let d = draft(&w, intake(&w)).await;
    let r = send(
        &w,
        "POST",
        &format!("/api/ticket-drafts/{}/submit", id(&d)),
        json!({"version": 1, "idempotency_key": key}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "same key on the other endpoint: {}",
        r.body
    );
    assert_eq!(r.code(), "conflict");
    assert_eq!(counts(&w).await, (1, 1, 1));
    // Replay still returns the original receipt even after lifecycle state changes.
    let mut tx = w.db.pool.begin().await.unwrap();
    tikkit_api::lifecycle::transition(
        &mut tx,
        id(&a.body),
        tikkit_api::lifecycle::By::Account(w.owner),
        &tikkit_api::lifecycle::Transition::Cancel {
            reason: "No longer needed".into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let retry = send(&w, "POST", "/api/tickets", body.clone()).await;
    assert_eq!(retry.status, StatusCode::OK);
    assert_eq!(retry.body, a.body);
    let live: String = sqlx::query_scalar("SELECT status::text FROM tickets WHERE id = $1")
        .bind(id(&a.body))
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(live, "cancelled");
    assert_eq!(
        counts(&w).await,
        (1, 2, 1),
        "replay must not write another ticket or event"
    );
    let other = fixtures::account(&w.db.pool, Role::User, "other").await;
    let cookie = login(&w.app, "other", PASSWORD).await.session();
    let r = call(&w.app, "POST", "/api/tickets", Some(body), Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_ne!(r.body["id"], a.body["id"]);
    let owner: Uuid = sqlx::query_scalar("SELECT requester_account_id FROM tickets WHERE id = $1")
        .bind(id(&r.body))
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(
        owner, other,
        "shared email never scopes ownership/idempotency"
    );
}

#[tokio::test]
async fn concurrent_draft_submit_and_edit_have_one_winner() {
    let Some(w) = world().await else { return };
    let d = draft(&w, intake(&w)).await;
    let path = format!("/api/ticket-drafts/{}", id(&d));
    let submit_path = format!("{path}/submit");
    hold_inserts(&w).await;
    let a_key = Uuid::new_v4();
    let b_key = Uuid::new_v4();
    let (a, b) = tokio::join!(
        send(
            &w,
            "POST",
            &submit_path,
            json!({"version": 1, "idempotency_key": a_key})
        ),
        send(
            &w,
            "POST",
            &submit_path,
            json!({"version": 1, "idempotency_key": b_key})
        )
    );
    assert!(
        matches!(
            (a.status, b.status),
            (StatusCode::CREATED, StatusCode::CONFLICT)
                | (StatusCode::CONFLICT, StatusCode::CREATED)
        ),
        "{} / {}",
        a.body,
        b.body
    );
    assert_eq!(counts(&w).await, (1, 1, 1));
    let d = draft(&w, intake(&w)).await;
    let path = format!("/api/ticket-drafts/{}", id(&d));
    let submit_path = format!("{path}/submit");
    let (a, b) = tokio::join!(
        send(
            &w,
            "POST",
            &submit_path,
            json!({"version": 1, "idempotency_key": Uuid::new_v4()})
        ),
        send(
            &w,
            "PATCH",
            &path,
            json!({"version": 1, "intake": {"subject": "new unsent text"}})
        )
    );
    assert!(
        matches!(
            (a.status, b.status),
            (StatusCode::CREATED, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
        ),
        "{} / {}",
        a.body,
        b.body
    );
    release_inserts(&w).await;
    let (tickets, events, keys) = counts(&w).await;
    assert_eq!((events, keys), (tickets, tickets));
    let row: (Option<String>, Option<Uuid>) = sqlx::query_as(
        "SELECT intake->>'subject', submitted_ticket_id FROM ticket_drafts WHERE id = $1",
    )
    .bind(id(&d))
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    if a.status == StatusCode::CREATED {
        assert_eq!(tickets, 2, "the edit must roll back when submit commits");
        assert!(row.0.is_none() && row.1.is_some());
    } else {
        assert_eq!(tickets, 1, "submit must roll back when the edit commits");
        assert_eq!(row.0.as_deref(), Some("new unsent text"));
        assert!(row.1.is_none());
    }
}

#[tokio::test]
async fn transaction_and_commit_failures_leave_no_partial_ticket_or_consumed_key() {
    let Some(w) = world().await else { return };
    for deferred in [false, true] {
        let d = draft(&w, intake(&w)).await;
        let path = format!("/api/ticket-drafts/{}/submit", id(&d));
        let body = json!({"version": 1, "idempotency_key": Uuid::new_v4()});
        sqlx::query("CREATE OR REPLACE FUNCTION fail_submission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected submission failure'; END $$")
            .execute(&w.db.pool).await.unwrap();
        let sql = if deferred {
            "CREATE CONSTRAINT TRIGGER fail_submission AFTER INSERT ON ticket_events DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fail_submission()"
        } else {
            "CREATE TRIGGER fail_submission BEFORE INSERT ON ticket_events FOR EACH ROW EXECUTE FUNCTION fail_submission()"
        };
        sqlx::query(sql).execute(&w.db.pool).await.unwrap();
        let before = counts(&w).await;
        let r = send(&w, "POST", &path, body.clone()).await;
        assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR, "{}", r.body);
        assert_eq!(r.code(), "internal");
        assert!(!r.body.to_string().contains("injected"));
        assert_eq!(counts(&w).await, before);
        let resumed = call(
            &w.app,
            "GET",
            &format!("/api/ticket-drafts/{}", id(&d)),
            None,
            Some(&w.cookie),
        )
        .await;
        assert_eq!(resumed.body, d);
        sqlx::query("DROP TRIGGER fail_submission ON ticket_events")
            .execute(&w.db.pool)
            .await
            .unwrap();
        let r = send(&w, "POST", &path, body).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    }
    assert_eq!(counts(&w).await, (2, 2, 2));
    // A direct submission is equally atomic.
    sqlx::query("CREATE TRIGGER fail_submission BEFORE INSERT ON ticket_events FOR EACH ROW EXECUTE FUNCTION fail_submission()")
        .execute(&w.db.pool).await.unwrap();
    let body = submission(&w, Uuid::new_v4());
    assert_eq!(
        send(&w, "POST", "/api/tickets", body.clone()).await.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(counts(&w).await, (2, 2, 2));
    sqlx::query("DROP TRIGGER fail_submission ON ticket_events")
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(
        send(&w, "POST", "/api/tickets", body).await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn requester_lists_are_paginated_owner_only_and_hide_staff_data() {
    let Some(w) = world().await else { return };
    for _ in 0..3 {
        assert_eq!(
            send(&w, "POST", "/api/tickets", submission(&w, Uuid::new_v4()))
                .await
                .status,
            StatusCode::CREATED
        );
    }
    let admin = login(&w.app, "admin", PASSWORD).await.session();
    let r = call(
        &w.app,
        "POST",
        "/api/tickets",
        Some(submission(&w, Uuid::new_v4())),
        Some(&admin),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let other_ticket = id(&r.body);
    let r = call(&w.app, "GET", "/api/tickets?limit=2", None, Some(&w.cookie)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["items"].as_array().unwrap().len(), 2);
    for row in r.body["items"].as_array().unwrap() {
        assert_ne!(id(row), other_ticket);
        assert!(row.get("requester_account_id").is_none());
        assert!(row.get("assignee_account_id").is_none());
        assert_eq!(
            row["assignment"],
            json!({"team_name": null, "assignee_display_name": null})
        );
    }
    let cursor = r.body["next_cursor"].as_str().unwrap();
    let second = call(
        &w.app,
        "GET",
        &format!("/api/tickets?limit=2&cursor={cursor}"),
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(second.body["items"].as_array().unwrap().len(), 1);
    assert!(second.body["next_cursor"].is_null());
    assert!(
        !r.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == second.body["items"][0]["id"])
    );
    let r = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{other_ticket}"),
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(&w.app, "GET", "/api/tickets", None, Some(&admin)).await;
    assert_eq!(
        r.body["items"].as_array().unwrap().len(),
        1,
        "even Admin requester list is own-only"
    );
    let r = call(
        &w.app,
        "GET",
        "/api/tickets?cursor=bad",
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn all_roles_submit_for_themselves_and_reject_forged_ownership_and_policy() {
    let Some(w) = world().await else { return };
    for (role, name) in [
        (Role::Operator, "operator"),
        (Role::Specialist, "specialist"),
        (Role::TeamLead, "lead"),
    ] {
        let account = fixtures::account(&w.db.pool, role, name).await;
        let cookie = login(&w.app, name, PASSWORD).await.session();
        let r = call(
            &w.app,
            "POST",
            "/api/tickets",
            Some(submission(&w, Uuid::new_v4())),
            Some(&cookie),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        let owner: Uuid =
            sqlx::query_scalar("SELECT requester_account_id FROM tickets WHERE id = $1")
                .bind(id(&r.body))
                .fetch_one(&w.db.pool)
                .await
                .unwrap();
        assert_eq!(owner, account);
    }
    for field in [
        "requester_account_id",
        "status",
        "team_id",
        "skip_ai",
        "ai_available",
    ] {
        let mut body = submission(&w, Uuid::new_v4());
        body[field] = json!(w.owner);
        let r = send(&w, "POST", "/api/tickets", body).await;
        assert_eq!(r.code(), "unknown_field", "{}", r.body);
    }
    let mut body = intake(&w);
    body["requester_account_id"] = json!(w.owner);
    assert_eq!(
        send(&w, "POST", "/api/ticket-drafts", body).await.code(),
        "unknown_field"
    );
    let r = call(
        &w.app,
        "POST",
        "/api/tickets",
        Some(submission(&w, Uuid::new_v4())),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let d = draft(&w, intake(&w)).await;
    let other = login(&w.app, "admin", PASSWORD).await.session();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/ticket-drafts/{}/submit", id(&d)),
        Some(json!({"version": 1, "idempotency_key": Uuid::new_v4()})),
        Some(&other),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn inactive_answers_are_discarded_before_active_answer_validation() {
    let Some(w) = world().await else { return };
    let hidden: Uuid = sqlx::query_scalar(
        "INSERT INTO intake_fields (label, kind) VALUES ('Hidden', 'text') RETURNING id",
    )
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    let active: Uuid = sqlx::query_scalar(
        "INSERT INTO intake_fields (label, kind) VALUES ('Active', 'text') RETURNING id",
    )
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO intake_field_categories VALUES ($1, $2)")
        .bind(active)
        .bind(w.category)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let mut body = submission(&w, Uuid::new_v4());
    body["intake"]["answers"] = json!({hidden.to_string(): {"text": "x".repeat(501)}});
    let r = send(&w, "POST", "/api/tickets", body.clone()).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "hidden stale answers do not constrain submission: {}",
        r.body
    );
    body["idempotency_key"] = json!(Uuid::new_v4());
    body["intake"]["answers"] = json!({active.to_string(): {"text": "text\u{0000}with NUL"}});
    let r = send(&w, "POST", "/api/tickets", body).await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid text must not become a database error: {}",
        r.body
    );
    assert_eq!(
        r.body["error"]["details"]["fields"][format!("answers.{active}")],
        json!(["invalid"])
    );
    assert_eq!(counts(&w).await, (1, 1, 1));
}

#[tokio::test]
async fn pagination_of_drafts_and_intake_limits_are_bounded() {
    let Some(w) = world().await else { return };
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.push(draft(&w, json!({})).await["id"].clone());
    }
    let first = call(
        &w.app,
        "GET",
        "/api/ticket-drafts?limit=2",
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(first.body["items"].as_array().unwrap().len(), 2);
    let cursor = first.body["next_cursor"].as_str().unwrap();
    let last = call(
        &w.app,
        "GET",
        &format!("/api/ticket-drafts?limit=2&cursor={cursor}"),
        None,
        Some(&w.cookie),
    )
    .await;
    assert_eq!(last.body["items"].as_array().unwrap().len(), 1);
    let actual: Vec<Value> = first.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(last.body["items"].as_array().unwrap())
        .map(|d| d["id"].clone())
        .collect();
    expected.reverse();
    assert_eq!(actual, expected);
    let r = send(
        &w,
        "POST",
        "/api/ticket-drafts",
        json!({"subject": "x".repeat(201)}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = send(
        &w,
        "POST",
        "/api/ticket-drafts",
        json!({"subject": "\u{0000}"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let answers: serde_json::Map<String, Value> = (0..101)
        .map(|_| (Uuid::new_v4().to_string(), json!({"text": "x"})))
        .collect();
    let r = send(
        &w,
        "POST",
        "/api/ticket-drafts",
        json!({"answers": answers}),
    )
    .await;
    assert_eq!(
        r.body["error"]["details"]["fields"]["answers"],
        json!(["too_many"])
    );
}
