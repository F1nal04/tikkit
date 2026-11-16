//! Admin BYOK AI configuration against real PostgreSQL. No real provider is ever
//! called: the connection test talks to a local mock (`support::provider_mock`).

mod support;

use serde_json::{Value, json};
use support::{
    TestDb, call, fixtures, login,
    provider_mock::{Mock, Reply, serve},
    test_app, test_app_with, test_app_with_provider,
};
use tikkit_api::{
    ai::{AiClient, Endpoint, Provider, ProviderConfig, supported_models},
    ai_config::{self, AiStatus},
    auth::Settings,
};
use uuid::Uuid;

const SECRET: &str = "sk-test-0123456789abcdefWXYZ";
const OTHER_SECRET: &str = "sk-ant-zyxwvutsrqponmlk9876";
/// `Settings::default()` in tests.
const KEY: [u8; 32] = [0; 32];

fn assert_secret_absent(value: &impl std::fmt::Display) {
    let text = value.to_string();
    for secret in [SECRET, OTHER_SECRET] {
        assert!(!text.contains(secret), "{text}");
        // Not even a recognizable prefix.
        assert!(!text.contains(&secret[..12]), "{text}");
    }
}

async fn admin(db: &TestDb) -> (axum::Router, String) {
    fixtures::bootstrap_admin(&db.pool).await;
    let app = test_app(&db.pool);
    let cookie = login(&app, "admin", fixtures::PASSWORD).await.session();
    (app, cookie)
}

async fn set_key(app: &axum::Router, cookie: &str, provider: &str, key: &str) -> support::Reply {
    let path = format!("/api/settings/ai/credentials/{provider}");
    call(
        app,
        "PUT",
        &path,
        Some(json!({ "api_key": key })),
        Some(cookie),
    )
    .await
}

/// The `n`th supported model of `provider`.
fn model(provider: &str, n: usize) -> &'static str {
    supported_models(Provider::parse(provider).unwrap())[n].id
}

/// Selects `provider` with its first supported model.
async fn select(app: &axum::Router, cookie: &str, provider: &str, enabled: bool) -> support::Reply {
    select_model(app, cookie, provider, model(provider, 0), enabled).await
}

async fn select_model(
    app: &axum::Router,
    cookie: &str,
    provider: &str,
    model: &str,
    enabled: bool,
) -> support::Reply {
    let body = json!({ "enabled": enabled, "provider": provider, "model": model });
    call(app, "PUT", "/api/settings/ai", Some(body), Some(cookie)).await
}

async fn test(app: &axum::Router, cookie: &str) -> support::Reply {
    call(app, "POST", "/api/settings/ai/test", None, Some(cookie)).await
}

fn key_header(provider: Provider) -> &'static str {
    match provider {
        Provider::OpenAi => "authorization",
        Provider::Gemini => "x-goog-api-key",
        Provider::Anthropic => "x-api-key",
    }
}

/// The key the adapters send for `active`. A decrypted key is readable only by
/// the adapters, so this is observed at a mock provider.
async fn sent_key(active: &ai_config::ActiveAi) -> String {
    let mock = serve(vec![Reply::json(200, json!({}))]).await;
    let endpoint = Endpoint::Override {
        base_url: mock.url.clone(),
        allow_private_network: true,
    };
    // Any supported model will do: only the key is of interest here.
    let config = ProviderConfig {
        model: supported_models(active.provider)[0].id.into(),
        ..active.provider_config(&endpoint)
    };
    AiClient::new().probe(&config).await.unwrap();
    let value = mock.only().headers[key_header(active.provider)]
        .to_str()
        .unwrap()
        .to_owned();
    value.trim_start_matches("Bearer ").to_owned()
}

async fn get(app: &axum::Router, cookie: &str) -> Value {
    let reply = call(app, "GET", "/api/settings/ai", None, Some(cookie)).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.body
}

fn credential<'a>(settings: &'a Value, provider: &str) -> &'a Value {
    settings["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == provider)
        .map(|p| &p["credential"])
        .unwrap()
}

async fn active(db: &TestDb, key: &[u8; 32]) -> Result<ai_config::ActiveAi, AiStatus> {
    let mut conn = db.pool.acquire().await.unwrap();
    ai_config::load_active(&mut conn, key).await.unwrap()
}

