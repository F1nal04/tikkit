//! AI usage safeguards against real PostgreSQL and a local mock provider
//! (docs/ai-usage.md): budgets under races, bursts against the concurrency
//! limit, the circuit breaker, failure outcomes, retention, the Admin API, and
//! what reaches storage and logs. No test talks to a real provider.

mod support;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login,
    provider_mock::{Mock, Reply, serve},
    test_app, test_app_with_provider,
};
use tikkit_api::{
    ai::{
        AiClient, AiErrorKind, ApiKey, CallOptions, Cancel, ConditionalAnswer, Endpoint,
        ImageInput, ImageMediaType, Language, Provider, SuggestionRequest, supported_models,
    },
    ai_config::ActiveAi,
    ai_guard::{self, AiGuard, BudgetScope, Refusal, estimate_tokens},
    ai_privacy::{REDACTED, minimize},
};
use uuid::Uuid;

/// Deliberately not shaped like a real provider key.
const KEY: &str = "test-key-not-real";
const PASSWORD_IN_TICKET: &str = "Tr0ub4dor&3";
const TOKEN_IN_TICKET: &str = "sk-proj-abcdefghijklmnop1234";

// --- Fixtures ------------------------------------------------------------------

fn active(provider: Provider) -> ActiveAi {
    ActiveAi {
        provider,
        model: supported_models(provider)[0].id.to_owned(),
        api_key: ApiKey::new(KEY),
    }
}

fn guard_with(pool: &PgPool, mock: &Mock, total_timeout: Duration) -> AiGuard {
    let client = AiClient::with_options(CallOptions {
        request_timeout: total_timeout,
        total_timeout,
        max_retries: 0,
        retry_backoff: Duration::from_millis(5),
    });
    let endpoint = Endpoint::Override {
        base_url: mock.url.clone(),
        allow_private_network: true,
    };
    AiGuard::new(pool.clone(), client, endpoint)
}

fn guard(pool: &PgPool, mock: &Mock) -> AiGuard {
    guard_with(pool, mock, Duration::from_secs(10))
}

fn request() -> SuggestionRequest {
    SuggestionRequest {
        language: Language::En,
        category: "Software".into(),
        subject: "Outlook does not start".into(),
        description: "Outlook shows an error on startup.".into(),
        answers: vec![ConditionalAnswer {
            label: "Program".into(),
            value: "Outlook".into(),
        }],
        images: vec![],
    }
}

fn png() -> ImageInput {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(8, 6, image::Rgb([200, 30, 30]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    ImageInput {
        media_type: ImageMediaType::Png,
        bytes: bytes.into_inner(),
    }
}

/// A successful provider response. Token metadata differs per provider:
/// Anthropic reports no total.
fn success(provider: Provider, usage: bool) -> Reply {
    let text = json!({ "explanation": "The profile is damaged.", "steps": ["Restart Outlook"] })
        .to_string();
    let mut body = match provider {
        Provider::OpenAi => json!({
            "id": "resp_1", "object": "response", "status": "completed",
            "output": [
                { "type": "message", "id": "msg_1", "status": "completed", "role": "assistant",
                  "content": [{ "type": "output_text", "text": text, "annotations": [] }] },
            ],
        }),
        Provider::Gemini => json!({
            "id": "v1_abc", "object": "interaction", "status": "completed",
            "steps": [{ "type": "model_output", "content": [{ "type": "text", "text": text }] }],
        }),
        Provider::Anthropic => json!({
            "id": "msg_1", "type": "message", "role": "assistant", "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": text }],
        }),
    };
    if usage {
        body["usage"] = match provider {
            Provider::OpenAi => {
                json!({ "input_tokens": 120, "output_tokens": 45, "total_tokens": 165 })
            }
            Provider::Gemini => json!({
                "total_input_tokens": 120, "total_output_tokens": 45, "total_tokens": 175,
            }),
            Provider::Anthropic => json!({ "input_tokens": 120, "output_tokens": 45 }),
        };
    }
    Reply::json(200, body)
}

/// A provider error whose free-text message echoes `message`.
fn failure(status: u16, message: &str) -> Reply {
    Reply::json(
        status,
        json!({ "error": { "message": message, "type": "api_error", "code": "server_error" } }),
    )
}

async fn limits(pool: &PgPool, set: &str) {
    sqlx::query(&format!("UPDATE ai_limits SET {set}"))
        .execute(pool)
        .await
        .unwrap();
}

async fn user(pool: &PgPool, name: &str) -> Uuid {
    fixtures::account(pool, Role::User, name).await
}

