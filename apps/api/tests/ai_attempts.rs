//! AI suggestion attempts end to end against real PostgreSQL and a local mock
//! provider (docs/ai-attempts.md): routing, the immutable snapshot, the worker,
//! handoff exactly once, invalidation, cancellation, restart recovery, late
//! results, and image preparation. No test talks to a real provider.

mod support;

use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use base64::Engine;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login,
    provider_mock::{Mock, Reply, serve},
    test_app, test_state,
};
use tikkit_api::{
    ai::{AiClient, CallOptions, Endpoint, MAX_IMAGE_BYTES},
    ai_attempts::{self, Worker, WorkerOptions},
    ai_guard::AiGuard,
    api::ApiError,
    lifecycle::{self, By, Policy, Transition},
};
use tower::ServiceExt;
use uuid::Uuid;

/// Deliberately not shaped like a real provider key.
const SECRET: &str = "test-key-not-real-0001";
/// `Settings::default()` in tests.
const KEY: [u8; 32] = [0; 32];

// --- Fixtures ------------------------------------------------------------------

struct World {
    db: TestDb,
    app: Router,
    mock: Mock,
    admin: String,
    requester: String,
    requester_id: Uuid,
    operator: String,
    category: Uuid,
    priority: Uuid,
}

fn first_model() -> &'static str {
    tikkit_api::ai::supported_models(tikkit_api::ai::Provider::OpenAi)[0].id
}

/// A deployment with AI enabled (OpenAI, mock endpoint), a requester, an
/// operator, and a catalog.
async fn world(replies: Vec<Reply>) -> Option<World> {
    let db = TestDb::new().await?;
    fixtures::bootstrap_admin(&db.pool).await;
    let requester_id = fixtures::account(&db.pool, Role::User, "pat.jones").await;
    fixtures::account(&db.pool, Role::Operator, "operator").await;
    let app = test_app(&db.pool);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let requester = login(&app, "pat.jones", PASSWORD).await.session();
    let operator = login(&app, "operator", PASSWORD).await.session();
    let category: Uuid =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let priority: Uuid =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let mock = serve(replies).await;
    let w = World {
        db,
        app,
        mock,
        admin,
        requester,
        requester_id,
        operator,
        category,
        priority,
    };
    w.set_key(SECRET).await;
    w.select(first_model(), true).await;
    Some(w)
}

impl World {
    fn pool(&self) -> &PgPool {
        &self.db.pool
    }

    async fn set_key(&self, key: &str) {
        let body = json!({ "api_key": key });
        let path = "/api/settings/ai/credentials/openai";
        let r = call(&self.app, "PUT", path, Some(body), Some(&self.admin)).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }

    async fn select(&self, model: &str, enabled: bool) {
        let body = json!({ "enabled": enabled, "provider": "openai", "model": model });
        let r = call(
            &self.app,
            "PUT",
            "/api/settings/ai",
            Some(body),
            Some(&self.admin),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }

    /// A worker whose provider calls go to the mock, with short timings.
    fn worker(&self) -> Worker {
        self.worker_with(Duration::from_secs(10), Duration::from_millis(100))
    }

    fn worker_with(&self, call_timeout: Duration, heartbeat: Duration) -> Worker {
        let client = AiClient::with_options(CallOptions {
            request_timeout: call_timeout,
            total_timeout: call_timeout,
            max_retries: 0,
            retry_backoff: Duration::from_millis(5),
        });
        let endpoint = Endpoint::Override {
            base_url: self.mock.url.clone(),
            allow_private_network: true,
        };
        let guard = AiGuard::new(self.pool().clone(), client, endpoint);
        Worker::with(&test_state(self.pool()), guard, KEY).options(WorkerOptions {
            lease: Duration::from_millis(600),
            heartbeat,
            saturated_retry: Duration::from_millis(100),
        })
    }

    fn intake(&self) -> Value {
        json!({ "category_id": self.category, "priority_id": self.priority,
            "subject": "Word crashes", "description": "It closes on startup.",
            "answers": {}, "screenshot_ids": [] })
    }

    async fn submit_with(&self, intake: Value, skip: bool) -> support::Reply {
        let body =
            json!({ "idempotency_key": Uuid::new_v4(), "intake": intake, "requester_skip": skip });
        call(
            &self.app,
            "POST",
            "/api/tickets",
            Some(body),
            Some(&self.requester),
        )
        .await
    }

    /// Submits the default intake and returns the ticket, which must be in `status`.
    async fn submit(&self, status: &str) -> Uuid {
        let r = self.submit_with(self.intake(), false).await;
        assert_eq!(r.status, 201, "{}", r.body);
        assert_eq!(r.body["status"], status, "{}", r.body);
        serde_json::from_value(r.body["id"].clone()).unwrap()
    }

    async fn status(&self, ticket: Uuid) -> String {
        sqlx::query_scalar("SELECT status::text FROM tickets WHERE id = $1")
            .bind(ticket)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    /// `(state, outcome, has suggestion)` of the ticket's attempt.
    async fn attempt(&self, ticket: Uuid) -> Option<(String, Option<String>, bool)> {
        sqlx::query_as(
            "SELECT state, outcome, explanation IS NOT NULL FROM ai_attempts WHERE ticket_id = $1",
        )
        .bind(ticket)
        .fetch_optional(self.pool())
        .await
        .unwrap()
    }

    async fn events(&self, ticket: Uuid, event_type: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM ticket_events WHERE ticket_id = $1 AND event_type = $2",
        )
        .bind(ticket)
        .bind(event_type)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// How often the ticket entered the operator queue.
    async fn queued(&self, ticket: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM ticket_events WHERE ticket_id = $1 AND to_status = 'queued'",
        )
        .bind(ticket)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn detail(&self, ticket: Uuid, cookie: &str) -> support::Reply {
        let path = format!("/api/tickets/{ticket}");
        call(&self.app, "GET", &path, None, Some(cookie)).await
    }

    /// The requester's view of the attempt.
    async fn ai(&self, ticket: Uuid) -> Value {
        let r = self.detail(ticket, &self.requester).await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["ai"].clone()
    }

    /// Whether the operator finds the ticket in the general queue.
    async fn in_queue(&self, ticket: Uuid) -> bool {
        let r = call(&self.app, "GET", "/api/queue", None, Some(&self.operator)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let items = r.body["items"].as_array().unwrap();
        items.iter().any(|t| t["id"] == json!(ticket))
    }

    /// The ticket was handed to operators exactly once, with `outcome`, and no
    /// suggestion was stored.
    async fn assert_handed_off(&self, ticket: Uuid, outcome: &str) {
        assert_eq!(self.status(ticket).await, "queued");
        assert_eq!(
            self.attempt(ticket).await,
            Some(("handed_off".into(), Some(outcome.into()), false))
        );
        assert_eq!(self.queued(ticket).await, 1);
        assert_eq!(self.events(ticket, "ai_failed").await, 1);
        assert_eq!(self.events(ticket, "ai_suggestion_ready").await, 0);
        let team: Option<Uuid> = sqlx::query_scalar("SELECT team_id FROM tickets WHERE id = $1")
            .bind(ticket)
            .fetch_one(self.pool())
            .await
            .unwrap();
        assert_eq!(team, None);
        assert!(self.in_queue(ticket).await);
    }

    async fn sql(&self, statement: &str) {
        sqlx::query(statement).execute(self.pool()).await.unwrap();
    }

    /// Moves time for a ticket's attempt: its deadline becomes now plus
    /// `seconds`. The trigger keeps the deadline immutable in normal operation,
    /// so the test (as table owner) steps around it.
    async fn deadline_in(&self, ticket: Uuid, seconds: f64) {
        let mut tx = self.pool().begin().await.unwrap();
        sqlx::query("ALTER TABLE ai_attempts DISABLE TRIGGER ai_attempts_guard")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE ai_attempts SET deadline_at = clock_timestamp() + make_interval(secs => $2)
             WHERE ticket_id = $1",
        )
        .bind(ticket)
        .bind(seconds)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("ALTER TABLE ai_attempts ENABLE TRIGGER ai_attempts_guard")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn overdue(&self, ticket: Uuid) {
        self.deadline_in(ticket, -1.0).await;
    }

    /// `(claims, has a claim)` of the ticket's attempt.
    async fn claims(&self, ticket: Uuid) -> (i32, bool) {
        sqlx::query_as(
            "SELECT claims, claimed_until IS NOT NULL FROM ai_attempts WHERE ticket_id = $1",
        )
        .bind(ticket)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// Takes every provider slot of the guard for `seconds`, as calls of
    /// another process would.
    async fn occupy_slots(&self, seconds: f64) {
        sqlx::query(
            "INSERT INTO ai_usage (account_id, provider, model, image_count, status,
                                   budget_tokens, budget_basis, lease_expires_at)
             SELECT a.id, 'openai', $1, 0, 'reserved', 0, 'estimated',
                    clock_timestamp() + make_interval(secs => $2)
             FROM accounts a, generate_series(1, (SELECT max_concurrency FROM ai_limits))
             WHERE a.username = 'operator'",
        )
        .bind(first_model())
        .bind(seconds)
        .execute(self.pool())
        .await
        .unwrap();
    }

    async fn wait_for_calls(&self, calls: usize) {
        for _ in 0..500 {
            if self.mock.count() >= calls {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the provider was not called");
    }
}

/// A successful OpenAI Responses reply.
fn success() -> Reply {
    success_with(
        "The profile is damaged.",
        &["Close Word", "Start Word again"],
    )
}

fn success_with(explanation: &str, steps: &[&str]) -> Reply {
    let text = json!({ "explanation": explanation, "steps": steps }).to_string();
    Reply::json(
        200,
        json!({
            "id": "resp_1", "object": "response", "status": "completed",
            "output": [
                { "type": "message", "id": "msg_1", "status": "completed", "role": "assistant",
                  "content": [{ "type": "output_text", "text": text, "annotations": [] }] },
            ],
            "usage": { "input_tokens": 120, "output_tokens": 45, "total_tokens": 165 },
        }),
    )
}

fn failure(status: u16) -> Reply {
    Reply::json(
        status,
        json!({ "error": { "message": "nope", "type": "api_error", "code": "server_error" } }),
    )
}

async fn upload(app: &Router, cookie: &str, bytes: Vec<u8>) -> Uuid {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/screenshots?filename=shot.png")
        .header("x-requested-with", "tikkit")
        .header("cookie", cookie)
        .header("content-type", "image/png")
        .body(Body::from(bytes))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))));
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    serde_json::from_value(body["id"].clone()).unwrap()
}

fn png(image: &image::RgbImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Incompressible pixels, deterministic.
fn noise(width: u32, height: u32) -> image::RgbImage {
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    image::RgbImage::from_fn(width, height, |_, _| {
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 24) as u8
        };
        image::Rgb([next(), next(), next()])
    })
}