/// A requester and a catalog; returns the requester's session.
async fn requester(db: &TestDb, app: &axum::Router) -> (String, Value) {
    fixtures::account(&db.pool, fixtures::Role::User, "requester").await;
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
    let cookie = login(app, "requester", fixtures::PASSWORD).await.session();
    let intake = json!({ "category_id": category, "priority_id": priority,
        "subject": "Word crashes", "description": "On startup", "answers": {}, "screenshot_ids": [] });
    (cookie, intake)
}

/// Submits a ticket and returns its status and the `submit` event's route.
async fn submit(db: &TestDb, app: &axum::Router, cookie: &str, intake: &Value) -> (String, String) {
    let body = json!({ "idempotency_key": Uuid::new_v4(), "intake": intake });
    let reply = call(app, "POST", "/api/tickets", Some(body), Some(cookie)).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let id: Uuid = serde_json::from_value(reply.body["id"].clone()).unwrap();
    let (status, team, route): (String, Option<Uuid>, String) = sqlx::query_as(
        "SELECT t.status::text, t.team_id, e.payload->>'route'
         FROM tickets t JOIN ticket_events e ON e.ticket_id = t.id WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(team, None);
    (status, route)
}

#[tokio::test]
async fn ai_is_off_by_default_and_only_admins_can_touch_its_configuration() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;
    for (role, name) in [
        (fixtures::Role::User, "ada"),
        (fixtures::Role::Operator, "op"),
        (fixtures::Role::Specialist, "spec"),
        (fixtures::Role::TeamLead, "lead"),
    ] {
        fixtures::account(&db.pool, role, name).await;
    }
    let anon = call(&app, "GET", "/api/settings/ai", None, None).await;
    assert_eq!(anon.code(), "unauthorized");
    assert_eq!(
        call(&app, "GET", "/api/ai/policy", None, None).await.code(),
        "unauthorized"
    );

    let settings = json!({ "enabled": true, "provider": "openai", "model": model("openai", 0) });
    let key = json!({ "api_key": SECRET });
    for name in ["ada", "op", "spec", "lead"] {
        let cookie = login(&app, name, fixtures::PASSWORD).await.session();
        for (method, path, body) in [
            ("GET", "/api/settings/ai", None),
            ("PUT", "/api/settings/ai", Some(&settings)),
            ("PUT", "/api/settings/ai/credentials/openai", Some(&key)),
            ("DELETE", "/api/settings/ai/credentials/openai", None),
            ("POST", "/api/settings/ai/test", None),
        ] {
            let reply = call(&app, method, path, body.cloned(), Some(&cookie)).await;
            assert_eq!(reply.status, 403, "{name} {method} {path} {}", reply.body);
            assert_eq!(reply.code(), "forbidden");
            assert_secret_absent(&reply.body);
        }
        // Everyone may read the effective policy; nobody can change it there.
        let policy = call(&app, "GET", "/api/ai/policy", None, Some(&cookie)).await;
        assert_eq!(policy.status, 200, "{}", policy.body);
        assert_eq!(
            policy.body,
            json!({ "ai_assistance": false, "requester_skip_allowed": false })
        );
        let write = call(
            &app,
            "PUT",
            "/api/ai/policy",
            Some(json!({ "requester_skip_allowed": true })),
            Some(&cookie),
        )
        .await;
        assert_eq!(write.status, 405, "{}", write.body);
        // The skip toggle itself is Admin-only.
        let org = call(&app, "GET", "/api/organization", None, Some(&cookie)).await;
        let mut wanted = org.body.clone();
        wanted["allow_requester_ai_skip"] = json!(true);
        let toggled = call(
            &app,
            "PUT",
            "/api/admin/settings",
            Some(wanted),
            Some(&cookie),
        )
        .await;
        assert_eq!(toggled.status, 403, "{}", toggled.body);
    }
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_credentials")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 0);

    let defaults = get(&app, &admin).await;
    assert_eq!(defaults["enabled"], false);
    assert_eq!(defaults["status"], "disabled");
    assert_eq!(defaults["active"], false);
    assert_eq!(defaults["allow_requester_skip"], false);
    assert!(defaults["provider"].is_null());
    assert_eq!(defaults["connection"]["status"], "untested");
    let providers: Vec<&str> = defaults["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["provider"].as_str().unwrap())
        .collect();
    assert_eq!(providers, ["openai", "gemini", "anthropic"]);
    assert_eq!(active(&db, &KEY).await.unwrap_err(), AiStatus::Disabled);

    // Only the three first-release providers exist.
    for unknown in ["ollama", "azure", "OpenAI"] {
        let reply = set_key(&app, &admin, unknown, SECRET).await;
        assert!(reply.status.is_client_error(), "{}", reply.body);
        assert_secret_absent(&reply.body);
        let body = json!({ "enabled": false, "provider": unknown, "model": "m" });
        let reply = call(&app, "PUT", "/api/settings/ai", Some(body), Some(&admin)).await;
        assert!(reply.status.is_client_error(), "{}", reply.body);
    }
}

#[tokio::test]
async fn keys_are_encrypted_masked_replaceable_and_removable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;

    for (bad, code) in [
        ("", "required"),
        ("short", "invalid"),
        ("has a space in it", "invalid"),
        (&"k".repeat(513), "too_long"),
    ] {
        let reply = set_key(&app, &admin, "openai", bad).await;
        assert_eq!(reply.status, 422, "{}", reply.body);
        assert_eq!(reply.body["error"]["details"]["fields"]["api_key"][0], code);
        if bad.len() > 4 {
            assert!(!reply.body.to_string().contains(bad), "{}", reply.body);
        }
    }

    let saved = set_key(&app, &admin, "openai", SECRET).await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_secret_absent(&saved.body);
    let info = credential(&saved.body, "openai");
    assert_eq!(info["hint"], "WXYZ");
    assert_eq!(info["readable"], true);
    assert_eq!(info["set_by_username"], "admin");
    assert!(info.get("api_key").is_none());
    assert!(credential(&saved.body, "anthropic").is_null());
    assert!(credential(&saved.body, "gemini").is_null());
    assert_secret_absent(&get(&app, &admin).await);

    let first: Vec<u8> =
        sqlx::query_scalar("SELECT key_ciphertext FROM ai_credentials WHERE provider = 'openai'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        !first.windows(8).any(|w| w == &SECRET.as_bytes()[..8]),
        "key stored in plaintext"
    );
    // The row holds nothing else that reveals the key.
    let row: Value = sqlx::query_scalar(
        "SELECT to_jsonb(c) - 'key_ciphertext' FROM ai_credentials c WHERE provider = 'openai'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_secret_absent(&row);

    // Replacement: new ciphertext, new hint, one row.
    let replaced = set_key(&app, &admin, "openai", OTHER_SECRET).await;
    assert_eq!(replaced.status, 200, "{}", replaced.body);
    assert_eq!(credential(&replaced.body, "openai")["hint"], "9876");
    assert_secret_absent(&replaced.body);
    let rows: Vec<Vec<u8>> = sqlx::query_scalar("SELECT key_ciphertext FROM ai_credentials")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0], first);

    let enabled = select(&app, &admin, "openai", true).await;
    assert_eq!(enabled.status, 200, "{}", enabled.body);
    assert_eq!(enabled.body["status"], "ready");
    assert_eq!(
        sent_key(&active(&db, &KEY).await.unwrap()).await,
        OTHER_SECRET
    );

    // Removing the selected provider's key disables AI at once.
    let removed = call(
        &app,
        "DELETE",
        "/api/settings/ai/credentials/openai",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    assert_eq!(removed.body["enabled"], false);
    assert_eq!(removed.body["status"], "disabled");
    assert!(credential(&removed.body, "openai").is_null());
    assert_eq!(active(&db, &KEY).await.unwrap_err(), AiStatus::Disabled);
    let again = call(
        &app,
        "DELETE",
        "/api/settings/ai/credentials/openai",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(again.status, 404, "{}", again.body);
    let back_on = select(&app, &admin, "openai", true).await;
    assert_eq!(back_on.status, 422, "{}", back_on.body);
    assert_eq!(
        back_on.body["error"]["details"]["fields"]["enabled"][0],
        "credential_required"
    );

    let events: Vec<(String, Uuid, Value)> = sqlx::query_as(
        "SELECT event_type, actor_account_id, metadata FROM audit_events
         WHERE event_type LIKE 'ai_%' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let types: Vec<&str> = events.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(
        types,
        [
            "ai_credential_set",
            "ai_credential_set",
            "ai_settings_updated",
            "ai_credential_removed"
        ]
    );
    assert_eq!(
        events[0].2,
        json!({ "provider": "openai", "replaced": false })
    );
    assert_eq!(
        events[1].2,
        json!({ "provider": "openai", "replaced": true })
    );
    assert_eq!(events[2].2["to"]["enabled"], true);
    assert_eq!(events[2].2["from"]["enabled"], false);
    assert_eq!(
        events[3].2,
        json!({ "provider": "openai", "disabled_ai": true })
    );
    for (_, _, metadata) in &events {
        assert_secret_absent(metadata);
        assert!(!metadata.to_string().contains("WXYZ"));
    }
}

#[tokio::test]
async fn a_key_is_bound_to_its_provider_and_never_follows_the_selection() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;
    assert_eq!(set_key(&app, &admin, "openai", SECRET).await.status, 200);
    assert_eq!(select(&app, &admin, "openai", true).await.status, 200);

    // Switching to a provider without its own key cannot be enabled.
    let switched = select(&app, &admin, "anthropic", true).await;
    assert_eq!(switched.status, 422, "{}", switched.body);
    assert_eq!(
        switched.body["error"]["details"]["fields"]["enabled"][0],
        "credential_required"
    );
    let still = active(&db, &KEY).await.unwrap();
    assert_eq!(still.provider, Provider::OpenAi);

    // Even a selection forced past the API finds no key for the other vendor.
    sqlx::query("UPDATE ai_settings SET provider = 'anthropic', enabled = true")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        active(&db, &KEY).await.unwrap_err(),
        AiStatus::CredentialMissing
    );
    assert_eq!(get(&app, &admin).await["status"], "credential_missing");
    // And a ciphertext copied onto the other provider's row does not decrypt.
    sqlx::query(
        "INSERT INTO ai_credentials (provider, key_ciphertext, key_hint)
         SELECT 'anthropic', key_ciphertext, key_hint FROM ai_credentials WHERE provider = 'openai'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        active(&db, &KEY).await.unwrap_err(),
        AiStatus::CredentialUnreadable
    );
    let view = get(&app, &admin).await;
    assert_eq!(view["status"], "credential_unreadable");
    assert_eq!(credential(&view, "anthropic")["readable"], false);
    assert_eq!(credential(&view, "openai")["readable"], true);
    assert_secret_absent(&view);

    // Each provider keeps its own key; selecting back uses the original one.
    assert_eq!(
        set_key(&app, &admin, "anthropic", OTHER_SECRET)
            .await
            .status,
        200
    );
    let anthropic = active(&db, &KEY).await.unwrap();
    assert_eq!(anthropic.provider, Provider::Anthropic);
    assert_eq!(sent_key(&anthropic).await, OTHER_SECRET);
    assert_eq!(select(&app, &admin, "openai", true).await.status, 200);
    let openai = active(&db, &KEY).await.unwrap();
    assert_eq!(
        (openai.provider, sent_key(&openai).await),
        (Provider::OpenAi, SECRET.to_owned())
    );
    // Removing an unselected provider's key leaves AI enabled.
    let removed = call(
        &app,
        "DELETE",
        "/api/settings/ai/credentials/anthropic",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(removed.body["enabled"], true, "{}", removed.body);
    assert_eq!(removed.body["status"], "ready");
}

#[tokio::test]
async fn a_wrong_encryption_key_makes_ai_unavailable_but_not_the_portal() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;
    assert_eq!(set_key(&app, &admin, "gemini", SECRET).await.status, 200);
    assert_eq!(select(&app, &admin, "gemini", true).await.status, 200);

    // The same database behind a deployment whose ENCRYPTION_KEY was lost or changed.
    let wrong = [9u8; 32];
    let other = test_app_with(
        &db.pool,
        Settings {
            encryption_key: wrong,
            ..Settings::default()
        },
    );
    assert_eq!(
        active(&db, &wrong).await.unwrap_err(),
        AiStatus::CredentialUnreadable
    );
    let view = get(&other, &admin).await;
    assert_eq!(view["status"], "credential_unreadable");
    assert_eq!(view["active"], false);
    assert_eq!(credential(&view, "gemini")["readable"], false);
    assert_secret_absent(&view);

    let test = call(&other, "POST", "/api/settings/ai/test", None, Some(&admin)).await;
    assert_eq!(test.status, 409, "{}", test.body);
    assert_eq!(test.code(), "ai_credential_unreadable");
    assert_secret_absent(&test.body);
    let enable = select(&other, &admin, "gemini", true).await;
    assert_eq!(enable.status, 422, "{}", enable.body);
    assert_eq!(
        enable.body["error"]["details"]["fields"]["enabled"][0],
        "credential_unreadable"
    );

    let (cookie, intake) = requester(&db, &other).await;
    let policy = call(&other, "GET", "/api/ai/policy", None, Some(&cookie)).await;
    assert_eq!(policy.body["ai_assistance"], false, "{}", policy.body);
    assert_eq!(
        submit(&db, &other, &cookie, &intake).await,
        ("queued".into(), "ai_disabled".into())
    );

    // Recovery: enter the key again under the current encryption key.
    let recovered = set_key(&other, &admin, "gemini", SECRET).await;
    assert_eq!(recovered.status, 200, "{}", recovered.body);
    assert_eq!(recovered.body["status"], "ready");
    assert_eq!(sent_key(&active(&db, &wrong).await.unwrap()).await, SECRET);
}