/// Every `ai_usage` row, oldest first, without the per-run columns.
async fn rows(pool: &PgPool) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT to_jsonb(u) - 'id' - 'account_id' - 'started_at' - 'finished_at'
                - 'lease_expires_at'
         FROM ai_usage u ORDER BY started_at, id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn count(pool: &PgPool, filter: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM ai_usage WHERE {filter}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// (open, consecutive failures) of the provider's circuit; `None`: closed, no row.
async fn circuit(pool: &PgPool, provider: Provider) -> Option<(bool, i32)> {
    sqlx::query_as(
        "SELECT open_until IS NOT NULL, consecutive_failures FROM ai_circuit WHERE provider = $1",
    )
    .bind(provider.as_str())
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Moves time past the breaker cooldown.
async fn end_cooldown(pool: &PgPool) {
    sqlx::query(
        "UPDATE ai_circuit SET open_until = clock_timestamp() - interval '1 second'
         WHERE open_until IS NOT NULL",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Runs `n` guarded calls at once, each for the account `accounts[i % len]`.
async fn burst(
    guard: &AiGuard,
    provider: Provider,
    accounts: &[Uuid],
    n: usize,
) -> Vec<Result<Uuid, Refusal>> {
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..n {
        let guard = guard.clone();
        let account = accounts[i % accounts.len()];
        tasks.spawn(async move {
            guard
                .suggest(account, &active(provider), &request(), &Cancel::new())
                .await
                .map(|done| done.usage_id)
                .map_err(|refused| refused.refusal)
        });
    }
    tasks.join_all().await
}

fn refused_with(results: &[Result<Uuid, Refusal>], refusal: Refusal) -> usize {
    results.iter().filter(|r| **r == Err(refusal)).count()
}

fn succeeded(results: &[Result<Uuid, Refusal>]) -> usize {
    results.iter().filter(|r| r.is_ok()).count()
}

// --- Usage records ---------------------------------------------------------------

#[tokio::test]
async fn usage_is_stored_exactly_as_each_provider_reports_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    for (provider, tokens) in [
        (Provider::OpenAi, json!([120, 45, 165, 165])),
        (Provider::Gemini, json!([120, 45, 175, 175])),
        // No total from Anthropic: none is stored, and none is invented. The
        // budget counts the two figures it did report.
        (Provider::Anthropic, json!([120, 45, null, 165])),
    ] {
        sqlx::query("DELETE FROM ai_usage")
            .execute(&db.pool)
            .await
            .unwrap();
        let mock = serve(vec![success(provider, true)]).await;
        let request = SuggestionRequest {
            images: vec![png()],
            ..request()
        };
        let done = guard(&db.pool, &mock)
            .suggest(account, &active(provider), &request, &Cancel::new())
            .await
            .unwrap();
        assert_eq!(done.suggestion.steps, ["Restart Outlook"]);
        assert_eq!(mock.count(), 1);
        assert_eq!(
            rows(&db.pool).await,
            [json!({
                "provider": provider.as_str(),
                "model": supported_models(provider)[0].id,
                "image_count": 1,
                "status": "succeeded",
                "outcome": null,
                "http_status": null,
                "provider_code": null,
                "input_tokens": tokens[0],
                "output_tokens": tokens[1],
                "total_tokens": tokens[2],
                "budget_tokens": tokens[3],
                "budget_basis": "reported",
                "breaker_probe": false,
            })],
            "{provider:?}"
        );
        let stored: Uuid = sqlx::query_scalar("SELECT id FROM ai_usage WHERE account_id = $1")
            .bind(account)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(stored, done.usage_id);
    }
}

#[tokio::test]
async fn unreported_usage_stays_unreported_and_the_budget_keeps_its_estimate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    for provider in Provider::ALL {
        let mock = serve(vec![success(provider, false)]).await;
        guard(&db.pool, &mock)
            .suggest(account, &active(provider), &request(), &Cancel::new())
            .await
            .unwrap();
    }
    let estimate = estimate_tokens(&request());
    for row in rows(&db.pool).await {
        assert_eq!(row["status"], "succeeded");
        assert_eq!(row["input_tokens"], Value::Null);
        assert_eq!(row["output_tokens"], Value::Null);
        assert_eq!(row["total_tokens"], Value::Null);
        assert_eq!(row["budget_basis"], "estimated");
        assert_eq!(row["budget_tokens"], json!(estimate));
    }
}

// --- Budgets -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_never_overshoots_the_per_user_request_budget() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    limits(
        &db.pool,
        "user_request_limit = 3, org_request_limit = NULL, max_concurrency = 64",
    )
    .await;
    let account = user(&db.pool, "requester").await;
    let other = user(&db.pool, "other").await;
    for provider in Provider::ALL {
        sqlx::query("DELETE FROM ai_usage")
            .execute(&db.pool)
            .await
            .unwrap();
        let mock = serve(vec![
            success(provider, true).after(Duration::from_millis(20)),
        ])
        .await;
        let guard = guard(&db.pool, &mock);
        let results = burst(&guard, provider, &[account], 20).await;
        assert_eq!(succeeded(&results), 3, "{provider:?}");
        assert_eq!(
            refused_with(
                &results,
                Refusal::BudgetExhausted(BudgetScope::UserRequests)
            ),
            17
        );
        // Refusals made no provider call.
        assert_eq!(mock.count(), 3);
        assert_eq!(count(&db.pool, "status = 'refused'").await, 17);
        // Another requester has their own budget.
        let results = burst(&guard, provider, &[other], 1).await;
        assert_eq!(succeeded(&results), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_never_overshoots_the_organization_budgets() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut accounts = Vec::new();
    for i in 0..6 {
        accounts.push(user(&db.pool, &format!("requester{i}")).await);
    }
    let mock = serve(vec![
        success(Provider::OpenAi, true).after(Duration::from_millis(20)),
    ])
    .await;
    // Two guards on separate pools: the API process and a worker process.
    let worker = TestDb::pool_for(&db).await;
    let api = guard(&db.pool, &mock);
    let background = guard(&worker, &mock);

    limits(
        &db.pool,
        "user_request_limit = NULL, org_request_limit = 5, max_concurrency = 64",
    )
    .await;
    let (a, b) = tokio::join!(
        burst(&api, Provider::OpenAi, &accounts, 12),
        burst(&background, Provider::OpenAi, &accounts, 12)
    );
    let results = [a, b].concat();
    assert_eq!(succeeded(&results), 5);
    assert_eq!(
        refused_with(&results, Refusal::BudgetExhausted(BudgetScope::OrgRequests)),
        19
    );
    assert_eq!(mock.count(), 5);

    // Token budgets admit on the estimate, so calls in flight cannot overshoot.
    sqlx::query("DELETE FROM ai_usage")
        .execute(&db.pool)
        .await
        .unwrap();
    let estimate = i64::try_from(estimate_tokens(&request())).unwrap();
    limits(
        &db.pool,
        &format!(
            "org_request_limit = NULL, org_token_limit = {}",
            estimate * 2 + 1
        ),
    )
    .await;
    // Slow replies: every admission is decided while the first calls are in flight.
    let slow = serve(vec![
        success(Provider::OpenAi, true).after(Duration::from_millis(700)),
    ])
    .await;
    let results = burst(&guard(&db.pool, &slow), Provider::OpenAi, &accounts, 12).await;
    assert_eq!(succeeded(&results), 2);
    assert_eq!(
        refused_with(&results, Refusal::BudgetExhausted(BudgetScope::OrgTokens)),
        10
    );
    // Afterwards the reported usage (165 each) replaces the estimates.
    let results = burst(&api, Provider::OpenAi, &accounts, 1).await;
    assert_eq!(succeeded(&results), 1);

    limits(
        &db.pool,
        &format!("org_token_limit = NULL, user_token_limit = {estimate}"),
    )
    .await;
    let fresh = [user(&db.pool, "fresh").await];
    let results = burst(&guard(&db.pool, &slow), Provider::OpenAi, &fresh, 3).await;
    assert_eq!(succeeded(&results), 1);
    assert_eq!(
        refused_with(&results, Refusal::BudgetExhausted(BudgetScope::UserTokens)),
        2
    );
}

#[tokio::test]
async fn budgets_count_a_rolling_window_and_zero_admits_nothing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    let mock = serve(vec![success(Provider::Gemini, true)]).await;
    let guard = guard(&db.pool, &mock);
    let call = || async {
        guard
            .suggest(
                account,
                &active(Provider::Gemini),
                &request(),
                &Cancel::new(),
            )
            .await
            .map_err(|refused| refused.refusal)
    };
    limits(&db.pool, "window_hours = 2, user_request_limit = 1").await;
    call().await.unwrap();
    let exhausted = Refusal::BudgetExhausted(BudgetScope::UserRequests);
    assert_eq!(call().await.unwrap_err(), exhausted);
    // Refusals do not use up the budget, and old calls leave the window.
    sqlx::query("UPDATE ai_usage SET started_at = started_at - interval '121 minutes'")
        .execute(&db.pool)
        .await
        .unwrap();
    call().await.unwrap();
    assert_eq!(call().await.unwrap_err(), exhausted);

    limits(&db.pool, "user_request_limit = NULL, org_request_limit = 0").await;
    assert_eq!(
        call().await.unwrap_err(),
        Refusal::BudgetExhausted(BudgetScope::OrgRequests)
    );
    assert_eq!(mock.count(), 2);
}

// --- Concurrency -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_waits_for_a_slot_and_never_exceeds_the_concurrency_limit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = NULL, max_concurrency = 2, concurrency_wait_seconds = 30",
    )
    .await;
    for provider in Provider::ALL {
        let mock = serve(vec![
            success(provider, true).after(Duration::from_millis(150)),
        ])
        .await;
        let worker = TestDb::pool_for(&db).await;
        let started = Instant::now();
        let (api, background, accounts) =
            (guard(&db.pool, &mock), guard(&worker, &mock), [account]);
        let (a, b) = tokio::join!(
            burst(&api, provider, &accounts, 4),
            burst(&background, provider, &accounts, 4)
        );
        assert_eq!(succeeded(&[a, b].concat()), 8, "{provider:?}");
        assert_eq!(mock.count(), 8);
        assert!(mock.max_in_flight() <= 2, "{}", mock.max_in_flight());
        // Eight calls, two at a time: at least four rounds.
        assert!(started.elapsed() >= Duration::from_millis(600));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saturated_provider_refuses_after_the_bounded_wait() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = NULL, max_concurrency = 1, concurrency_wait_seconds = 0",
    )
    .await;
    let mock = serve(vec![
        success(Provider::Anthropic, true).after(Duration::from_millis(400)),
    ])
    .await;
    let guard = guard(&db.pool, &mock);
    let results = burst(&guard, Provider::Anthropic, &[account], 5).await;
    assert_eq!(succeeded(&results), 1);
    assert_eq!(refused_with(&results, Refusal::ConcurrencySaturated), 4);
    assert_eq!(mock.count(), 1);
    assert_eq!(mock.max_in_flight(), 1);
    assert_eq!(
        count(
            &db.pool,
            "status = 'refused' AND outcome = 'concurrency_saturated'"
        )
        .await,
        4
    );
    // Refusals for saturation do not use up the request budget.
    limits(&db.pool, "user_request_limit = 2").await;
    assert_eq!(
        succeeded(&burst(&guard, Provider::Anthropic, &[account], 1).await),
        1
    );
}