/// The images of an OpenAI Responses request, as `(media type, bytes)`.
fn sent_images(body: &Value) -> Vec<(String, Vec<u8>)> {
    let mut images = Vec::new();
    for item in body["input"].as_array().unwrap() {
        for part in item["content"].as_array().unwrap() {
            if part["type"] == "input_image" {
                let url = part["image_url"].as_str().unwrap();
                let (head, data) = url.split_once(";base64,").unwrap();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap();
                images.push((head.trim_start_matches("data:").to_owned(), bytes));
            }
        }
    }
    images
}

// --- Routing and the happy path ---------------------------------------------------

#[tokio::test]
async fn a_non_exempt_ticket_gets_exactly_one_stored_suggestion() {
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    let policy = call(&w.app, "GET", "/api/ai/policy", None, Some(&w.requester)).await;
    assert_eq!(
        policy.body,
        json!({ "ai_assistance": true, "requester_skip_allowed": false })
    );

    let ticket = w.submit("ai_processing").await;
    assert_eq!(w.events(ticket, "submit").await, 1);
    assert_eq!(
        w.attempt(ticket).await,
        Some(("processing".into(), None, false))
    );
    let ai = w.ai(ticket).await;
    assert_eq!(ai["state"], "processing");
    assert_eq!(ai["language"], "en");
    assert_eq!(ai["suggestion"], Value::Null);
    assert_eq!(ai["reason"], Value::Null);
    // Not operator work yet: invisible in detail and the queue.
    assert_eq!(w.detail(ticket, &w.operator).await.status, 404);
    assert!(!w.in_queue(ticket).await);
    assert_eq!(
        w.mock.count(),
        0,
        "submission itself never calls the provider"
    );

    let worker = w.worker();
    assert!(ai_attempts::process_one(&worker).await.unwrap());

    // Completion stores the suggestion and waits for the requester. It neither
    // queues nor resolves the ticket.
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.events(ticket, "ai_suggestion_ready").await, 1);
    assert_eq!(w.queued(ticket).await, 0);
    let ai = w.ai(ticket).await;
    assert_eq!(ai["state"], "suggestion_ready");
    assert_eq!(ai["language"], "en");
    assert_eq!(
        ai["suggestion"],
        json!({ "explanation": "The profile is damaged.",
                "steps": ["Close Word", "Start Word again"] })
    );
    assert!(ai["finished_at"].is_string());
    assert_eq!(w.detail(ticket, &w.operator).await.status, 404);
    assert!(!w.in_queue(ticket).await);
    // Staff views never carry the attempt, not even for Admins.
    let admin = w.detail(ticket, &w.admin).await;
    assert_eq!(admin.status, 200);
    assert_eq!(admin.body["ai"], Value::Null);

    // The request held the frozen intake and nothing about the requester.
    let seen = w.mock.only();
    let sent = seen.body.to_string();
    for expected in ["Software", "Word crashes", "It closes on startup."] {
        assert!(sent.contains(expected), "{sent}");
    }
    for absent in [
        "pat.jones",
        "shared@example.com",
        &w.requester_id.to_string(),
    ] {
        assert!(!sent.contains(absent), "{sent}");
    }

    // One attempt, one solution: nothing left to do, no second provider call.
    assert!(!ai_attempts::process_one(&worker).await.unwrap());
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
    let (attempts, linked): (i64, i64) =
        sqlx::query_as("SELECT count(*), count(usage_id) FROM ai_attempts")
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!((attempts, linked), (1, 1));
    // The schema allows no second attempt for the ticket.
    let second = sqlx::query(
        "INSERT INTO ai_attempts (ticket_id, language, input, provider, model,
                                  credential_set_at, deadline_at)
         SELECT ticket_id, language, input, provider, model, credential_set_at, deadline_at
         FROM ai_attempts",
    )
    .execute(w.pool())
    .await;
    assert!(second.is_err());

    // A replayed submission returns the same ticket and creates no attempt.
    let key = Uuid::new_v4();
    let body = json!({ "idempotency_key": key, "intake": w.intake() });
    let first = call(
        &w.app,
        "POST",
        "/api/tickets",
        Some(body.clone()),
        Some(&w.requester),
    )
    .await;
    let replay = call(
        &w.app,
        "POST",
        "/api/tickets",
        Some(body),
        Some(&w.requester),
    )
    .await;
    assert_eq!((first.status.as_u16(), replay.status.as_u16()), (201, 200));
    assert_eq!(first.body["id"], replay.body["id"]);
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_attempts")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn the_snapshot_and_its_language_are_frozen_at_submission() {
    let Some(w) = world(vec![success_with(
        "Das Profil ist beschädigt.",
        &["Word neu starten"],
    )])
    .await
    else {
        return;
    };
    w.sql("UPDATE accounts SET language = 'de' WHERE username = 'pat.jones'")
        .await;
    // "Program" is visible for Software; "Printer" belongs to another category.
    let other: Uuid =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Hardware') RETURNING id")
            .fetch_one(w.pool())
            .await
            .unwrap();
    let mut fields = Vec::new();
    for (label, category) in [("Program name", w.category), ("Printer model", other)] {
        let field: Uuid = sqlx::query_scalar(
            "INSERT INTO intake_fields (label, kind) VALUES ($1, 'text') RETURNING id",
        )
        .bind(label)
        .fetch_one(w.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO intake_field_categories (field_id, category_id) VALUES ($1, $2)")
            .bind(field)
            .bind(category)
            .execute(w.pool())
            .await
            .unwrap();
        fields.push(field);
    }
    let shot = upload(
        &w.app,
        &w.requester,
        png(&image::RgbImage::from_pixel(
            40,
            30,
            image::Rgb([200, 30, 30]),
        )),
    )
    .await;
    let mut intake = w.intake();
    intake["description"] = json!("It closes on startup. My password: Tr0ub4dor&3");
    intake["answers"] = json!({
        fields[0].to_string(): { "text": "Word 2024" },
        fields[1].to_string(): { "text": "LaserJet 9000" },
    });
    intake["screenshot_ids"] = json!([shot]);
    let r = w.submit_with(intake, false).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let ticket: Uuid = serde_json::from_value(r.body["id"].clone()).unwrap();

    let (language, input): (String, Value) =
        sqlx::query_as("SELECT language, input FROM ai_attempts WHERE ticket_id = $1")
            .bind(ticket)
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!(language, "de");
    assert_eq!(input["category"], "Software");
    assert_eq!(
        input["answers"],
        json!([{ "label": "Program name", "value": "Word 2024" }])
    );
    assert_eq!(input["screenshots"][0]["id"], json!(shot));
    assert_eq!(
        input["screenshots"][0]["sha256"].as_str().unwrap().len(),
        64
    );
    assert_eq!(input["redactions"], 1);
    let stored = input.to_string();
    for absent in ["Tr0ub4dor", "LaserJet", "pat.jones", "shared@example.com"] {
        assert!(!stored.contains(absent), "{stored}");
    }

    // Everything that could change afterwards changes: the interface language,
    // the category name, the field label. The attempt does not follow.
    w.sql("UPDATE accounts SET language = 'en' WHERE username = 'pat.jones'")
        .await;
    w.sql("UPDATE categories SET name = 'Renamed' WHERE name = 'Software'")
        .await;
    w.sql("UPDATE intake_fields SET label = 'Renamed field'")
        .await;
    // Nothing can rewrite the snapshot or remove the attempt.
    for statement in [
        "UPDATE ai_attempts SET input = '{}'",
        "UPDATE ai_attempts SET language = 'en'",
        "UPDATE ai_attempts SET model = 'other'",
        "DELETE FROM ai_attempts",
    ] {
        assert!(
            sqlx::query(statement).execute(w.pool()).await.is_err(),
            "{statement}"
        );
    }

    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    let seen = w.mock.only();
    let sent = seen.body.to_string();
    assert!(sent.contains("German"), "{sent}");
    for expected in ["Software", "Program name", "Word 2024", "[redacted]"] {
        assert!(sent.contains(expected), "{sent}");
    }
    for absent in [
        "Renamed",
        "LaserJet",
        "Printer model",
        "Tr0ub4dor",
        "pat.jones",
    ] {
        assert!(!sent.contains(absent), "{sent}");
    }
    assert_eq!(sent_images(&seen.body).len(), 1);

    // The stored solution keeps its language whatever the interface shows now.
    let ai = w.ai(ticket).await;
    assert_eq!(ai["language"], "de");
    assert_eq!(
        ai["suggestion"]["explanation"],
        "Das Profil ist beschädigt."
    );
    for statement in [
        "UPDATE ai_attempts SET explanation = 'other'",
        "UPDATE ai_attempts SET state = 'processing', explanation = NULL, steps = NULL,
                finished_at = NULL",
    ] {
        assert!(
            sqlx::query(statement).execute(w.pool()).await.is_err(),
            "{statement}"
        );
    }
    // Reading in another language triggers nothing.
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.mock.count(), 0, "`only` consumed the single request");
}