#[tokio::test]
async fn submissions_reach_the_operator_queue_whatever_the_ai_configuration() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;
    let (cookie, intake) = requester(&db, &app).await;

    // No key, AI disabled: the default deployment.
    assert_eq!(
        submit(&db, &app, &cookie, &intake).await,
        ("queued".into(), "ai_disabled".into())
    );

    // Fully configured: the stored configuration is usable ...
    assert_eq!(set_key(&app, &admin, "openai", SECRET).await.status, 200);
    let enabled = select(&app, &admin, "openai", true).await;
    assert_eq!(enabled.body["status"], "ready", "{}", enabled.body);
    let ready = active(&db, &KEY).await.unwrap();
    assert_eq!(ready.model, model("openai", 0));
    // ... but until the attempt worker (#35) exists nothing may enter
    // `ai_processing`, where no job would ever pick it up.
    assert_eq!(enabled.body["active"], false);
    let mut conn = db.pool.acquire().await.unwrap();
    assert!(!ai_config::ai_available(&mut conn, &KEY).await.unwrap());
    drop(conn);
    let policy = call(&app, "GET", "/api/ai/policy", None, Some(&cookie)).await;
    assert_eq!(
        policy.body,
        json!({ "ai_assistance": false, "requester_skip_allowed": false })
    );
    assert_eq!(
        submit(&db, &app, &cookie, &intake).await,
        ("queued".into(), "ai_disabled".into())
    );

    // Disabling and removing the key keep the same direct path.
    let disabled = select(&app, &admin, "openai", false).await;
    assert_eq!(disabled.body["status"], "disabled", "{}", disabled.body);
    assert_eq!(
        submit(&db, &app, &cookie, &intake).await,
        ("queued".into(), "ai_disabled".into())
    );
    call(
        &app,
        "DELETE",
        "/api/settings/ai/credentials/openai",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(
        submit(&db, &app, &cookie, &intake).await,
        ("queued".into(), "ai_disabled".into())
    );
    let stranded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets WHERE status IN ('ai_processing', 'awaiting_ai_decision')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(stranded, 0);
}