#[tokio::test]
async fn a_crashed_call_releases_its_slot_when_its_lease_ends() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "max_concurrency = 1, concurrency_wait_seconds = 0",
    )
    .await;
    let mock = serve(vec![
        success(Provider::OpenAi, true).after(Duration::from_secs(5)),
    ])
    .await;
    let guard = guard(&db.pool, &mock);
    // The process dies mid-call: the future is dropped, nothing is recorded.
    let dropped = tokio::time::timeout(
        Duration::from_millis(300),
        guard.suggest(
            account,
            &active(Provider::OpenAi),
            &request(),
            &Cancel::new(),
        ),
    )
    .await;
    assert!(dropped.is_err());
    assert_eq!(count(&db.pool, "status = 'reserved'").await, 1);
    let blocked = guard
        .suggest(
            account,
            &active(Provider::OpenAi),
            &request(),
            &Cancel::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(blocked.refusal, Refusal::ConcurrencySaturated);

    sqlx::query(
        "UPDATE ai_usage SET lease_expires_at = clock_timestamp() WHERE status = 'reserved'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(ai_guard::release_abandoned(&db.pool).await.unwrap(), 1);
    assert_eq!(ai_guard::release_abandoned(&db.pool).await.unwrap(), 0);
    // It still counts against the budgets, with its estimate.
    assert_eq!(
        count(
            &db.pool,
            "status = 'failed' AND outcome = 'abandoned' AND budget_basis = 'estimated'
             AND budget_tokens > 0"
        )
        .await,
        1
    );
    let fast = serve(vec![success(Provider::OpenAi, true)]).await;
    ok_call(
        &guard_with(&db.pool, &fast, Duration::from_secs(10)),
        account,
    )
    .await;
}

async fn ok_call(guard: &AiGuard, account: Uuid) {
    guard
        .suggest(
            account,
            &active(Provider::OpenAi),
            &request(),
            &Cancel::new(),
        )
        .await
        .unwrap();
}

// --- Circuit breaker -----------------------------------------------------------------

#[tokio::test]
async fn the_circuit_opens_after_repeated_failures_and_recovers_through_one_probe() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = NULL, breaker_failure_threshold = 3, breaker_cooldown_seconds = 600",
    )
    .await;
    for provider in Provider::ALL {
        let mock = serve(vec![
            failure(503, "overloaded"),
            failure(503, "overloaded"),
            failure(503, "overloaded"),
            failure(503, "still overloaded"),
            success(provider, true),
        ])
        .await;
        // The API process and a worker process share the breaker.
        let worker = TestDb::pool_for(&db).await;
        let guards = [guard(&db.pool, &mock), guard(&worker, &mock)];
        let call = |i: usize| {
            let guard = guards[i % 2].clone();
            async move {
                guard
                    .suggest(account, &active(provider), &request(), &Cancel::new())
                    .await
                    .map(|_| ())
                    .map_err(|refused| refused.refusal)
            }
        };
        let unavailable = Refusal::ProviderFailed(AiErrorKind::UpstreamUnavailable);
        assert_eq!(call(0).await, Err(unavailable));
        assert_eq!(call(1).await, Err(unavailable));
        assert_eq!(circuit(&db.pool, provider).await, Some((false, 2)));
        assert_eq!(call(0).await, Err(unavailable));
        assert_eq!(circuit(&db.pool, provider).await, Some((true, 3)));

        // Open: refused in either process without a provider call.
        assert_eq!(call(1).await, Err(Refusal::CircuitOpen));
        assert_eq!(call(0).await, Err(Refusal::CircuitOpen));
        assert_eq!(mock.count(), 3);

        // Half-open: one probe. It fails, so the circuit opens again.
        end_cooldown(&db.pool).await;
        assert_eq!(call(1).await, Err(unavailable));
        assert_eq!(mock.count(), 4);
        assert_eq!(circuit(&db.pool, provider).await, Some((true, 4)));
        assert_eq!(call(0).await, Err(Refusal::CircuitOpen));

        // The next probe succeeds: closed, failures forgotten.
        end_cooldown(&db.pool).await;
        assert_eq!(call(0).await, Ok(()));
        assert_eq!(circuit(&db.pool, provider).await, None);
        assert_eq!(call(1).await, Ok(()));
        assert_eq!(mock.count(), 6);
        assert_eq!(count(&db.pool, "breaker_probe").await, 2);
        // The other providers' circuits were never touched.
        let others: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_circuit")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(others, 0);
        sqlx::query("DELETE FROM ai_usage")
            .execute(&db.pool)
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_one_probe_runs_while_the_circuit_is_half_open() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = NULL, max_concurrency = 64, breaker_failure_threshold = 1",
    )
    .await;
    let mock = serve(vec![
        failure(500, "boom"),
        success(Provider::OpenAi, true).after(Duration::from_millis(300)),
    ])
    .await;
    let guard = guard(&db.pool, &mock);
    let results = burst(&guard, Provider::OpenAi, &[account], 1).await;
    assert_eq!(
        results,
        [Err(Refusal::ProviderFailed(
            AiErrorKind::UpstreamUnavailable
        ))]
    );
    end_cooldown(&db.pool).await;
    let results = burst(&guard, Provider::OpenAi, &[account], 10).await;
    assert_eq!(succeeded(&results), 1);
    assert_eq!(refused_with(&results, Refusal::CircuitOpen), 9);
    assert_eq!(mock.count(), 2);
    // Closed again: everyone gets through.
    assert_eq!(
        succeeded(&burst(&guard, Provider::OpenAi, &[account], 5).await),
        5
    );
}

