//! Admin-managed BYOK AI configuration. See docs/ai-configuration.md.
//!
//! This module stores which provider and model the organization uses, one
//! encrypted credential per provider, and the result of the last connection
//! test. Its only outbound call is the connection test ([`check_provider`]),
//! through the provider adapters in [`crate::ai`].
//!
//! Later branches read configuration through [`load_active`] (provider, model,
//! and the decrypted key, or the reason AI cannot run) and route tickets through
//! [`ai_available`].
//!
//! API keys are write-only. They are sealed with the external `ENCRYPTION_KEY`
//! and bound to their provider, live in memory only as [`ai::ApiKey`] (redacted
//! `Debug`, readable only by the adapters), and never reach responses, logs,
//! errors, or audit metadata.

use std::sync::Arc;

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use axum::{Extension, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    ai::{self, AiClient, AiErrorKind, ApiKey, Endpoint, ModelInfo, Provider, ProviderConfig},
    ai_attempts, ai_guard,
    api::{ApiError, FieldErrors, Json, Path},
    auth::{Auth, CurrentUser, audit, ident, ident_str},
    authz::{Permission, require_permission},
};

/// Why AI can or cannot run with the stored configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiStatus {
    /// Not enabled by an Admin (the default).
    Disabled,
    /// Enabled, but the selected provider has no stored key.
    CredentialMissing,
    /// Enabled, but the stored key does not decrypt with this deployment's `ENCRYPTION_KEY`.
    CredentialUnreadable,
    /// Enabled, but this version does not support the stored model (or the
    /// model takes no image input). An Admin must select another model.
    ModelUnsupported,
    /// Enabled with a provider, a supported model, and a readable key.
    Ready,
}

/// Everything a provider call needs. The key belongs to `provider` and no other.
#[derive(Clone, Debug)]
pub struct ActiveAi {
    pub provider: Provider,
    pub model: String,
    pub api_key: ApiKey,
}

impl ActiveAi {
    /// What the adapters need for a call with this selection. `endpoint` is
    /// always the process-wide [`AppState::ai_endpoint`].
    pub fn provider_config(&self, endpoint: &Endpoint) -> ProviderConfig {
        ProviderConfig {
            provider: self.provider,
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            endpoint: endpoint.clone(),
        }
    }
}

/// The usable configuration, or the reason there is none (never [`AiStatus::Ready`]).
///
/// Reads `ai_settings` `FOR SHARE`: inside a transaction, a concurrent Admin
/// change waits for the work that relied on this answer. Only the selected
/// provider's key is read and decrypted.
pub async fn load_active(
    db: &mut PgConnection,
    encryption_key: &[u8; 32],
) -> Result<Result<ActiveAi, AiStatus>, sqlx::Error> {
    load(db, encryption_key, true).await
}

/// [`load_active`] without the row lock, for read-only answers (status pages,
/// the requester policy) that nothing in the same transaction relies on.
async fn peek_active(
    db: &mut PgConnection,
    encryption_key: &[u8; 32],
) -> Result<Result<ActiveAi, AiStatus>, sqlx::Error> {
    load(db, encryption_key, false).await
}

async fn load(
    db: &mut PgConnection,
    encryption_key: &[u8; 32],
    lock: bool,
) -> Result<Result<ActiveAi, AiStatus>, sqlx::Error> {
    let (enabled, provider, model): (bool, Option<String>, Option<String>) =
        sqlx::query_as(if lock {
            "SELECT enabled, provider, model FROM ai_settings FOR SHARE"
        } else {
            "SELECT enabled, provider, model FROM ai_settings"
        })
        .fetch_one(&mut *db)
        .await?;
    let (true, Some(provider), Some(model)) = (enabled, provider.and_then(provider_of), model)
    else {
        return Ok(Err(AiStatus::Disabled));
    };
    // The supported table can change between releases; a stored model that
    // dropped out of it must not look usable.
    if model_problem(ai::model_info(provider, &model)).is_some() {
        return Ok(Err(AiStatus::ModelUnsupported));
    }
    let sealed: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT key_ciphertext FROM ai_credentials WHERE provider = $1")
            .bind(provider.as_str())
            .fetch_optional(&mut *db)
            .await?;
    let Some(sealed) = sealed else {
        return Ok(Err(AiStatus::CredentialMissing));
    };
    Ok(match open(encryption_key, provider, &sealed) {
        Some(api_key) => Ok(ActiveAi {
            provider,
            model,
            api_key,
        }),
        None => Err(AiStatus::CredentialUnreadable),
    })
}