#[tokio::test]
async fn selection_is_validated_and_the_connection_status_is_safe() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;

    let unset = call(&app, "POST", "/api/settings/ai/test", None, Some(&admin)).await;
    assert_eq!(unset.status, 409, "{}", unset.body);
    assert_eq!(unset.code(), "ai_not_configured");

    let incomplete = call(
        &app,
        "PUT",
        "/api/settings/ai",
        Some(json!({ "enabled": true, "provider": null, "model": " " })),
        Some(&admin),
    )
    .await;
    assert_eq!(incomplete.status, 422, "{}", incomplete.body);
    let fields = &incomplete.body["error"]["details"]["fields"];
    assert_eq!(fields["provider"][0], "required");
    assert_eq!(fields["model"][0], "required");
    // Only models from the supported table of the selected provider are accepted.
    for bad in [
        "https://example.com/model",
        "some-model",
        model("anthropic", 0),
    ] {
        let bad = select_model(&app, &admin, "openai", bad, false).await;
        assert_eq!(bad.status, 422, "{}", bad.body);
        assert_eq!(bad.code(), "validation_failed");
        assert_eq!(
            bad.body["error"]["details"]["fields"]["model"][0],
            "unsupported"
        );
    }
    let no_provider = call(
        &app,
        "PUT",
        "/api/settings/ai",
        Some(json!({ "enabled": false, "provider": null, "model": model("openai", 0) })),
        Some(&admin),
    )
    .await;
    assert_eq!(no_provider.status, 422, "{}", no_provider.body);
    assert_eq!(
        no_provider.body["error"]["details"]["fields"]["provider"][0],
        "required"
    );
    // The Admin UI is fed the same table: every provider with its models.
    let view = get(&app, &admin).await;
    for (state, provider) in view["providers"]
        .as_array()
        .unwrap()
        .iter()
        .zip(Provider::ALL)
    {
        assert_eq!(state["provider"], provider.as_str());
        let models: Vec<Value> = supported_models(provider)
            .iter()
            .map(|m| json!({ "id": m.id, "image_input": m.image_input }))
            .collect();
        assert!(!models.is_empty());
        assert_eq!(state["models"], Value::Array(models));
    }
    let extra = call(
        &app,
        "PUT",
        "/api/settings/ai",
        Some(json!({ "enabled": false, "provider": "openai", "model": model("openai", 0), "base_url": "http://169.254.169.254" })),
        Some(&admin),
    )
    .await;
    assert!(extra.status.is_client_error(), "{}", extra.body);

    // A selection without a key can be saved (disabled) but not tested.
    let saved = select(&app, &admin, "openai", false).await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    let no_key = call(&app, "POST", "/api/settings/ai/test", None, Some(&admin)).await;
    assert_eq!(no_key.code(), "ai_not_configured", "{}", no_key.body);

    assert_eq!(set_key(&app, &admin, "openai", SECRET).await.status, 200);
    // Nothing listens at this app's provider endpoint: a clear, stored, safe failure.
    let test = call(&app, "POST", "/api/settings/ai/test", None, Some(&admin)).await;
    assert_eq!(test.status, 502, "{}", test.body);
    assert_eq!(test.code(), "ai_provider_unreachable");
    assert_secret_absent(&test.body);
    let view = get(&app, &admin).await;
    assert_eq!(view["connection"]["status"], "failed");
    assert_eq!(view["connection"]["error"], "ai_provider_unreachable");
    assert!(view["connection"]["tested_at"].is_string());
    let tested: Value = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'ai_connection_tested'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        tested,
        json!({ "provider": "openai", "model": model("openai", 0), "result": "ai_provider_unreachable" })
    );

    // A result describes one provider, model, and key; any change clears it.
    let same = select(&app, &admin, "openai", false).await;
    assert_eq!(same.body["connection"]["status"], "failed");
    let changed = select_model(&app, &admin, "openai", model("openai", 1), false).await;
    assert_eq!(
        changed.body["connection"]["status"], "untested",
        "{}",
        changed.body
    );
    assert!(changed.body["connection"]["error"].is_null());
    call(&app, "POST", "/api/settings/ai/test", None, Some(&admin)).await;
    let rekeyed = set_key(&app, &admin, "openai", OTHER_SECRET).await;
    assert_eq!(rekeyed.body["connection"]["status"], "untested");
    // Another provider's key does not affect the selected provider's result.
    call(&app, "POST", "/api/settings/ai/test", None, Some(&admin)).await;
    let unrelated = set_key(&app, &admin, "gemini", SECRET).await;
    assert_eq!(unrelated.body["connection"]["status"], "failed");
}