#[tokio::test]
async fn request_specific_failures_do_not_open_the_circuit_and_a_success_resets_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = NULL, breaker_failure_threshold = 2",
    )
    .await;
    let provider = Provider::Anthropic;
    let mock = serve(vec![
        failure(400, "bad request"),
        failure(400, "bad request"),
        failure(400, "bad request"),
        failure(500, "boom"),
        success(provider, true),
        failure(500, "boom"),
        success(provider, true),
    ])
    .await;
    let guard = guard(&db.pool, &mock);
    let mut seen = Vec::new();
    for _ in 0..7 {
        let result = guard
            .suggest(account, &active(provider), &request(), &Cancel::new())
            .await;
        seen.push(result.map(|_| ()).map_err(|refused| refused.refusal));
    }
    let model = Err(Refusal::ProviderFailed(AiErrorKind::ModelError));
    let down = Err(Refusal::ProviderFailed(AiErrorKind::UpstreamUnavailable));
    // Never two counted failures in a row, so the circuit never opened.
    assert_eq!(seen, [model, model, model, down, Ok(()), down, Ok(())]);
    assert_eq!(circuit(&db.pool, provider).await, None);
}

#[tokio::test]
async fn a_new_key_or_model_starts_with_a_closed_circuit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let app = test_app(&db.pool);
    let cookie = login(&app, "admin", PASSWORD).await.session();
    let open = || async {
        for provider in ["openai", "anthropic"] {
            sqlx::query(
                "INSERT INTO ai_circuit (provider, consecutive_failures, open_until, opened_at)
                 VALUES ($1, 9, now() + interval '1 hour', now())
                 ON CONFLICT (provider) DO NOTHING",
            )
            .bind(provider)
            .execute(&db.pool)
            .await
            .unwrap();
        }
    };
    open().await;
    let body = json!({ "api_key": concat!("sk-test-", "0123456789abcdefWXYZ") });
    let path = "/api/settings/ai/credentials/openai";
    let reply = call(&app, "PUT", path, Some(body), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(circuit(&db.pool, Provider::OpenAi).await, None);
    // Only that provider's circuit.
    assert_eq!(
        circuit(&db.pool, Provider::Anthropic).await,
        Some((true, 9))
    );

    open().await;
    let model = supported_models(Provider::OpenAi)[1].id;
    let body = json!({ "enabled": false, "provider": "openai", "model": model });
    let reply = call(
        &app,
        "PUT",
        "/api/settings/ai",
        Some(body.clone()),
        Some(&cookie),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(circuit(&db.pool, Provider::OpenAi).await, None);
    // Saving the same selection again changes nothing.
    open().await;
    call(&app, "PUT", "/api/settings/ai", Some(body), Some(&cookie)).await;
    assert_eq!(circuit(&db.pool, Provider::OpenAi).await, Some((true, 9)));
}

// --- Failure outcomes ------------------------------------------------------------------

#[tokio::test]
async fn every_failure_is_a_distinct_recoverable_outcome() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = NULL, breaker_failure_threshold = 100",
    )
    .await;
    for provider in Provider::ALL {
        sqlx::query("DELETE FROM ai_usage")
            .execute(&db.pool)
            .await
            .unwrap();
        let cases = [
            (
                failure(429, "slow down").header("retry-after", "7"),
                Refusal::RateLimited {
                    retry_after: Some(Duration::from_secs(7)),
                },
                json!(["rate_limited", 429, "none", 0]),
            ),
            (
                failure(401, "bad key"),
                Refusal::ProviderFailed(AiErrorKind::InvalidCredentials),
                json!(["invalid_credentials", 401, "none", 0]),
            ),
            (
                failure(503, "overloaded"),
                Refusal::ProviderFailed(AiErrorKind::UpstreamUnavailable),
                json!(["upstream_unavailable", 503, "none", 0]),
            ),
            (
                Reply::raw(200, "not json"),
                Refusal::ProviderFailed(AiErrorKind::MalformedOutput),
                // A 2xx may have been billed: the estimate stays.
                json!([
                    "malformed_output",
                    200,
                    "estimated",
                    estimate_tokens(&request())
                ]),
            ),
        ];
        for (reply, refusal, stored) in cases {
            let mock = serve(vec![reply]).await;
            let refused = guard(&db.pool, &mock)
                .suggest(account, &active(provider), &request(), &Cancel::new())
                .await
                .unwrap_err();
            assert_eq!(refused.refusal, refusal, "{provider:?}");
            assert!(refused.refusal.provider_called());
            let row: Value = sqlx::query_scalar(
                "SELECT jsonb_build_array(outcome, http_status, budget_basis, budget_tokens)
                 FROM ai_usage WHERE id = $1 AND status = 'failed' AND finished_at IS NOT NULL",
            )
            .bind(refused.usage_id.unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
            assert_eq!(row, stored, "{provider:?}");
        }
        // The same four codes, whatever the provider.
        let codes: Vec<String> =
            sqlx::query_scalar("SELECT outcome FROM ai_usage ORDER BY started_at")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            codes,
            [
                "rate_limited",
                "invalid_credentials",
                "upstream_unavailable",
                "malformed_output"
            ]
        );
    }
}