#[tokio::test]
async fn a_requester_skip_needs_the_admin_toggle() {
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    // Toggle off: a skip at submission is refused and nothing is created.
    let refused = w.submit_with(w.intake(), true).await;
    assert_eq!(refused.status, 403, "{}", refused.body);
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(tickets, 0);

    // Toggle off: a skip while AI is processing cannot stop mandatory AI.
    let ticket = w.submit("ai_processing").await;
    assert!(matches!(skip(&w, ticket).await, Err(ApiError::Forbidden)));
    assert_eq!(w.status(ticket).await, "ai_processing");
    assert_eq!(
        w.attempt(ticket).await,
        Some(("processing".into(), None, false))
    );

    w.sql("UPDATE organization SET allow_requester_ai_skip = true")
        .await;
    let policy = call(&w.app, "GET", "/api/ai/policy", None, Some(&w.requester)).await;
    assert_eq!(policy.body["requester_skip_allowed"], true);

    // Toggle on: the skip ends the attempt and queues the ticket once.
    skip(&w, ticket).await.unwrap();
    assert_eq!(w.status(ticket).await, "queued");
    assert_eq!(
        w.attempt(ticket).await,
        Some(("handed_off".into(), Some("requester_skip".into()), false))
    );
    assert_eq!(w.queued(ticket).await, 1);
    assert_eq!(w.ai(ticket).await["reason"], "requester_skip");
    assert!(w.in_queue(ticket).await);
    // A second skip, and the worker, find nothing left to do.
    assert!(matches!(skip(&w, ticket).await, Err(ApiError::Conflict)));
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.queued(ticket).await, 1);

    // Toggle on: a skip at submission goes straight to operators, no attempt.
    let skipped = w.submit_with(w.intake(), true).await;
    assert_eq!(skipped.status, 201, "{}", skipped.body);
    assert_eq!(skipped.body["status"], "queued");
    let skipped: Uuid = serde_json::from_value(skipped.body["id"].clone()).unwrap();
    assert_eq!(w.attempt(skipped).await, None);
    assert_eq!(w.ai(skipped).await, Value::Null);
    let route: String = sqlx::query_scalar(
        "SELECT payload->>'route' FROM ticket_events WHERE ticket_id = $1 AND event_type = 'submit'",
    )
    .bind(skipped)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(route, "requester_skip");
    assert_eq!(w.mock.count(), 0);
}