/// A provider error body that repeats the key and carries free text, as real
/// providers do. Neither may come back out of the API.
fn provider_error(status: u16) -> Reply {
    Reply::json(
        status,
        json!({ "error": {
            "message": format!("{PROVIDER_TEXT}: {SECRET}"),
            "type": "provider_error_type",
            "code": "provider_error_code",
        }}),
    )
}

const PROVIDER_TEXT: &str = "Provider says no";

async fn mocked(db: &TestDb, replies: Vec<Reply>) -> (axum::Router, Mock) {
    let mock = serve(replies).await;
    (test_app_with_provider(&db.pool, &mock.url), mock)
}

#[tokio::test]
async fn the_connection_test_probes_the_selected_provider_with_its_own_key() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;
    for (provider, secret) in [
        (Provider::OpenAi, SECRET),
        (Provider::Gemini, OTHER_SECRET),
        (Provider::Anthropic, SECRET),
    ] {
        let name = provider.as_str();
        assert_eq!(set_key(&app, &admin, name, secret).await.status, 200);
        assert_eq!(select(&app, &admin, name, false).await.status, 200);
        let (app, mock) = mocked(&db, vec![Reply::json(200, json!({ "id": "m" }))]).await;
        let ok = test(&app, &admin).await;
        assert_eq!(ok.status, 200, "{}", ok.body);
        assert_eq!(ok.body, json!({ "ok": true }));

        // One metadata read with this provider's key: no prompt, nothing generated.
        let seen = mock.only();
        assert_eq!(seen.method, "GET", "{name}");
        assert!(seen.path.ends_with(model(name, 0)), "{}", seen.path);
        assert_eq!(seen.body, Value::Null);
        let sent = seen.headers[key_header(provider)].to_str().unwrap();
        assert!(sent.ends_with(secret), "{name}");

        let view = get(&app, &admin).await;
        assert_eq!(view["connection"]["status"], "ok", "{view}");
        assert!(view["connection"]["error"].is_null());
        assert!(view["connection"]["tested_at"].is_string());
        assert_secret_absent(&view);
    }
    let results: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE event_type = 'ai_connection_tested' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(results.len(), 3);
    for (result, provider) in results.iter().zip(Provider::ALL) {
        let name = provider.as_str();
        assert_eq!(
            *result,
            json!({ "provider": name, "model": model(name, 0), "result": "ok" })
        );
    }
}