#[tokio::test]
async fn timeouts_and_cancellation_are_recorded_and_release_the_slot() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "max_concurrency = 1, concurrency_wait_seconds = 0",
    )
    .await;
    let slow = serve(vec![
        success(Provider::Gemini, true).after(Duration::from_secs(5)),
    ])
    .await;
    let guard = guard_with(&db.pool, &slow, Duration::from_millis(200));
    let refused = guard
        .suggest(
            account,
            &active(Provider::Gemini),
            &request(),
            &Cancel::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused.refusal,
        Refusal::ProviderFailed(AiErrorKind::Timeout)
    );

    let cancel = Cancel::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.cancel();
    });
    let refused = guard_with(&db.pool, &slow, Duration::from_secs(10))
        .suggest(account, &active(Provider::Gemini), &request(), &cancel)
        .await
        .unwrap_err();
    assert_eq!(refused.refusal, Refusal::Cancelled);
    // Cancelled before admission: nothing is reserved or called.
    let before = slow.count();
    let refused = guard
        .suggest(account, &active(Provider::Gemini), &request(), &cancel)
        .await
        .unwrap_err();
    assert_eq!(
        (refused.refusal, refused.usage_id),
        (Refusal::Cancelled, None)
    );
    assert_eq!(slow.count(), before);

    assert_eq!(count(&db.pool, "status = 'reserved'").await, 0);
    let stored: Vec<(String, String)> =
        sqlx::query_as("SELECT outcome, budget_basis FROM ai_usage ORDER BY started_at")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    // Both may have been billed, so both keep their estimate.
    assert_eq!(
        stored,
        [
            ("timeout".to_owned(), "estimated".to_owned()),
            ("cancelled".to_owned(), "estimated".to_owned())
        ]
    );
}

// --- Privacy -----------------------------------------------------------------------------

thread_local! {
    /// What this thread logged since [`capture_logs`]. `None`: not capturing.
    static LOGS: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

struct ThreadLogs;

impl std::io::Write for ThreadLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        LOGS.with(|logs| {
            if let Some(buffer) = logs.borrow_mut().as_mut() {
                buffer.extend_from_slice(bytes);
            }
        });
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Captures everything this thread logs from now on, at the most verbose
/// level. One process-wide subscriber writes into a per-thread buffer: a
/// thread-scoped subscriber would race with the other tests' threads over
/// tracing's per-callsite interest cache.
fn capture_logs() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(|| ThreadLogs)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
    });
    LOGS.with(|logs| *logs.borrow_mut() = Some(Vec::new()));
}

fn captured_logs() -> String {
    LOGS.with(|logs| String::from_utf8(logs.borrow().clone().unwrap_or_default()).unwrap())
}