/// The single decision whether a new ticket is routed through AI: the stored
/// configuration is usable ([`load_active`]). `lifecycle::policy` asks here.
pub async fn ai_available(
    db: &mut PgConnection,
    encryption_key: &[u8; 32],
) -> Result<bool, sqlx::Error> {
    Ok(load_active(db, encryption_key).await?.is_ok())
}

/// The selected provider and model and the version of that provider's key
/// (when it was stored), or `None` without a selection or key. An attempt
/// records this and is invalid once it differs (docs/ai-attempts.md).
pub async fn selection(
    db: &mut PgConnection,
) -> Result<Option<(String, String, OffsetDateTime)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.provider, s.model, c.set_at
         FROM ai_settings s JOIN ai_credentials c ON c.provider = s.provider
         WHERE s.model IS NOT NULL",
    )
    .fetch_optional(db)
    .await
}

/// Why a connection test failed. The stable codes are stored and shown to Admins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderCheckError {
    /// The provider rejected the key.
    Auth,
    /// DNS, connect, TLS, timeout, or a provider outage (5xx).
    Unreachable,
    /// The provider's rate limit or the account's quota is exhausted.
    RateLimited,
    /// The model is not in the supported table, cannot take image input
    /// (screenshot assistance needs a vision-capable model), or the provider
    /// does not offer it to this key.
    ModelUnsupported,
    /// Any other provider-side failure.
    Failed,
}

impl ProviderCheckError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Auth => "ai_provider_auth_failed",
            Self::Unreachable => "ai_provider_unreachable",
            Self::RateLimited => "ai_provider_rate_limited",
            Self::ModelUnsupported => "ai_model_unsupported",
            Self::Failed => "ai_provider_failed",
        }
    }

    fn api(self) -> ApiError {
        match self {
            Self::Auth => ApiError::AiProviderAuthFailed,
            Self::Unreachable => ApiError::AiProviderUnreachable,
            Self::RateLimited => ApiError::AiProviderRateLimited,
            Self::ModelUnsupported => ApiError::AiModelUnsupported,
            Self::Failed => ApiError::AiProviderFailed,
        }
    }
}

// Only the kind crosses this boundary: the adapter error's HTTP status and
// provider code are neither stored nor returned.
impl From<AiErrorKind> for ProviderCheckError {
    fn from(kind: AiErrorKind) -> Self {
        match kind {
            AiErrorKind::InvalidCredentials => Self::Auth,
            AiErrorKind::Timeout
            | AiErrorKind::UpstreamUnavailable
            | AiErrorKind::DestinationBlocked => Self::Unreachable,
            AiErrorKind::RateLimited => Self::RateLimited,
            AiErrorKind::ModelError | AiErrorKind::Vision => Self::ModelUnsupported,
            AiErrorKind::InvalidInput
            | AiErrorKind::MalformedOutput
            | AiErrorKind::OversizedOutput
            | AiErrorKind::Cancelled => Self::Failed,
        }
    }
}

/// Why `model` cannot be selected, as a field error code: it must be in the
/// adapters' supported table and accept image input.
fn model_problem(model: Option<&ModelInfo>) -> Option<&'static str> {
    match model {
        None => Some("unsupported"),
        Some(model) if !model.image_input => Some("image_input_required"),
        Some(_) => None,
    }
}

