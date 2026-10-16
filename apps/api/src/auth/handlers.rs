//! HTTP handlers for first setup, login, logout and the current session.

use axum::{
    Extension, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Actor, ClientIp, SessionCookies, SessionDetails,
    account::{self, SessionInfo},
    audit::{self, AuditActor, AuditEvent, actions},
    cookie, password, sessions,
    sessions::RevokeReason,
    username,
};
use crate::{
    AppState,
    domain::{AccountStatus, Role},
    dto::Language,
    error::{ApiError, ErrorBody, ErrorCode},
    extract::{ApiJson, FieldErrors},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/setup", get(setup_status).post(setup))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/session", get(current_session))
}

/// Adds `Cache-Control: no-store` (responses carrying session data).
fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn rate_limited(secs: u64) -> ApiError {
    ApiError::rate_limited().with_retry_after(secs)
}

// ---------------------------------------------------------------------------
// First setup
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    /// `true` until the initial Admin has been created. Never becomes `true`
    /// again.
    pub setup_required: bool,
}

async fn bootstrap_completed<'e, E>(executor: E) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_scalar("SELECT bootstrap_completed_at IS NOT NULL FROM organization_settings")
        .fetch_one(executor)
        .await
}

/// Whether first setup is still open.
#[utoipa::path(
    get,
    path = "/api/v1/setup",
    tag = "setup",
    operation_id = "getSetupStatus",
    responses(
        (status = 200, description = "Setup state", body = SetupStatus),
        (status = 503, description = "Database unavailable", body = ErrorBody),
    )
)]
pub async fn setup_status(State(state): State<AppState>) -> Result<Response, ApiError> {
    let completed = bootstrap_completed(&state.db).await?;
    Ok(no_store(ApiJson(SetupStatus {
        setup_required: !completed,
    })))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetupRequest {
    /// 3–32 characters of `[a-z0-9._-]` after trimming, NFKC normalization
    /// and lowercasing. Stored as entered (trimmed) for display.
    pub username: String,
    pub display_name: String,
    /// Contact address; not unique.
    pub email: String,
    /// 12–256 characters, not equal to the username.
    pub password: String,
    pub language: Language,
}

/// Creates the initial Admin account and signs it in. One-time: after it
/// succeeded (or concurrently with a winning attempt) it answers `409
/// setup_completed`. All later accounts require an invitation.
#[utoipa::path(
    post,
    path = "/api/v1/setup",
    tag = "setup",
    operation_id = "completeSetup",
    request_body = SetupRequest,
    responses(
        (status = 201, description = "Admin created and signed in; sets the session cookie", body = SessionInfo),
        (status = 400, description = "Malformed request", body = ErrorBody),
        (status = 403, description = "`csrf_rejected`", body = ErrorBody),
        (status = 409, description = "`setup_completed`", body = ErrorBody),
        (status = 422, description = "`validation_failed`", body = ErrorBody),
        (status = 429, description = "`rate_limited` (see `Retry-After`)", body = ErrorBody),
    )
)]
pub async fn setup(
    State(state): State<AppState>,
    client_ip: ClientIp,
    cookies: SessionCookies,
    ApiJson(body): ApiJson<SetupRequest>,
) -> Result<Response, ApiError> {
    state
        .auth
        .setup_limiter
        .check(&client_ip.key())
        .map_err(rate_limited)?;

    // Cheap pre-check so a closed setup never costs a password hash.
    if bootstrap_completed(&state.db).await? {
        return Err(ApiError::new(ErrorCode::SetupCompleted));
    }

    let mut errors = FieldErrors::new();
    let username = username::validate(&mut errors, "username", &body.username);
    account::check_display_name(&mut errors, "displayName", &body.display_name);
    account::check_email(&mut errors, "email", &body.email);
    password::check_policy(
        &mut errors,
        "password",
        &body.password,
        &[body.username.as_str(), &username::normalize(&body.username)],
    );
    errors.finish()?;
    let username = username.expect("validated");

    let password_hash = state.auth.hasher.hash_blocking(body.password).await?;

    let mut tx = state.db.begin().await?;
    // Serializes concurrent setups on the singleton row; the database
    // trigger additionally makes completion irreversible.
    let completed: bool = sqlx::query_scalar(
        "SELECT bootstrap_completed_at IS NOT NULL FROM organization_settings FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    if completed {
        return Err(ApiError::new(ErrorCode::SetupCompleted));
    }

    let account_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO accounts (id, username, username_normalized, email, display_name, role, \
                               language, password_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(account_id)
    .bind(&username.display)
    .bind(&username.normalized)
    .bind(body.email.trim())
    .bind(body.display_name.trim())
    .bind(Role::Admin.as_str())
    .bind(body.language.as_str())
    .bind(&password_hash)
    .execute(&mut *tx)
    .await
    .map_err(|e| username::map_unique_violation(e, "username"))?;

    sqlx::query(
        "UPDATE organization_settings \
         SET bootstrap_completed_at = now(), bootstrap_admin_account_id = $1",
    )
    .bind(account_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        let once = e
            .as_database_error()
            .and_then(|d| d.constraint())
            .is_some_and(|c| c == "organization_settings_bootstrap_once");
        if once {
            ApiError::new(ErrorCode::SetupCompleted)
        } else {
            ApiError::from(e)
        }
    })?;

    let session = sessions::create(&mut *tx, account_id, &state.auth.session_policy).await?;
    audit::record(
        &mut *tx,
        AuditEvent::new(AuditActor::Account(account_id), actions::SETUP_COMPLETED)
            .target("account", account_id)
            .metadata(json!({ "username": username.normalized, "sessionId": session.id })),
    )
    .await?;
    let account = account::load_account(&mut *tx, account_id)
        .await?
        .ok_or_else(|| ApiError::internal("created admin account not found"))?;
    tx.commit().await?;

    tracing::info!(account_id = %account_id, "first setup completed");
    cookies.set(session.token, session.max_age_secs);
    Ok(no_store((
        StatusCode::CREATED,
        ApiJson(SessionInfo {
            account,
            expires_at: session.expires_at,
        }),
    )))
}