#[tokio::test]
async fn secrets_and_ticket_content_reach_neither_the_provider_storage_nor_logs() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    // A current-thread runtime: every task of this test logs on this thread.
    capture_logs();

    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = 6, breaker_failure_threshold = 100",
    )
    .await;
    let image = png();
    let image_base64 = {
        use base64::{Engine, engine::general_purpose::STANDARD};
        STANDARD.encode(&image.bytes)
    };
    let raw = SuggestionRequest {
        language: Language::De,
        category: "Netzwerkzugang".into(),
        subject: format!("VPN kaputt, token {TOKEN_IN_TICKET}"),
        description: format!(
            "Der Quartalsbericht laesst sich nicht oeffnen. Mein Passwort lautet \
             {PASSWORD_IN_TICKET} und der Server ist smb://anna:{PASSWORD_IN_TICKET}@files.test/x"
        ),
        answers: vec![
            ConditionalAnswer {
                label: "Geraet".into(),
                value: "Thinkpad-Seriennummer".into(),
            },
            ConditionalAnswer {
                label: "Aktuelles Passwort".into(),
                value: "nurbuchstaben".into(),
            },
        ],
        images: vec![image],
    };
    let minimized = minimize(raw);
    assert_eq!(minimized.redactions, 4);
    let request = minimized.request;

    let mut usage_ids = Vec::new();
    for provider in Provider::ALL {
        // A success, then a provider error that echoes the request and the key.
        let echo = format!("{} {KEY} Quartalsbericht", request.description);
        let mock = serve(vec![success(provider, true), failure(500, &echo)]).await;
        let guard = guard(&db.pool, &mock);
        let done = guard
            .suggest(account, &active(provider), &request, &Cancel::new())
            .await
            .unwrap();
        usage_ids.push(done.usage_id);
        let refused = guard
            .suggest(account, &active(provider), &request, &Cancel::new())
            .await
            .unwrap_err();
        usage_ids.push(refused.usage_id.unwrap());

        // What left the server: redacted text, the real image, no secret.
        for body in mock.bodies() {
            assert!(!body.contains(PASSWORD_IN_TICKET), "{provider:?}");
            assert!(!body.contains(TOKEN_IN_TICKET));
            assert!(!body.contains("nurbuchstaben"));
            assert!(body.contains(REDACTED));
            assert!(body.contains("Quartalsbericht"));
            assert!(body.contains("Thinkpad-Seriennummer"));
            assert!(body.contains(&image_base64));
        }
    }
    // A refusal is stored too.
    let mock = serve(vec![success(Provider::OpenAi, true)]).await;
    let refused = guard(&db.pool, &mock)
        .suggest(account, &active(Provider::OpenAi), &request, &Cancel::new())
        .await
        .unwrap_err();
    assert_eq!(
        refused.refusal,
        Refusal::BudgetExhausted(BudgetScope::UserRequests)
    );
    usage_ids.push(refused.usage_id.unwrap());
    ai_guard::purge_expired(&db.pool).await.unwrap();

    // Snapshot of what is stored: exactly these columns, exactly this metadata.
    let stored = rows(&db.pool).await;
    assert_eq!(stored.len(), 7);
    let model = supported_models(Provider::OpenAi)[0].id;
    let estimate = estimate_tokens(&request);
    let row =
        |status: &str, outcome: Value, http: Value, code: Value, tokens: Value, budget: Value| {
            json!({
                "provider": "openai", "model": model, "image_count": 1, "status": status,
                "outcome": outcome, "http_status": http, "provider_code": code,
                "input_tokens": tokens[0], "output_tokens": tokens[1], "total_tokens": tokens[2],
                "budget_tokens": budget[0], "budget_basis": budget[1], "breaker_probe": false,
            })
        };
    let none = json!([null, null, null]);
    assert_eq!(
        stored[0],
        row(
            "succeeded",
            json!(null),
            json!(null),
            json!(null),
            json!([120, 45, 165]),
            json!([165, "reported"])
        )
    );
    assert_eq!(
        stored[1],
        row(
            "failed",
            json!("upstream_unavailable"),
            json!(500),
            json!("server_error"),
            none.clone(),
            json!([0, "none"])
        )
    );
    assert_eq!(
        stored[6],
        row(
            "refused",
            json!("budget_user_requests"),
            json!(null),
            json!(null),
            none,
            json!([0, "none"])
        )
    );
    assert!(estimate > 2_000);

    let database: String = sqlx::query_scalar(
        "SELECT concat((SELECT string_agg(to_jsonb(u)::text, ' ') FROM ai_usage u),
                       (SELECT string_agg(to_jsonb(c)::text, ' ') FROM ai_circuit c),
                       (SELECT string_agg(to_jsonb(a)::text, ' ') FROM audit_events a))",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let logs = captured_logs();
    // The guard did log every call, by id and stable code.
    for id in &usage_ids {
        assert!(logs.contains(&id.to_string()), "{logs}");
    }
    assert!(logs.contains("AI provider call finished"));
    assert!(logs.contains("outcome=\"upstream_unavailable\""), "{logs}");
    assert!(logs.contains("outcome=\"budget_user_requests\""));
    assert!(logs.contains("input_tokens=120"));
    for (place, text) in [("database", &database), ("logs", &logs)] {
        for private in [
            PASSWORD_IN_TICKET,
            TOKEN_IN_TICKET,
            KEY,
            "nurbuchstaben",
            "Quartalsbericht",
            "Netzwerkzugang",
            "VPN kaputt",
            "Thinkpad",
            "Geraet",
            "files.test",
            "The profile is damaged",
            "Restart Outlook",
            image_base64.as_str(),
            &image_base64[..24],
            REDACTED,
        ] {
            assert!(!text.contains(private), "{private} in {place}: {text}");
        }
    }
}

// --- Retention ---------------------------------------------------------------------------

#[tokio::test]
async fn retention_deletes_old_finished_records_and_audits_the_deletion() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let account = user(&db.pool, "requester").await;
    limits(&db.pool, "user_request_limit = NULL, retention_days = 30").await;
    let mock = serve(vec![success(Provider::OpenAi, true)]).await;
    let guard = guard(&db.pool, &mock);
    for _ in 0..4 {
        ok_call(&guard, account).await;
    }
    assert_eq!(ai_guard::purge_expired(&db.pool).await.unwrap(), 0);
    // Two records fall out of the retention; one of them is still in flight.
    sqlx::query(
        "UPDATE ai_usage SET started_at = started_at - interval '31 days'
         WHERE id IN (SELECT id FROM ai_usage ORDER BY id LIMIT 3)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ai_usage SET status = 'reserved', finished_at = NULL,
                lease_expires_at = now() + interval '1 hour'
         WHERE id = (SELECT id FROM ai_usage ORDER BY id LIMIT 1)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(ai_guard::purge_expired(&db.pool).await.unwrap(), 2);
    assert_eq!(ai_guard::purge_expired(&db.pool).await.unwrap(), 0);
    assert_eq!(count(&db.pool, "true").await, 2);
    assert_eq!(count(&db.pool, "status = 'reserved'").await, 1);
    let audit: Vec<(Option<Uuid>, Value)> = sqlx::query_as(
        "SELECT actor_account_id, metadata FROM audit_events WHERE event_type = 'ai_usage_purged'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        audit,
        [(None, json!({ "deleted": 2, "retention_days": 30 }))]
    );
}

// --- Admin API -----------------------------------------------------------------------------

fn limits_body() -> Value {
    json!({
        "window_hours": 24, "org_request_limit": 500, "user_request_limit": 10,
        "org_token_limit": null, "user_token_limit": null, "max_concurrency": 2,
        "concurrency_wait_seconds": 30, "breaker_failure_threshold": 5,
        "breaker_cooldown_seconds": 60, "retention_days": 90,
    })
}

fn without_time(mut value: Value) -> Value {
    value.as_object_mut().unwrap().remove("updated_at");
    value
}