#[tokio::test]
async fn an_exempt_category_makes_no_provider_call_even_mid_flight() {
    let Some(w) = world(vec![success().after(Duration::from_millis(800))]).await else {
        return;
    };
    // A ticket already being processed when the Admin exempts its category.
    let ticket = w.submit("ai_processing").await;
    let worker = w.worker_with(Duration::from_secs(10), Duration::from_secs(60));
    let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(1).await;
    let path = format!("/api/categories/{}", w.category);
    let body = json!({ "skip_ai": true });
    let r = call(&w.app, "PATCH", &path, Some(body), Some(&w.admin)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    // Handed off in the Admin's transaction; the late result is not accepted.
    w.assert_handed_off(ticket, "category_exempt").await;
    assert!(running.await.unwrap().unwrap());
    w.assert_handed_off(ticket, "category_exempt").await;
    assert_eq!(w.ai(ticket).await["reason"], "ai_unavailable");

    // New tickets of the exempt category never reach the provider.
    let calls = w.mock.count();
    let exempt = w.submit("queued").await;
    assert_eq!(w.attempt(exempt).await, None);
    let route: String = sqlx::query_scalar(
        "SELECT payload->>'route' FROM ticket_events WHERE ticket_id = $1 AND event_type = 'submit'",
    )
    .bind(exempt)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(route, "category_exempt");
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.mock.count(), calls);
}

// --- Failures hand off exactly once -------------------------------------------------

#[tokio::test]
async fn every_failure_hands_the_ticket_to_operators_exactly_once() {
    // (provider replies, setup, expected outcome, provider calls)
    let cases: Vec<(Reply, &str, &str, usize)> = vec![
        (failure(500), "", "upstream_unavailable", 1),
        (failure(429), "", "rate_limited", 1),
        (failure(401), "", "invalid_credentials", 1),
        (
            Reply::json(200, json!({ "status": "completed", "output": [] })),
            "",
            "malformed_output",
            1,
        ),
        (
            success(),
            "UPDATE ai_limits SET org_request_limit = 0",
            "budget_org_requests",
            0,
        ),
        (
            success(),
            "UPDATE ai_limits SET user_request_limit = 0",
            "budget_user_requests",
            0,
        ),
        (
            success(),
            "INSERT INTO ai_circuit (provider, consecutive_failures, open_until, opened_at)
             VALUES ('openai', 5, now() + interval '1 hour', now())",
            "circuit_open",
            0,
        ),
        (success().after(Duration::from_secs(3)), "", "timeout", 1),
    ];
    for (reply, setup, outcome, calls) in cases {
        let Some(w) = world(vec![reply]).await else {
            return;
        };
        let ticket = w.submit("ai_processing").await;
        if !setup.is_empty() {
            w.sql(setup).await;
        }
        let worker = w.worker_with(Duration::from_millis(500), Duration::from_millis(100));
        assert!(
            ai_attempts::process_one(&worker).await.unwrap(),
            "{outcome}"
        );
        w.assert_handed_off(ticket, outcome).await;
        assert_eq!(w.mock.count(), calls, "{outcome}");
        let ai = w.ai(ticket).await;
        assert_eq!(ai["state"], "handed_off", "{outcome}");
        // The requester view names no internal cause: a slow provider is
        // `timeout`, everything else `ai_failed`.
        let reason = if outcome == "timeout" {
            "timeout"
        } else {
            "ai_failed"
        };
        assert_eq!(ai["reason"], reason, "{outcome}");
        assert_eq!(ai["suggestion"], Value::Null);
        // No retry: the worker and the sweep are done with it.
        assert!(!ai_attempts::process_one(&worker).await.unwrap());
        assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
        assert_eq!(w.mock.count(), calls, "{outcome}");
        w.assert_handed_off(ticket, outcome).await;
        // Operators got it as ordinary queue work.
        assert_eq!(w.detail(ticket, &w.operator).await.status, 200);
    }
}

// --- Admin invalidation ---------------------------------------------------------------

#[tokio::test]
async fn admin_changes_stop_pending_attempts_and_apply_to_the_next_submission() {
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    let other_model = tikkit_api::ai::supported_models(tikkit_api::ai::Provider::OpenAi)
        .iter()
        .map(|m| m.id)
        .find(|id| *id != first_model())
        .expect("a second supported model");

    // Another model: attempts created for the old one are stopped.
    let ticket = w.submit("ai_processing").await;
    w.select(other_model, true).await;
    w.assert_handed_off(ticket, "configuration_changed").await;

    // A replaced key.
    let ticket = w.submit("ai_processing").await;
    w.set_key("test-key-not-real-0002").await;
    w.assert_handed_off(ticket, "credential_changed").await;

    // Disabled: pending attempts stop, and the next submission skips AI.
    let ticket = w.submit("ai_processing").await;
    w.select(other_model, false).await;
    w.assert_handed_off(ticket, "ai_disabled").await;
    assert_eq!(w.ai(ticket).await["reason"], "ai_unavailable");
    let direct = w.submit("queued").await;
    assert_eq!(w.attempt(direct).await, None);

    // A removed key (which also disables AI).
    w.select(other_model, true).await;
    let ticket = w.submit("ai_processing").await;
    let path = "/api/settings/ai/credentials/openai";
    let r = call(&w.app, "DELETE", path, None, Some(&w.admin)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    w.assert_handed_off(ticket, "credential_missing").await;
    let direct = w.submit("queued").await;
    assert_eq!(w.attempt(direct).await, None);

    // The handoffs are in the audit trail of the change that caused them.
    let audited: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum((metadata->>'attempts_handed_off')::int), 0)::bigint
         FROM audit_events WHERE event_type LIKE 'ai_%'",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(audited, 4);

    // None of it called the provider, and nothing is stranded.
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.mock.count(), 0);
    let stranded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets WHERE status IN ('ai_processing', 'awaiting_ai_decision')",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(stranded, 0);
}