// ---------------------------------------------------------------------------
// Login / logout / session
// ---------------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

#[derive(sqlx::FromRow)]
struct LoginCandidate {
    id: Uuid,
    password_hash: String,
    #[sqlx(try_from = "String")]
    status: AccountStatus,
}

/// Signs in with username and password. Always creates a new session; a
/// session cookie sent with the request is revoked (session fixation
/// defence). Unknown usernames, wrong passwords and deactivated accounts get
/// the same `401 invalid_credentials`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    operation_id = "login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Signed in; sets the session cookie", body = SessionInfo),
        (status = 400, description = "Malformed request", body = ErrorBody),
        (status = 401, description = "`invalid_credentials`", body = ErrorBody),
        (status = 403, description = "`csrf_rejected`", body = ErrorBody),
        (status = 429, description = "`rate_limited` (see `Retry-After`)", body = ErrorBody),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    client_ip: ClientIp,
    cookies: SessionCookies,
    headers: HeaderMap,
    ApiJson(body): ApiJson<LoginRequest>,
) -> Result<Response, ApiError> {
    let normalized = username::normalize(&body.username);
    let ip_key = client_ip.key();
    state
        .auth
        .login_ip_limiter
        .check(&ip_key)
        .map_err(rate_limited)?;
    // Bound key size: overly long usernames cannot exist anyway.
    let user_key: String = normalized.chars().take(64).collect();
    state
        .auth
        .login_username_limiter
        .check(&format!("{ip_key}\u{0}{user_key}"))
        .map_err(|secs| {
            tracing::warn!(client = %ip_key, "login rate limit reached for username");
            rate_limited(secs)
        })?;

    let candidate: Option<LoginCandidate> = if normalized.chars().count() <= 32 {
        sqlx::query_as(
            "SELECT id, password_hash, status FROM accounts WHERE username_normalized = $1",
        )
        .bind(&normalized)
        .fetch_optional(&state.db)
        .await?
    } else {
        None
    };
    // Unknown accounts verify against the dummy hash, deactivated accounts
    // against their real hash, so all failures take comparable time.
    let verified = state
        .auth
        .hasher
        .verify_blocking(
            body.password,
            candidate.as_ref().map(|c| c.password_hash.clone()),
        )
        .await?;

    let account_id = match &candidate {
        Some(c) if verified && c.status == AccountStatus::Active => c.id,
        _ => {
            let reason = match &candidate {
                None => "unknown_username",
                Some(c) if c.status != AccountStatus::Active => "account_inactive",
                Some(_) => "wrong_password",
            };
            let mut event = AuditEvent::new(AuditActor::Anonymous, actions::LOGIN_FAILED).metadata(
                json!({ "username": user_key, "reason": reason, "clientIp": client_ip.0 }),
            );
            if let Some(c) = &candidate {
                event = event.target("account", c.id);
            }
            audit::record(&state.db, event).await?;
            return Err(ApiError::new(ErrorCode::InvalidCredentials));
        }
    };

    let mut tx = state.db.begin().await?;
    if let Some(old_token) = cookie::read_token(&headers, &state.config)
        && let Some((old_session, old_account)) =
            sessions::revoke_by_token(&mut *tx, &old_token, RevokeReason::LoginReplaced).await?
    {
        audit::record(
            &mut *tx,
            AuditEvent::new(AuditActor::Account(account_id), actions::SESSION_REVOKED)
                .target("account", old_account)
                .metadata(json!({
                    "sessionId": old_session,
                    "reason": RevokeReason::LoginReplaced.as_str(),
                })),
        )
        .await?;
    }
    let session = sessions::create(&mut *tx, account_id, &state.auth.session_policy).await?;
    audit::record(
        &mut *tx,
        AuditEvent::new(AuditActor::Account(account_id), actions::LOGIN_SUCCEEDED)
            .target("account", account_id)
            .metadata(json!({ "sessionId": session.id, "clientIp": client_ip.0 })),
    )
    .await?;
    let account = account::load_account(&mut *tx, account_id)
        .await?
        .ok_or_else(|| ApiError::internal("signed-in account not found"))?;
    tx.commit().await?;

    cookies.set(session.token, session.max_age_secs);
    Ok(no_store(ApiJson(SessionInfo {
        account,
        expires_at: session.expires_at,
    })))
}