/// The connection test: the only place this module reaches a provider.
///
/// `Ok` means `active.model` is a supported, image-capable model, the key
/// authenticates at `active.provider`, and the provider offers that model to
/// it. The probe reads model metadata only; it generates nothing.
async fn check_provider(
    client: &AiClient,
    endpoint: &Endpoint,
    active: &ActiveAi,
) -> Result<(), ProviderCheckError> {
    if model_problem(ai::model_info(active.provider, &active.model)).is_some() {
        return Err(ProviderCheckError::ModelUnsupported);
    }
    client
        .probe(&active.provider_config(endpoint))
        .await
        .map_err(|error| error.kind.into())
}

// --- Admin API ---

/// Masked configuration. No key, ciphertext, or key length is ever included.
#[derive(Serialize, ToSchema)]
pub struct AiSettings {
    pub enabled: bool,
    #[schema(required = true)]
    pub provider: Option<Provider>,
    #[schema(required = true)]
    pub model: Option<String>,
    /// Whether the stored configuration is usable. With `ready`, new tickets
    /// in non-exempt categories get an AI suggestion before human support.
    pub status: AiStatus,
    /// The organization toggle "Allow requester to skip AI" (`PUT /api/admin/settings`).
    pub allow_requester_skip: bool,
    /// Every supported provider, in a fixed order.
    pub providers: Vec<AiProviderState>,
    pub connection: AiConnection,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

#[derive(Serialize, ToSchema)]
pub struct AiProviderState {
    pub provider: Provider,
    /// The models that can be selected for this provider. Nothing else is accepted.
    pub models: Vec<ModelInfo>,
    #[schema(required = true)]
    pub credential: Option<AiCredentialInfo>,
}

/// What an Admin may know about a stored key.
#[derive(Serialize, ToSchema)]
pub struct AiCredentialInfo {
    /// Last four characters of the key, or empty for a short key.
    pub hint: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub set_at: OffsetDateTime,
    #[schema(required = true)]
    pub set_by: Option<Uuid>,
    #[schema(required = true)]
    pub set_by_username: Option<String>,
    /// `false`: the key does not decrypt with the current `ENCRYPTION_KEY`. Enter it again.
    pub readable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiConnectionStatus {
    /// No test since the provider, model, or its key last changed.
    Untested,
    Ok,
    Failed,
}

/// The last connection test of the selected provider and model.
#[derive(Serialize, ToSchema)]
pub struct AiConnection {
    pub status: AiConnectionStatus,
    /// Stable code of the failure (`ai_provider_auth_failed`, ...), if it failed.
    #[schema(required = true)]
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime, required = true)]
    pub tested_at: Option<OffsetDateTime>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiSettingsWrite {
    /// Enabling needs a provider, a model, and a readable stored key for that provider.
    pub enabled: bool,
    pub provider: Option<Provider>,
    /// One of the selected provider's `models` that accepts image input.
    pub model: Option<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiCredentialWrite {
    /// 8 to 512 printable ASCII characters without spaces. Write-only.
    pub api_key: String,
}

#[derive(Serialize, ToSchema)]
pub struct AiTestResult {
    /// The key authenticated and the model accepts image input.
    pub ok: bool,
}

/// What any signed-in account may know: the effective AI policy for new tickets.
#[derive(Serialize, ToSchema)]
pub struct AiPolicy {
    /// New tickets in non-exempt categories get an AI suggestion before human support.
    pub ai_assistance: bool,
    /// The requester may skip AI. Always `false` while `ai_assistance` is `false`.
    /// Only an Admin can change the underlying toggle.
    pub requester_skip_allowed: bool,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    enabled: bool,
    provider: Option<String>,
    model: Option<String>,
    connection_status: String,
    connection_error: Option<String>,
    connection_tested_at: Option<OffsetDateTime>,
    updated_at: OffsetDateTime,
}

const COLUMNS: &str = "enabled, provider, model, connection_status, connection_error,
    connection_tested_at, updated_at";

#[derive(sqlx::FromRow)]
struct CredentialRow {
    provider: String,
    key_ciphertext: Vec<u8>,
    key_hint: String,
    set_at: OffsetDateTime,
    set_by: Option<Uuid>,
    set_by_username: Option<String>,
}

fn provider_of(stored: String) -> Option<Provider> {
    ident(stored).ok()
}

async fn view(db: &mut PgConnection, encryption_key: &[u8; 32]) -> Result<AiSettings, ApiError> {
    let row: SettingsRow = sqlx::query_as(&format!("SELECT {COLUMNS} FROM ai_settings"))
        .fetch_one(&mut *db)
        .await?;
    let credentials: Vec<CredentialRow> = sqlx::query_as(
        "SELECT c.provider, c.key_ciphertext, c.key_hint, c.set_at, c.set_by,
                a.username AS set_by_username
         FROM ai_credentials c LEFT JOIN accounts a ON a.id = c.set_by",
    )
    .fetch_all(&mut *db)
    .await?;
    let allow_requester_skip: bool =
        sqlx::query_scalar("SELECT allow_requester_ai_skip FROM organization WHERE id")
            .fetch_one(&mut *db)
            .await?;
    let status = match peek_active(db, encryption_key).await? {
        Ok(_) => AiStatus::Ready,
        Err(status) => status,
    };
    let providers = Provider::ALL
        .into_iter()
        .map(|provider| AiProviderState {
            provider,
            models: ai::supported_models(provider).to_vec(),
            credential: credentials
                .iter()
                .find(|c| c.provider == provider.as_str())
                .map(|c| AiCredentialInfo {
                    hint: c.key_hint.clone(),
                    set_at: c.set_at,
                    set_by: c.set_by,
                    set_by_username: c.set_by_username.clone(),
                    readable: open(encryption_key, provider, &c.key_ciphertext).is_some(),
                }),
        })
        .collect();
    Ok(AiSettings {
        enabled: row.enabled,
        provider: row.provider.map(ident).transpose()?,
        model: row.model,
        status,
        allow_requester_skip,
        providers,
        connection: AiConnection {
            status: ident(row.connection_status)?,
            error: row.connection_error,
            tested_at: row.connection_tested_at,
        },
        updated_at: row.updated_at,
    })
}

/// Serializes Admin writes and waits for transitions that read the settings `FOR SHARE`.
async fn lock_settings(db: &mut PgConnection) -> Result<SettingsRow, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLUMNS} FROM ai_settings FOR UPDATE"))
        .fetch_one(db)
        .await
}