#[tokio::test]
async fn disabling_ai_mid_flight_never_accepts_the_late_result() {
    let Some(w) = world(vec![success().after(Duration::from_millis(800))]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    // A long heartbeat: the call is not cancelled, so its result really arrives late.
    let worker = w.worker_with(Duration::from_secs(10), Duration::from_secs(60));
    let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(1).await;
    w.select(first_model(), false).await;
    // Operators have it before the provider answered.
    w.assert_handed_off(ticket, "ai_disabled").await;
    assert!(running.await.unwrap().unwrap());
    w.assert_handed_off(ticket, "ai_disabled").await;
    assert_eq!(w.mock.count(), 1);
}

#[tokio::test]
async fn removing_the_key_mid_flight_never_accepts_the_late_result() {
    let Some(w) = world(vec![success().after(Duration::from_millis(800))]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    let worker = w.worker_with(Duration::from_secs(10), Duration::from_secs(60));
    let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(1).await;
    let path = "/api/settings/ai/credentials/openai";
    let r = call(&w.app, "DELETE", path, None, Some(&w.admin)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["status"], "disabled", "{}", r.body);
    w.assert_handed_off(ticket, "credential_missing").await;
    // The provider answers with a suggestion made with the revoked key.
    assert!(running.await.unwrap().unwrap());
    w.assert_handed_off(ticket, "credential_missing").await;
    assert_eq!(w.ai(ticket).await["suggestion"], Value::Null);
    assert_eq!(w.mock.count(), 1);
}

#[tokio::test]
async fn a_running_call_is_cancelled_when_its_attempt_is_stopped() {
    let Some(w) = world(vec![success().after(Duration::from_secs(8))]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    let worker = w.worker();
    let started = std::time::Instant::now();
    let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(1).await;
    w.select(first_model(), false).await;
    assert!(running.await.unwrap().unwrap());
    // The heartbeat noticed and stopped the provider call long before it answered.
    assert!(started.elapsed() < Duration::from_secs(5));
    w.assert_handed_off(ticket, "ai_disabled").await;
}

#[tokio::test]
async fn policy_is_checked_again_when_the_result_is_committed() {
    // Changes that bypass the Admin handlers (another process, a restored
    // database, a rotated encryption key) are caught by the worker itself.
    for (change, outcome) in [
        (
            "UPDATE ai_credentials SET set_at = set_at + interval '1 second'",
            "credential_changed",
        ),
        ("UPDATE ai_settings SET enabled = false", "ai_disabled"),
        ("UPDATE categories SET skip_ai = true", "category_exempt"),
        (
            "UPDATE ai_credentials SET key_ciphertext = decode(repeat('00', 40), 'hex')",
            "credential_unreadable",
        ),
    ] {
        let Some(w) = world(vec![success().after(Duration::from_millis(600))]).await else {
            return;
        };
        let ticket = w.submit("ai_processing").await;
        let worker = w.worker_with(Duration::from_secs(10), Duration::from_secs(60));
        let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
        w.wait_for_calls(1).await;
        w.sql(change).await;
        assert!(running.await.unwrap().unwrap());
        // The provider answered with a suggestion; it is not accepted.
        w.assert_handed_off(ticket, outcome).await;

        // Before dispatch the same check makes zero provider calls.
        let Some(w) = world(vec![success()]).await else {
            return;
        };
        let ticket = w.submit("ai_processing").await;
        w.sql(change).await;
        assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
        w.assert_handed_off(ticket, outcome).await;
        assert_eq!(w.mock.count(), 0, "{outcome}");
    }
}

// --- Requester cancellation ---------------------------------------------------------------

#[tokio::test]
async fn a_cancelled_request_is_terminated_and_late_output_is_ignored() {
    for reply in [success(), failure(500)] {
        let Some(w) = world(vec![reply.after(Duration::from_millis(800))]).await else {
            return;
        };
        let ticket = w.submit("ai_processing").await;
        let worker = w.worker_with(Duration::from_secs(10), Duration::from_secs(60));
        let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
        w.wait_for_calls(1).await;

        let path = format!("/api/tickets/{ticket}/actions");
        // The reason is mandatory; mandatory AI does not forbid cancelling.
        let body = json!({ "action": "cancel", "reason": "  " });
        let r = call(&w.app, "POST", &path, Some(body), Some(&w.requester)).await;
        assert_eq!(r.status, 422, "{}", r.body);
        let body = json!({ "action": "cancel", "reason": "Solved it myself" });
        let r = call(&w.app, "POST", &path, Some(body), Some(&w.requester)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["status"], "cancelled");

        // The provider answers (or fails) afterwards: nothing happens.
        assert!(running.await.unwrap().unwrap());
        assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
        assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
        assert_eq!(w.status(ticket).await, "cancelled");
        assert_eq!(
            w.attempt(ticket).await,
            Some((
                "cancelled".into(),
                Some("requester_cancelled".into()),
                false
            ))
        );
        assert_eq!(w.queued(ticket).await, 0);
        assert_eq!(w.events(ticket, "ai_failed").await, 0);
        assert_eq!(w.events(ticket, "ai_suggestion_ready").await, 0);
        assert_eq!(w.detail(ticket, &w.operator).await.status, 404);
        assert!(!w.in_queue(ticket).await);
        let ai = w.ai(ticket).await;
        assert_eq!(ai["state"], "cancelled");
        assert_eq!(ai["suggestion"], Value::Null);
        // Abandoned, auditable, and not a solved outcome.
        let solved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tickets WHERE status = 'ai_solved' OR resolution_source = 'ai'",
        )
        .fetch_one(w.pool())
        .await
        .unwrap();
        assert_eq!(solved, 0);
    }
}

// --- Restart recovery, leases, deadlines --------------------------------------------------

#[tokio::test]
async fn concurrent_workers_claim_an_attempt_once() {
    let Some(w) = world(vec![success().after(Duration::from_millis(300))]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    // Several processes: each worker has its own pool.
    let mut workers = Vec::new();
    for _ in 0..4 {
        let pool = TestDb::pool_for(&w.db).await;
        let client = AiClient::with_options(CallOptions {
            max_retries: 0,
            ..CallOptions::default()
        });
        let endpoint = Endpoint::Override {
            base_url: w.mock.url.clone(),
            allow_private_network: true,
        };
        let guard = AiGuard::new(pool.clone(), client, endpoint);
        workers.push(Worker::with(&test_state(&pool), guard, KEY));
    }
    let results = futures(workers).await;
    assert_eq!(results.iter().filter(|claimed| **claimed).count(), 1);
    assert_eq!(w.mock.count(), 1);
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.events(ticket, "ai_suggestion_ready").await, 1);
}

/// The requester skips AI on a ticket that is being processed.
async fn skip(w: &World, ticket: Uuid) -> Result<tikkit_api::api::TicketStatus, ApiError> {
    let mut tx = w.pool().begin().await.unwrap();
    let by = By::Account(w.requester_id);
    let result = lifecycle::transition(&mut tx, ticket, by, &Transition::SkipAi).await;
    if result.is_ok() {
        tx.commit().await.unwrap();
    }
    result
}

async fn futures(workers: Vec<Worker>) -> Vec<bool> {
    let tasks: Vec<_> = workers
        .into_iter()
        .map(|worker| tokio::spawn(async move { ai_attempts::process_one(&worker).await }))
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap().unwrap());
    }
    results
}