/// Signs out: revokes the presented session server-side and clears the
/// cookie. Idempotent (204 without a session too).
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "auth",
    operation_id = "logout",
    responses(
        (status = 204, description = "Signed out; clears the session cookie"),
        (status = 403, description = "`csrf_rejected`", body = ErrorBody),
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    cookies: SessionCookies,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = cookie::read_token(&headers, &state.config) {
        let mut tx = state.db.begin().await?;
        if let Some((session_id, account_id)) =
            sessions::revoke_by_token(&mut *tx, &token, RevokeReason::Logout).await?
        {
            audit::record(
                &mut *tx,
                AuditEvent::new(AuditActor::Account(account_id), actions::LOGOUT)
                    .target("account", account_id)
                    .metadata(json!({ "sessionId": session_id })),
            )
            .await?;
        }
        tx.commit().await?;
    }
    cookies.clear();
    Ok(no_store(StatusCode::NO_CONTENT))
}

/// The current session. This is the source of truth for the signed-in UI
/// state (e.g. after a page refresh).
#[utoipa::path(
    get,
    path = "/api/v1/auth/session",
    tag = "auth",
    operation_id = "getSession",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "Signed in", body = SessionInfo),
        (status = 401, description = "`unauthenticated` (an invalid cookie is cleared)", body = ErrorBody),
    )
)]
pub async fn current_session(
    State(state): State<AppState>,
    actor: Actor,
    Extension(details): Extension<SessionDetails>,
) -> Result<Response, ApiError> {
    let account = account::load_account(&state.db, actor.account_id)
        .await?
        .ok_or_else(ApiError::unauthenticated)?;
    Ok(no_store(ApiJson(SessionInfo {
        account,
        expires_at: details.expires_at,
    })))
}