/// Masked AI configuration (Admin only).
#[utoipa::path(get, path = "/api/settings/ai", tag = "settings",
    responses(
        (status = 200, body = AiSettings),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn get_ai(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
) -> Result<Json<AiSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let mut db = state.pool.acquire().await?;
    Ok(Json(view(&mut db, &auth.settings.encryption_key).await?))
}

/// Selects the provider and model and enables or disables AI (Admin only).
/// Keys are stored separately per provider; changing the selection never moves
/// a key to another provider. Disabling takes effect for the next submission.
#[utoipa::path(put, path = "/api/settings/ai", tag = "settings", request_body = AiSettingsWrite,
    responses(
        (status = 200, body = AiSettings),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`provider`: `required`; `model`: `required`, `unsupported`, or `image_input_required`; `enabled`: `credential_required` or `credential_unreadable`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn put_ai(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
    Json(body): Json<AiSettingsWrite>,
) -> Result<Json<AiSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let key = &auth.settings.encryption_key;
    let model = body
        .model
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty());
    let mut tx = state.pool.begin().await?;
    let old = lock_settings(&mut tx).await?;
    let mut errors = FieldErrors::new();
    if let Some(model) = &model {
        match body.provider {
            None => errors.insert("provider".into(), vec!["required"]),
            Some(provider) => model_problem(ai::model_info(provider, model))
                .and_then(|code| errors.insert("model".into(), vec![code])),
        };
    }
    if body.enabled {
        if body.provider.is_none() {
            errors.insert("provider".into(), vec!["required"]);
        }
        if model.is_none() {
            errors.insert("model".into(), vec!["required"]);
        }
        if let Some(provider) = body.provider {
            let sealed: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT key_ciphertext FROM ai_credentials WHERE provider = $1")
                    .bind(provider.as_str())
                    .fetch_optional(&mut *tx)
                    .await?;
            match sealed {
                None => errors.insert("enabled".into(), vec!["credential_required"]),
                Some(sealed) if open(key, provider, &sealed).is_none() => {
                    errors.insert("enabled".into(), vec!["credential_unreadable"])
                }
                Some(_) => None,
            };
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let provider = body.provider.map(Provider::as_str);
    // A test result describes one provider and model; it does not carry over.
    let retested = old.provider.as_deref() != provider || old.model != model;
    sqlx::query(
        "UPDATE ai_settings SET enabled = $1, provider = $2, model = $3,
                connection_status = CASE WHEN $4 THEN 'untested' ELSE connection_status END,
                connection_error = CASE WHEN $4 THEN NULL ELSE connection_error END,
                connection_tested_at = CASE WHEN $4 THEN NULL ELSE connection_tested_at END,
                updated_at = now(), updated_by = $5",
    )
    .bind(body.enabled)
    .bind(provider)
    .bind(&model)
    .bind(retested)
    .bind(user.account_id())
    .execute(&mut *tx)
    .await?;
    // Failures of the previous model say nothing about the new one.
    if let (true, Some(provider)) = (retested, body.provider) {
        ai_guard::reset_circuit(&mut tx, provider).await?;
    }
    let mut handed_off = 0;
    if retested || old.enabled != body.enabled {
        // Attempts created under the old configuration must not finish under
        // the new one: stop them and hand their tickets to operators now.
        handed_off = if !body.enabled {
            ai_attempts::invalidate(&mut tx, None, ai_attempts::AI_DISABLED).await?
        } else {
            ai_attempts::invalidate(&mut tx, None, ai_attempts::CONFIGURATION_CHANGED).await?
        };
        audit(
            &mut *tx,
            "ai_settings_updated",
            Some(user.account_id()),
            None,
            json!({
                "from": { "enabled": old.enabled, "provider": old.provider, "model": old.model },
                "to": { "enabled": body.enabled, "provider": provider, "model": model },
                "attempts_handed_off": handed_off,
            }),
        )
        .await?;
    }
    let settings = view(&mut tx, key).await?;
    tx.commit().await?;
    state
        .metrics
        .event(crate::metrics::Event::AiHandoff, handed_off);
    Ok(Json(settings))
}

/// Stores or replaces one provider's API key (Admin only). The key is
/// encrypted, bound to this provider, and never returned.
#[utoipa::path(put, path = "/api/settings/ai/credentials/{provider}", tag = "settings",
    request_body = AiCredentialWrite, params(("provider" = Provider, Path)),
    responses(
        (status = 200, body = AiSettings),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`api_key`: `required`, `invalid`, or `too_long`)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn put_credential(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
    Path(provider): Path<Provider>,
    Json(body): Json<AiCredentialWrite>,
) -> Result<Json<AiSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let key = &auth.settings.encryption_key;
    let api_key = body.api_key.trim();
    let problem = match api_key.chars().count() {
        0 => Some("required"),
        n if n > 512 => Some("too_long"),
        n if n < 8 || !api_key.chars().all(|c| c.is_ascii_graphic()) => Some("invalid"),
        _ => None,
    };
    if let Some(code) = problem {
        return Err(ApiError::Validation(FieldErrors::from([(
            "api_key".into(),
            vec![code],
        )])));
    }
    let mut tx = state.pool.begin().await?;
    let settings = lock_settings(&mut tx).await?;
    let replaced: bool = sqlx::query_scalar(
        "WITH old AS (SELECT 1 FROM ai_credentials WHERE provider = $1),
              new AS (
                INSERT INTO ai_credentials (provider, key_ciphertext, key_hint, set_by)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (provider) DO UPDATE SET
                    key_ciphertext = EXCLUDED.key_ciphertext,
                    key_hint = EXCLUDED.key_hint,
                    set_at = now(),
                    set_by = EXCLUDED.set_by
                RETURNING 1)
         SELECT EXISTS (SELECT 1 FROM old) FROM new",
    )
    .bind(provider.as_str())
    .bind(seal(key, provider, api_key))
    .bind(hint(api_key))
    .bind(user.account_id())
    .fetch_one(&mut *tx)
    .await?;
    let mut handed_off = 0;
    if settings.provider.as_deref() == Some(provider.as_str()) {
        reset_connection(&mut tx).await?;
        // Attempts in flight were started with the previous key.
        handed_off =
            ai_attempts::invalidate(&mut tx, None, ai_attempts::CREDENTIAL_CHANGED).await?;
    }
    // A new key starts with a closed circuit: the old key's failures are not its own.
    ai_guard::reset_circuit(&mut tx, provider).await?;
    audit(
        &mut *tx,
        "ai_credential_set",
        Some(user.account_id()),
        None,
        json!({ "provider": provider, "replaced": replaced, "attempts_handed_off": handed_off }),
    )
    .await?;
    let settings = view(&mut tx, key).await?;
    tx.commit().await?;
    state
        .metrics
        .event(crate::metrics::Event::AiHandoff, handed_off);
    Ok(Json(settings))
}