#[tokio::test]
async fn an_attempt_survives_a_worker_that_died_mid_call() {
    let Some(w) = world(vec![
        success().after(Duration::from_secs(30)),
        success_with("Second answer.", &["Step"]),
    ])
    .await
    else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    // The first worker dies while the provider is still answering.
    let worker = w.worker();
    let dying = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(1).await;
    dying.abort();
    let _ = dying.await;
    assert_eq!(w.status(ticket).await, "ai_processing");
    assert_eq!(
        w.attempt(ticket).await,
        Some(("processing".into(), None, false))
    );

    // While its lease lasts nobody else touches the attempt ...
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    // ... and once it ran out, the next worker (after a restart) finishes it.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.events(ticket, "ai_suggestion_ready").await, 1);
    assert_eq!(
        w.ai(ticket).await["suggestion"]["explanation"],
        "Second answer."
    );
    let (attempts, claims): (i64, i32) =
        sqlx::query_as("SELECT count(*), max(claims) FROM ai_attempts")
            .fetch_one(w.pool())
            .await
            .unwrap();
    // Still one logical attempt (taken over once) and one stored solution.
    assert_eq!((attempts, claims), (1, 2));
    // The abandoned call's reservation does not block admission forever.
    assert_eq!(w.mock.count(), 2);
}

#[tokio::test]
async fn an_overdue_attempt_is_handed_off_even_without_a_worker() {
    let Some(w) = world(vec![success().after(Duration::from_secs(8))]).await else {
        return;
    };
    // No worker at all: the sweep ends it at the deadline.
    let ticket = w.submit("ai_processing").await;
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
    w.overdue(ticket).await;
    // An overdue attempt is not started any more.
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert!(ai_attempts::sweep_one(&w.worker()).await.unwrap());
    w.assert_handed_off(ticket, "timeout").await;
    assert_eq!(w.ai(ticket).await["reason"], "timeout");
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
    assert_eq!(w.mock.count(), 0);

    // A worker that died mid-call: once its lease ran out past the deadline,
    // the sweep hands the ticket off without another provider call.
    let ticket = w.submit("ai_processing").await;
    let worker = w.worker();
    let dying = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(1).await;
    dying.abort();
    let _ = dying.await;
    w.overdue(ticket).await;
    assert!(
        !ai_attempts::sweep_one(&w.worker()).await.unwrap(),
        "a live claim may still finish its call"
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert!(ai_attempts::sweep_one(&w.worker()).await.unwrap());
    w.assert_handed_off(ticket, "timeout").await;
    assert_eq!(w.mock.count(), 1);
}

#[tokio::test]
async fn a_call_started_before_the_deadline_is_not_cut_short() {
    let Some(w) = world(vec![success().after(Duration::from_millis(700))]).await else {
        return;
    };
    // The deadline passes while the provider is answering: the call was
    // admitted in time, so its suggestion is accepted.
    let ticket = w.submit("ai_processing").await;
    let worker = w.worker();
    let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(1).await;
    w.overdue(ticket).await;
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
    assert!(running.await.unwrap().unwrap());
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.events(ticket, "ai_suggestion_ready").await, 1);
    assert_eq!(w.queued(ticket).await, 0);

    // The hard bound: so long past the deadline that no call started in time
    // can still be running. The sweep ends it whatever the worker does, and
    // the result that arrives afterwards is dropped.
    let ticket = w.submit("ai_processing").await;
    let worker = w.worker();
    let running = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
    w.wait_for_calls(2).await;
    w.deadline_in(ticket, -3600.0).await;
    assert!(ai_attempts::sweep_one(&w.worker()).await.unwrap());
    w.assert_handed_off(ticket, "timeout").await;
    assert!(running.await.unwrap().unwrap());
    w.assert_handed_off(ticket, "timeout").await;
}

#[tokio::test]
async fn a_locked_ticket_does_not_block_the_sweep() {
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    let first = w.submit("ai_processing").await;
    let second = w.submit("ai_processing").await;
    w.overdue(first).await;
    w.overdue(second).await;
    // Something holds the older ticket's row.
    let mut holder = w.pool().begin().await.unwrap();
    sqlx::query("SELECT 1 FROM tickets WHERE id = $1 FOR UPDATE")
        .bind(first)
        .execute(&mut *holder)
        .await
        .unwrap();
    assert!(ai_attempts::sweep_one(&w.worker()).await.unwrap());
    w.assert_handed_off(second, "timeout").await;
    assert_eq!(w.status(first).await, "ai_processing");
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
    holder.rollback().await.unwrap();
    assert!(ai_attempts::sweep_one(&w.worker()).await.unwrap());
    w.assert_handed_off(first, "timeout").await;
}

#[tokio::test]
async fn a_ticket_in_ai_processing_without_an_attempt_is_not_stranded() {
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    // Only reachable around the submission path (here: the fixed-policy test
    // entry point of the lifecycle). The sweep still gets it to operators.
    let ticket: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, subject, description)
         VALUES ($1, $2, $3, 'Orphan', 'No attempt') RETURNING id",
    )
    .bind(w.requester_id)
    .bind(w.category)
    .bind(w.priority)
    .fetch_one(w.pool())
    .await
    .unwrap();
    let mut tx = w.pool().begin().await.unwrap();
    let policy = Policy {
        ai_available: true,
        ..Policy::default()
    };
    let submit = Transition::Submit {
        requester_skip: false,
    };
    let by = By::Account(w.requester_id);
    lifecycle::transition_with(&mut tx, ticket, by, &submit, Some(policy))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(w.status(ticket).await, "ai_processing");
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert!(ai_attempts::sweep_one(&w.worker()).await.unwrap());
    assert_eq!(w.status(ticket).await, "queued");
    assert_eq!(w.queued(ticket).await, 1);
    // There is no attempt row to carry the outcome, so it is in the audit log.
    let audited: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'ai_attempt_orphaned'",
    )
    .fetch_all(w.pool())
    .await
    .unwrap();
    assert_eq!(
        audited,
        [json!({ "ticket_id": ticket, "outcome": "orphaned" })]
    );
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
}