#[tokio::test]
async fn admins_set_bounded_limits_and_every_change_is_audited() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let admin = fixtures::bootstrap_admin(&db.pool).await;
    let app = test_app(&db.pool);
    let cookie = login(&app, "admin", PASSWORD).await.session();
    let path = "/api/settings/ai/limits";

    // Safe defaults exist before any Admin touches them.
    let reply = call(&app, "GET", path, None, Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(without_time(reply.body), limits_body());

    let mut body = limits_body();
    body["user_request_limit"] = json!(3);
    body["org_token_limit"] = json!(2_000_000);
    body["window_hours"] = json!(168);
    let reply = call(&app, "PUT", path, Some(body.clone()), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(without_time(reply.body), body);
    let (actor, metadata): (Option<Uuid>, Value) = sqlx::query_as(
        "SELECT actor_account_id, metadata FROM audit_events WHERE event_type = 'ai_limits_updated'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(actor, Some(admin));
    assert_eq!(metadata, json!({ "from": limits_body(), "to": body }));

    // Out-of-range and extreme values are rejected field by field; nothing changes.
    let mut bad = limits_body();
    bad["window_hours"] = json!(0);
    bad["org_request_limit"] = json!(-1);
    bad["user_token_limit"] = json!(i64::MAX);
    bad["max_concurrency"] = json!(65);
    bad["concurrency_wait_seconds"] = json!(i64::MIN);
    bad["breaker_failure_threshold"] = json!(0);
    bad["breaker_cooldown_seconds"] = json!(86_401);
    bad["retention_days"] = json!(3651);
    let reply = call(&app, "PUT", path, Some(bad), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    let fields = reply.body["error"]["details"]["fields"]
        .as_object()
        .unwrap();
    assert_eq!(fields.len(), 8);
    assert!(
        fields
            .values()
            .all(|codes| codes == &json!(["out_of_range"]))
    );

    let mut short = limits_body();
    short["window_hours"] = json!(744);
    short["retention_days"] = json!(30);
    let reply = call(&app, "PUT", path, Some(short), Some(&cookie)).await;
    assert_eq!(
        reply.body["error"]["details"]["fields"],
        json!({ "retention_days": ["shorter_than_window"] })
    );

    // Not numbers at all, too large for any integer, missing, or unknown fields.
    for (field, value) in [
        ("max_concurrency", json!(1e30)),
        ("max_concurrency", json!(1.5)),
        ("max_concurrency", json!("2")),
        ("max_concurrency", Value::Null),
        ("surprise", json!(1)),
    ] {
        let mut body = limits_body();
        body[field] = value.clone();
        let reply = call(&app, "PUT", path, Some(body), Some(&cookie)).await;
        assert!(
            reply.status.is_client_error(),
            "{field} = {value}: {}",
            reply.status
        );
    }
    let mut missing = limits_body();
    missing.as_object_mut().unwrap().remove("retention_days");
    let reply = call(&app, "PUT", path, Some(missing), Some(&cookie)).await;
    assert!(reply.status.is_client_error());

    let reply = call(&app, "GET", path, None, Some(&cookie)).await;
    assert_eq!(without_time(reply.body), body);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event_type = 'ai_limits_updated'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn only_admins_reach_limits_and_usage() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let app = test_app(&db.pool);
    for (role, name) in [
        (Role::User, "user"),
        (Role::Operator, "operator"),
        (Role::Specialist, "specialist"),
        (Role::TeamLead, "lead"),
    ] {
        fixtures::account(&db.pool, role, name).await;
        let cookie = login(&app, name, PASSWORD).await.session();
        for (method, path, body) in [
            ("GET", "/api/settings/ai/limits", None),
            ("PUT", "/api/settings/ai/limits", Some(limits_body())),
            ("GET", "/api/settings/ai/usage", None),
        ] {
            let reply = call(&app, method, path, body, Some(&cookie)).await;
            assert_eq!(
                reply.status,
                StatusCode::FORBIDDEN,
                "{name} {method} {path}"
            );
        }
    }
    let reply = call(&app, "GET", "/api/settings/ai/usage", None, None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let unchanged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE event_type = 'ai_limits_updated'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(unchanged, 0);
}

#[tokio::test]
async fn the_usage_report_counts_calls_failures_and_reported_tokens_per_provider() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let account = user(&db.pool, "requester").await;
    limits(
        &db.pool,
        "user_request_limit = NULL, org_request_limit = 7, org_token_limit = 900000,
         breaker_failure_threshold = 2",
    )
    .await;
    let run = |provider: Provider, replies: Vec<Reply>, images: usize| {
        let pool = db.pool.clone();
        async move {
            let n = replies.len();
            let mock = serve(replies).await;
            let guard = guard(&pool, &mock);
            let request = SuggestionRequest {
                images: vec![png(); images],
                ..request()
            };
            for _ in 0..n {
                let _ = guard
                    .suggest(account, &active(provider), &request, &Cancel::new())
                    .await;
            }
        }
    };
    run(
        Provider::OpenAi,
        vec![
            success(Provider::OpenAi, true),
            success(Provider::OpenAi, false),
        ],
        2,
    )
    .await;
    // Two failures open Gemini's circuit; the third call is refused by it. The
    // last OpenAI call is refused by the organization budget (7 admitted calls).
    run(
        Provider::Gemini,
        vec![
            failure(503, "down"),
            failure(503, "down"),
            failure(503, "down"),
        ],
        1,
    )
    .await;
    run(
        Provider::Anthropic,
        vec![
            success(Provider::Anthropic, true),
            success(Provider::Anthropic, true),
            failure(429, "slow"),
        ],
        0,
    )
    .await;
    run(Provider::OpenAi, vec![success(Provider::OpenAi, true)], 0).await;

    let app = test_app(&db.pool);
    let cookie = login(&app, "admin", PASSWORD).await.session();
    let reply = call(&app, "GET", "/api/settings/ai/usage", None, Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    let report = reply.body;
    let estimate = estimate_tokens(&SuggestionRequest {
        images: vec![png(); 2],
        ..request()
    });
    assert_eq!(report["days"], 30);
    assert_eq!(
        report["window"],
        json!({
            "window_hours": 24, "requests": 7, "request_limit": 7,
            // 165 + 2 × (120 + 45) reported, one unreported call at its estimate.
            "budget_tokens": 495 + estimate, "estimated_tokens": estimate,
            "token_limit": 900_000, "in_flight": 0, "max_concurrency": 2,
        })
    );
    assert_eq!(
        report["totals"],
        json!({
            "requests": 7, "succeeded": 4, "failed": 3, "refused": 2, "images": 6,
            "input_tokens": 360, "output_tokens": 135, "total_tokens": 165,
            "usage_unreported": 1,
        })
    );
    let providers = report["providers"].as_array().unwrap();
    assert_eq!(providers[0]["provider"], "openai");
    assert_eq!(
        providers[0]["usage"],
        json!({
            "requests": 2, "succeeded": 2, "failed": 0, "refused": 1, "images": 4,
            "input_tokens": 120, "output_tokens": 45, "total_tokens": 165, "usage_unreported": 1,
        })
    );
    assert_eq!(providers[0]["circuit"]["state"], "closed");
    assert_eq!(providers[1]["provider"], "gemini");
    assert_eq!(
        providers[1]["usage"],
        json!({
            "requests": 2, "succeeded": 0, "failed": 2, "refused": 1, "images": 2,
            "input_tokens": null, "output_tokens": null, "total_tokens": null,
            "usage_unreported": 0,
        })
    );
    assert_eq!(providers[1]["circuit"]["state"], "open");
    assert_eq!(providers[1]["circuit"]["consecutive_failures"], 2);
    assert!(providers[1]["circuit"]["open_until"].is_string());
    assert_eq!(providers[2]["provider"], "anthropic");
    // Anthropic reported input and output but no total: the total stays absent.
    assert_eq!(
        providers[2]["usage"],
        json!({
            "requests": 3, "succeeded": 2, "failed": 1, "refused": 0, "images": 0,
            "input_tokens": 240, "output_tokens": 90, "total_tokens": null, "usage_unreported": 0,
        })
    );
    assert_eq!(
        providers[2]["circuit"],
        json!({ "state": "closed", "consecutive_failures": 1, "open_until": null })
    );
    assert_eq!(
        report["outcomes"],
        json!([
            { "status": "failed", "outcome": "upstream_unavailable", "count": 2 },
            { "status": "failed", "outcome": "rate_limited", "count": 1 },
            { "status": "refused", "outcome": "budget_org_requests", "count": 1 },
            { "status": "refused", "outcome": "circuit_open", "count": 1 },
        ])
    );
    // No cost anywhere: Tikkit has no price information.
    assert!(!report.to_string().contains("cost"));

    // The period only narrows the counts; the half-open state shows once the cooldown ends.
    sqlx::query("UPDATE ai_usage SET started_at = started_at - interval '3 days' WHERE provider = 'anthropic'")
        .execute(&db.pool)
        .await
        .unwrap();
    end_cooldown(&db.pool).await;
    let reply = call(
        &app,
        "GET",
        "/api/settings/ai/usage?days=2",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(reply.body["days"], 2);
    assert_eq!(reply.body["totals"]["requests"], 4);
    assert_eq!(reply.body["providers"][2]["usage"]["requests"], 0);
    assert_eq!(reply.body["providers"][1]["circuit"]["state"], "half_open");
    assert_eq!(reply.body["window"]["requests"], 4);
    for days in ["0", "3651", "-1", "99999999999999999999", "x"] {
        let path = format!("/api/settings/ai/usage?days={days}");
        let reply = call(&app, "GET", &path, None, Some(&cookie)).await;
        assert!(reply.status.is_client_error(), "{days}: {}", reply.status);
    }
}

// --- Disabled AI and exhausted limits never block submission -------------------------------

#[tokio::test]
async fn tickets_are_submitted_whatever_the_ai_state() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    user(&db.pool, "requester").await;
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
    let mock = serve(vec![success(Provider::OpenAi, true)]).await;
    let app = test_app_with_provider(&db.pool, &mock.url);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let requester = login(&app, "requester", PASSWORD).await.session();
    let submit = || async {
        let body = json!({
            "idempotency_key": Uuid::new_v4(),
            "intake": {
                "category_id": category, "priority_id": priority, "subject": "Word crashes",
                "description": "On startup", "answers": {}, "screenshot_ids": [],
            },
        });
        call(&app, "POST", "/api/tickets", Some(body), Some(&requester)).await
    };

    // AI disabled (the default): straight to the operator queue.
    let reply = submit().await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["status"], "queued");

    // AI enabled, but every budget is zero and the circuit is open.
    let key = json!({ "api_key": concat!("sk-test-", "0123456789abcdefWXYZ") });
    let path = "/api/settings/ai/credentials/openai";
    assert_eq!(
        call(&app, "PUT", path, Some(key), Some(&admin))
            .await
            .status,
        StatusCode::OK
    );
    let model = supported_models(Provider::OpenAi)[0].id;
    let enable = json!({ "enabled": true, "provider": "openai", "model": model });
    let reply = call(&app, "PUT", "/api/settings/ai", Some(enable), Some(&admin)).await;
    assert_eq!(reply.body["status"], "ready");
    let mut zero = limits_body();
    zero["org_request_limit"] = json!(0);
    zero["user_request_limit"] = json!(0);
    zero["org_token_limit"] = json!(0);
    zero["user_token_limit"] = json!(0);
    let path = "/api/settings/ai/limits";
    assert_eq!(
        call(&app, "PUT", path, Some(zero), Some(&admin))
            .await
            .status,
        StatusCode::OK
    );
    sqlx::query(
        "INSERT INTO ai_circuit (provider, consecutive_failures, open_until, opened_at)
         VALUES ('openai', 5, now() + interval '1 hour', now())",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let reply = submit().await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    // Disabling AI again changes nothing about submission either.
    let disable = json!({ "enabled": false, "provider": "openai", "model": model });
    call(&app, "PUT", "/api/settings/ai", Some(disable), Some(&admin)).await;
    let reply = submit().await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.body["status"], "queued");

    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE status = 'queued'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 3);
    // Submission itself never calls the provider.
    assert_eq!(mock.count(), 0);
    assert_eq!(count(&db.pool, "true").await, 0);
}