/// Deletes one provider's API key (Admin only). Removing the selected
/// provider's key also disables AI, so the next submission goes to operators.
#[utoipa::path(delete, path = "/api/settings/ai/credentials/{provider}", tag = "settings",
    params(("provider" = Provider, Path)),
    responses(
        (status = 200, body = AiSettings),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 404, description = "`not_found` (no key stored for this provider)", body = crate::api::ErrorEnvelope),
    ))]
pub async fn delete_credential(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
    Path(provider): Path<Provider>,
) -> Result<Json<AiSettings>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    let mut tx = state.pool.begin().await?;
    let settings = lock_settings(&mut tx).await?;
    let removed = sqlx::query("DELETE FROM ai_credentials WHERE provider = $1")
        .bind(provider.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(ApiError::NotFound);
    }
    let selected = settings.provider.as_deref() == Some(provider.as_str());
    let mut handed_off = 0;
    if selected {
        reset_connection(&mut tx).await?;
        sqlx::query("UPDATE ai_settings SET enabled = false, updated_at = now(), updated_by = $1")
            .bind(user.account_id())
            .execute(&mut *tx)
            .await?;
        handed_off =
            ai_attempts::invalidate(&mut tx, None, ai_attempts::CREDENTIAL_MISSING).await?;
    }
    audit(
        &mut *tx,
        "ai_credential_removed",
        Some(user.account_id()),
        None,
        json!({
            "provider": provider,
            "disabled_ai": selected && settings.enabled,
            "attempts_handed_off": handed_off,
        }),
    )
    .await?;
    let settings = view(&mut tx, &auth.settings.encryption_key).await?;
    tx.commit().await?;
    state
        .metrics
        .event(crate::metrics::Event::AiHandoff, handed_off);
    Ok(Json(settings))
}