#[tokio::test]
async fn a_result_that_cannot_be_stored_never_causes_another_provider_call() {
    // Output the database cannot hold is not a suggestion in the first place.
    let Some(w) = world(vec![success_with("bad\u{0}text", &["Step"])]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    w.assert_handed_off(ticket, "malformed_output").await;
    assert_eq!(w.mock.count(), 1);

    // The transaction that stores a good suggestion keeps failing.
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    w.sql(
        "CREATE FUNCTION fail_commit() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'disk full'; END $$",
    )
    .await;
    w.sql(
        "CREATE TRIGGER fail_commit BEFORE UPDATE ON ai_attempts FOR EACH ROW
         WHEN (NEW.state = 'suggestion_ready') EXECUTE FUNCTION fail_commit()",
    )
    .await;
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    // All or nothing: no half-stored suggestion. The ticket went to operators
    // instead of waiting for the lease and a second paid call.
    w.assert_handed_off(ticket, "commit_failed").await;
    assert_eq!(w.ai(ticket).await["reason"], "ai_failed");
    let linked: i64 = sqlx::query_scalar("SELECT count(usage_id) FROM ai_attempts")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(linked, 1, "the paid call stays linked to the attempt");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
    assert_eq!(w.mock.count(), 1);
}

#[tokio::test]
async fn a_crash_loop_ends_after_a_fixed_number_of_claims() {
    let Some(w) = world(vec![success().after(Duration::from_secs(30))]).await else {
        return;
    };
    // The dead workers' reservations must not be what stops the next call.
    w.sql("UPDATE ai_limits SET max_concurrency = 16").await;
    let ticket = w.submit("ai_processing").await;
    for claim in 1..=ai_attempts::MAX_CLAIMS {
        let worker = w.worker();
        let dying = tokio::spawn(async move { ai_attempts::process_one(&worker).await });
        w.wait_for_calls(claim as usize).await;
        dying.abort();
        let _ = dying.await;
        assert_eq!(w.claims(ticket).await.0, claim);
        // Not before the last lease ran out.
        assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    // No further claim and no further provider call: the next step hands off.
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.mock.count(), ai_attempts::MAX_CLAIMS as usize);
    assert!(ai_attempts::sweep_one(&w.worker()).await.unwrap());
    w.assert_handed_off(ticket, "claims_exhausted").await;
    assert_eq!(w.ai(ticket).await["reason"], "ai_failed");
    assert_eq!(w.mock.count(), ai_attempts::MAX_CLAIMS as usize);
}

#[tokio::test]
async fn a_takeover_drops_the_first_workers_late_result() {
    let Some(w) = world(vec![
        success_with("First answer.", &["Step"]).after(Duration::from_millis(1500)),
        success_with("Second answer.", &["Step"]),
    ])
    .await
    else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    // The first worker stalls: it is alive but no longer renews its lease.
    let stalled = w.worker_with(Duration::from_secs(10), Duration::from_secs(60));
    let first = tokio::spawn(async move { ai_attempts::process_one(&stalled).await });
    w.wait_for_calls(1).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    // A second worker takes the attempt over and finishes it.
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.claims(ticket).await.0, 2);
    // The first worker's provider answers afterwards: its result is dropped.
    assert!(first.await.unwrap().unwrap());
    assert_eq!(w.mock.count(), 2);
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.events(ticket, "ai_suggestion_ready").await, 1);
    assert_eq!(w.queued(ticket).await, 0);
    assert_eq!(
        w.ai(ticket).await["suggestion"]["explanation"],
        "Second answer."
    );
}

#[tokio::test]
async fn shutdown_cancels_the_call_and_leaves_the_attempt_for_the_next_start() {
    let Some(w) = world(vec![success().after(Duration::from_secs(20)), success()]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    let worker = w.worker();
    let running = {
        let worker = worker.clone();
        tokio::spawn(async move { ai_attempts::process_one(&worker).await })
    };
    w.wait_for_calls(1).await;
    let stopping = std::time::Instant::now();
    worker.stop();
    assert!(running.await.unwrap().unwrap());
    assert!(stopping.elapsed() < Duration::from_secs(3));

    // Not handed off, not cancelled: still processing, and unclaimed.
    assert_eq!(w.status(ticket).await, "ai_processing");
    assert_eq!(
        w.attempt(ticket).await,
        Some(("processing".into(), None, false))
    );
    assert_eq!(w.claims(ticket).await, (1, false));
    assert_eq!(w.queued(ticket).await, 0);
    assert_eq!(w.events(ticket, "ai_failed").await, 0);
    // The cancelled call gave its provider slot back at once.
    let usage: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, outcome FROM ai_usage")
            .fetch_all(w.pool())
            .await
            .unwrap();
    assert_eq!(usage, [("failed".into(), Some("cancelled".into()))]);
    // A stopped worker starts nothing new.
    assert!(ai_attempts::process_one(&worker).await.unwrap());
    assert_eq!(w.status(ticket).await, "ai_processing");
    assert_eq!(w.mock.count(), 1);

    // The next process takes it over without waiting for a lease.
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.events(ticket, "ai_suggestion_ready").await, 1);
    assert_eq!(w.mock.count(), 2);
}

/// The background loop itself, as `main.rs` runs and stops it. The only test
/// that calls `stop_workers` (a process-wide flag): nothing else in this binary
/// uses the polling loops.
#[tokio::test]
async fn the_worker_loop_stops_within_the_shutdown_window() {
    let Some(w) = world(vec![success().after(Duration::from_secs(20)), success()]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    let looping = tokio::spawn(ai_attempts::worker_loop(w.worker()));
    w.wait_for_calls(1).await;
    let stopping = std::time::Instant::now();
    tikkit_api::notifications::stop_workers();
    // `main.rs` waits five seconds for its workers.
    tokio::time::timeout(Duration::from_secs(5), looping)
        .await
        .expect("the worker loop ends in time")
        .unwrap();
    assert!(stopping.elapsed() < Duration::from_secs(5));
    assert_eq!(w.status(ticket).await, "ai_processing");
    assert_eq!(w.claims(ticket).await, (1, false));
    let reserved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ai_usage WHERE status = 'reserved'")
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!(reserved, 0, "no provider slot stays taken after shutdown");
    // After the restart the attempt is finished as usual.
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
}

// --- Provider concurrency -------------------------------------------------------------------

#[tokio::test]
async fn attempts_beyond_the_provider_slots_wait_their_turn() {
    let Some(w) = world(vec![success().after(Duration::from_millis(400))]).await else {
        return;
    };
    // Two provider slots (the default), three tickets, three workers at once.
    let tickets = [
        w.submit("ai_processing").await,
        w.submit("ai_processing").await,
        w.submit("ai_processing").await,
    ];
    let claimed = futures(vec![w.worker(), w.worker(), w.worker()]).await;
    assert_eq!(claimed.iter().filter(|c| **c).count(), 2);
    assert_eq!(w.mock.max_in_flight(), 2);
    // The oldest two are done; the third was left unclaimed, not refused.
    assert_eq!(w.status(tickets[0]).await, "awaiting_ai_decision");
    assert_eq!(w.status(tickets[1]).await, "awaiting_ai_decision");
    assert_eq!(w.status(tickets[2]).await, "ai_processing");
    assert_eq!(w.claims(tickets[2]).await, (0, false));
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.status(tickets[2]).await, "awaiting_ai_decision");
    let refused: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_usage WHERE status = 'refused'")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(refused, 0);
}