#[tokio::test]
async fn provider_failures_are_stored_as_stable_codes_without_provider_text_or_secrets() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (app, admin) = admin(&db).await;
    assert_eq!(set_key(&app, &admin, "openai", SECRET).await.status, 200);
    assert_eq!(select(&app, &admin, "openai", false).await.status, 200);

    for (upstream, status, code) in [
        (401, 502, "ai_provider_auth_failed"),
        (403, 502, "ai_provider_auth_failed"),
        (404, 409, "ai_model_unsupported"),
        (429, 502, "ai_provider_rate_limited"),
        (500, 502, "ai_provider_unreachable"),
        (503, 502, "ai_provider_unreachable"),
        (302, 502, "ai_provider_unreachable"),
    ] {
        let (app, mock) = mocked(&db, vec![provider_error(upstream)]).await;
        let failed = test(&app, &admin).await;
        assert_eq!(failed.status, status, "{upstream}: {}", failed.body);
        assert_eq!(failed.code(), code, "{upstream}");
        assert_eq!(mock.count(), 1, "the test is a single attempt");

        let view = get(&app, &admin).await;
        assert_eq!(view["connection"]["status"], "failed");
        assert_eq!(view["connection"]["error"], code);
        let audit: Value = sqlx::query_scalar(
            "SELECT metadata FROM audit_events WHERE event_type = 'ai_connection_tested'
             ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            audit,
            json!({ "provider": "openai", "model": model("openai", 0), "result": code })
        );
        for value in [&failed.body, &view, &audit] {
            assert_secret_absent(value);
            let text = value.to_string();
            assert!(!text.contains(PROVIDER_TEXT), "{text}");
            assert!(!text.contains("provider_error"), "{text}");
        }
    }

    // A later success replaces the stored failure.
    let (app, _mock) = mocked(&db, vec![Reply::json(200, json!({}))]).await;
    assert_eq!(test(&app, &admin).await.status, 200);
    let view = get(&app, &admin).await;
    assert_eq!(view["connection"]["status"], "ok");
    assert!(view["connection"]["error"].is_null());

    // A stored model that is no longer in the supported table is refused
    // before any provider call.
    sqlx::query("UPDATE ai_settings SET model = 'retired-model'")
        .execute(&db.pool)
        .await
        .unwrap();
    let (app, mock) = mocked(&db, vec![Reply::json(200, json!({}))]).await;
    let retired = test(&app, &admin).await;
    assert_eq!(retired.status, 409, "{}", retired.body);
    assert_eq!(retired.code(), "ai_model_unsupported");
    assert_eq!(mock.count(), 0);
    assert_eq!(
        get(&app, &admin).await["connection"]["error"],
        "ai_model_unsupported"
    );
}