/// Tests the selected provider and model with the stored key (Admin only):
/// the key must authenticate and the model must accept image input. The
/// outcome is stored as a stable code; provider responses are never echoed.
#[utoipa::path(post, path = "/api/settings/ai/test", tag = "settings",
    responses(
        (status = 200, body = AiTestResult),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 403, description = "`forbidden`", body = crate::api::ErrorEnvelope),
        (status = 409, description = "`ai_not_configured` (no provider, model, or key), `ai_credential_unreadable`, `ai_model_unsupported`, or `outbound_disabled` (restore mode)", body = crate::api::ErrorEnvelope),
        (status = 502, description = "`ai_provider_auth_failed`, `ai_provider_unreachable`, `ai_provider_rate_limited`, or `ai_provider_failed`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn test_ai(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: CurrentUser,
) -> Result<Json<AiTestResult>, ApiError> {
    require_permission(&user, Permission::ManageSettings)?;
    if !auth.settings.outbound_enabled {
        return Err(ApiError::OutboundDisabled);
    }
    let key = &auth.settings.encryption_key;
    // Testing does not require AI to be enabled: an Admin tests first, then enables.
    let (provider, model): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT provider, model FROM ai_settings")
            .fetch_one(&state.pool)
            .await?;
    let (Some(provider), Some(model)) = (provider.and_then(provider_of), model) else {
        return Err(ApiError::AiNotConfigured);
    };
    let sealed: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT key_ciphertext FROM ai_credentials WHERE provider = $1")
            .bind(provider.as_str())
            .fetch_optional(&state.pool)
            .await?;
    let sealed = sealed.ok_or(ApiError::AiNotConfigured)?;
    let api_key = open(key, provider, &sealed).ok_or(ApiError::AiCredentialUnreadable)?;
    let active = ActiveAi {
        provider,
        model,
        api_key,
    };
    // No transaction or lock is held across the provider call.
    let result = check_provider(&state.ai_client, &state.ai_endpoint, &active).await;
    let code = result.err().map(ProviderCheckError::code);
    // Record the outcome only if it still describes the stored selection and
    // key. The audit event is written either way: the test did happen.
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE ai_settings SET connection_status = $1, connection_error = $2,
                connection_tested_at = now()
         WHERE provider = $3 AND model = $4
           AND EXISTS (SELECT 1 FROM ai_credentials WHERE provider = $3 AND key_ciphertext = $5)",
    )
    .bind(ident_str(if code.is_some() {
        AiConnectionStatus::Failed
    } else {
        AiConnectionStatus::Ok
    }))
    .bind(code)
    .bind(provider.as_str())
    .bind(&active.model)
    .bind(&sealed)
    .execute(&mut *tx)
    .await?;
    audit(
        &mut *tx,
        "ai_connection_tested",
        Some(user.account_id()),
        None,
        json!({ "provider": provider, "model": active.model, "result": code.unwrap_or("ok") }),
    )
    .await?;
    tx.commit().await?;
    result.map_err(ProviderCheckError::api)?;
    Ok(Json(AiTestResult { ok: true }))
}