#[tokio::test]
async fn a_saturated_attempt_retries_and_hands_off_only_at_its_deadline() {
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    // The guard refuses at once when every slot is taken.
    w.sql("UPDATE ai_limits SET concurrency_wait_seconds = 0")
        .await;

    // Slots held elsewhere for a moment: the attempt asks again and gets its
    // suggestion under the same claim.
    let ticket = w.submit("ai_processing").await;
    w.occupy_slots(3600.0).await;
    let refusals = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM ai_usage WHERE outcome = 'concurrency_saturated'",
        )
        .fetch_one(w.pool())
        .await
        .unwrap()
    };
    let worker = w.worker();
    // The slots are freed only after the guard refused once, so the retry is
    // observed however slow the machine is.
    let (processed, ()) = tokio::join!(ai_attempts::process_one(&worker), async {
        for _ in 0..3000 {
            if refusals().await >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        w.sql("UPDATE ai_usage SET lease_expires_at = clock_timestamp() WHERE status = 'reserved'")
            .await;
    });
    assert!(processed.unwrap());
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");
    assert_eq!(w.claims(ticket).await.0, 1);
    assert_eq!(w.mock.count(), 1);
    assert!(refusals().await >= 1);

    // Slots held past the attempt's deadline: only then it hands off as saturated.
    let ticket = w.submit("ai_processing").await;
    w.occupy_slots(3600.0).await;
    w.deadline_in(ticket, 2.0).await;
    let started = std::time::Instant::now();
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    assert!(started.elapsed() >= Duration::from_millis(1500));
    w.assert_handed_off(ticket, "concurrency_saturated").await;
    assert_eq!(w.ai(ticket).await["reason"], "ai_failed");
    assert_eq!(w.mock.count(), 1, "no provider call for the second ticket");
}

// --- Screenshots ----------------------------------------------------------------------------

#[tokio::test]
async fn screenshots_are_prepared_to_fit_and_never_dropped() {
    let Some(w) = world(vec![success()]).await else {
        return;
    };
    let small = png(&image::RgbImage::from_pixel(
        64,
        48,
        image::Rgb([240, 240, 240]),
    ));
    let big = png(&noise(1500, 1500));
    assert!(big.len() > MAX_IMAGE_BYTES, "{}", big.len());
    let small_id = upload(&w.app, &w.requester, small.clone()).await;
    let big_id = upload(&w.app, &w.requester, big).await;
    let mut intake = w.intake();
    intake["screenshot_ids"] = json!([small_id, big_id]);
    let r = w.submit_with(intake.clone(), false).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let ticket: Uuid = serde_json::from_value(r.body["id"].clone()).unwrap();
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    assert_eq!(w.status(ticket).await, "awaiting_ai_decision");

    let images = sent_images(&w.mock.only().body);
    assert_eq!(images.len(), 2, "both screenshots are sent");
    // Within the limits: sent as stored.
    assert_eq!(images[0].0, "image/png");
    assert_eq!(images[0].1.len(), {
        let stored = std::fs::read(support::screenshot_dir(w.pool()).join(small_id.to_string()));
        stored.unwrap().len()
    });
    // Over the limits: re-encoded to fit, still a real, readable image.
    assert_eq!(images[1].0, "image/jpeg");
    assert!(images[1].1.len() <= MAX_IMAGE_BYTES);
    let decoded = image::load_from_memory(&images[1].1).unwrap();
    assert!(decoded.width() >= 700, "{}", decoded.width());
    let usage_images: i32 = sqlx::query_scalar("SELECT image_count FROM ai_usage")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(usage_images, 2);

    // A screenshot whose stored bytes are gone or differ from the snapshot is
    // never silently left out: the attempt hands off without a provider call.
    for damage in ["remove", "replace"] {
        let shot = upload(&w.app, &w.requester, small.clone()).await;
        intake["screenshot_ids"] = json!([shot]);
        let r = w.submit_with(intake.clone(), false).await;
        assert_eq!(r.status, 201, "{}", r.body);
        let ticket: Uuid = serde_json::from_value(r.body["id"].clone()).unwrap();
        let file = support::screenshot_dir(w.pool()).join(shot.to_string());
        if damage == "remove" {
            std::fs::remove_file(&file).unwrap();
        } else {
            std::fs::write(&file, png(&noise(8, 8))).unwrap();
        }
        assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
        w.assert_handed_off(ticket, "image_unavailable").await;
    }
    assert_eq!(w.mock.count(), 0, "`only` consumed the single request");
}

#[tokio::test]
async fn later_replies_and_screenshots_never_reach_the_provider() {
    let Some(w) = world(vec![failure(500)]).await else {
        return;
    };
    let ticket = w.submit("ai_processing").await;
    assert!(ai_attempts::process_one(&w.worker()).await.unwrap());
    w.assert_handed_off(ticket, "upstream_unavailable").await;
    let before: Value = sqlx::query_scalar("SELECT input FROM ai_attempts WHERE ticket_id = $1")
        .bind(ticket)
        .fetch_one(w.pool())
        .await
        .unwrap();

    // The requester adds a reply with a new screenshot for the operators.
    let shot = upload(
        &w.app,
        &w.requester,
        png(&image::RgbImage::from_pixel(20, 20, image::Rgb([1, 2, 3]))),
    )
    .await;
    let path = format!("/api/tickets/{ticket}/replies");
    let body = json!({ "idempotency_key": Uuid::new_v4(), "body": "One more detail",
        "screenshot_ids": [shot] });
    let r = call(&w.app, "POST", &path, Some(body), Some(&w.requester)).await;
    assert!(r.status.is_success(), "{} {}", r.status, r.body);

    // No regeneration, no second call, the snapshot is what it was.
    assert!(!ai_attempts::process_one(&w.worker()).await.unwrap());
    assert!(!ai_attempts::sweep_one(&w.worker()).await.unwrap());
    assert_eq!(w.mock.count(), 1);
    let after: Value = sqlx::query_scalar("SELECT input FROM ai_attempts WHERE ticket_id = $1")
        .bind(ticket)
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
    w.assert_handed_off(ticket, "upstream_unavailable").await;
}