/// The effective AI policy for new tickets, for any signed-in account. Read-only:
/// it reveals whether AI applies and whether skipping is allowed, and nothing
/// about the provider. Category exemptions ("Skip AI automatically") apply on
/// top of this and are decided by Rust at submission.
#[utoipa::path(get, path = "/api/ai/policy", tag = "tickets",
    responses(
        (status = 200, body = AiPolicy),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn policy(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    _user: CurrentUser,
) -> Result<Json<AiPolicy>, ApiError> {
    let mut db = state.pool.acquire().await?;
    let ai_assistance = peek_active(&mut db, &auth.settings.encryption_key)
        .await?
        .is_ok();
    let allow_skip: bool =
        sqlx::query_scalar("SELECT allow_requester_ai_skip FROM organization WHERE id")
            .fetch_one(&mut *db)
            .await?;
    Ok(Json(AiPolicy {
        ai_assistance,
        requester_skip_allowed: ai_assistance && allow_skip,
    }))
}

async fn reset_connection(db: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ai_settings SET connection_status = 'untested', connection_error = NULL,
                connection_tested_at = NULL",
    )
    .execute(db)
    .await
    .map(drop)
}

/// Last four characters, only when the key is long enough that they reveal little.
fn hint(api_key: &str) -> &str {
    if api_key.len() >= 20 {
        &api_key[api_key.len() - 4..]
    } else {
        ""
    }
}

/// Associated data: ties a ciphertext to its provider row.
fn binding(provider: Provider) -> Vec<u8> {
    format!("tikkit:ai-credential:v1:{}", provider.as_str()).into_bytes()
}

fn seal(key: &[u8; 32], provider: Provider, api_key: &str) -> Vec<u8> {
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = Aes256Gcm::new(key.into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: api_key.as_bytes(),
                aad: &binding(provider),
            },
        )
        .expect("aes-256-gcm encrypt");
    [nonce.as_slice(), &ciphertext].concat()
}

fn open(key: &[u8; 32], provider: Provider, sealed: &[u8]) -> Option<ApiKey> {
    if sealed.len() < 12 + 16 {
        return None;
    }
    let (nonce, ciphertext) = sealed.split_at(12);
    let plain = Aes256Gcm::new(key.into())
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &binding(provider),
            },
        )
        .ok()?;
    String::from_utf8(plain).ok().map(ApiKey::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-test-0123456789abcdefWXYZ";

    #[test]
    fn a_sealed_key_opens_only_with_its_key_and_its_provider() {
        let key = [7u8; 32];
        let sealed = seal(&key, Provider::OpenAi, SECRET);
        assert!(!sealed.windows(8).any(|w| w == &SECRET.as_bytes()[..8]));
        let opened = open(&key, Provider::OpenAi, &sealed).unwrap();
        assert_eq!(opened.expose(), SECRET);
        assert!(open(&[8u8; 32], Provider::OpenAi, &sealed).is_none());
        assert!(open(&key, Provider::Anthropic, &sealed).is_none());
        assert!(open(&key, Provider::Gemini, &sealed).is_none());
        assert!(open(&key, Provider::OpenAi, &sealed[..20]).is_none());
    }

    #[test]
    fn keys_are_redacted_and_hints_stay_short() {
        let active = ActiveAi {
            provider: Provider::OpenAi,
            model: "some-model".into(),
            api_key: ApiKey::new(SECRET),
        };
        let text = format!("{active:?}");
        assert!(!text.contains("sk-test"), "{text}");
        assert!(text.contains("<redacted>"));
        assert_eq!(hint(SECRET), "WXYZ");
        assert_eq!(hint("short-key-123"), "");
    }

    #[test]
    fn only_supported_image_capable_models_are_selectable() {
        let model = |image_input| ModelInfo {
            id: "m",
            image_input,
        };
        assert_eq!(model_problem(None), Some("unsupported"));
        assert_eq!(
            model_problem(Some(&model(false))),
            Some("image_input_required")
        );
        assert_eq!(model_problem(Some(&model(true))), None);
        for provider in Provider::ALL {
            assert!(!ai::supported_models(provider).is_empty());
            assert_eq!(
                model_problem(ai::model_info(provider, "some-model")),
                Some("unsupported")
            );
        }
    }

    #[test]
    fn provider_failures_map_to_stable_codes() {
        use AiErrorKind::*;
        let code = |kind| ProviderCheckError::from(kind).code();
        assert_eq!(code(InvalidCredentials), "ai_provider_auth_failed");
        assert_eq!(code(RateLimited), "ai_provider_rate_limited");
        assert_eq!(code(ModelError), "ai_model_unsupported");
        assert_eq!(code(Vision), "ai_model_unsupported");
        for kind in [Timeout, UpstreamUnavailable, DestinationBlocked] {
            assert_eq!(code(kind), "ai_provider_unreachable");
        }
        for kind in [InvalidInput, MalformedOutput, OversizedOutput, Cancelled] {
            assert_eq!(code(kind), "ai_provider_failed");
        }
    }
}
