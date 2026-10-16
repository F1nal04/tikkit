//! First-admin setup, login/logout, server-side sessions, CSRF header check,
//! login rate limits, and security audit events. See docs/auth.md.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{
        SaltString,
        rand_core::{OsRng, RngCore},
    },
};
use axum::{
    Extension,
    extract::{ConnectInfo, FromRequestParts, OptionalFromRequestParts, Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{COOKIE, SET_COOKIE, USER_AGENT},
        request::Parts,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgExecutor;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    AppState,
    api::{ApiError, FieldErrors, Json, Language, Role},
    config::{Config, Environment},
};

pub const COOKIE_NAME: &str = "tikkit_session";
/// Required on every state-changing `/api/` request (CSRF defence on top of SameSite=Lax).
pub const CSRF_HEADER: &str = "x-requested-with";
pub const CSRF_VALUE: &str = "tikkit";

const LIMIT_WINDOW: Duration = Duration::from_secs(60);
const LIMIT_PER_IP: u32 = 10;
const LIMIT_PER_USERNAME: u32 = 5;
const LIMIT_MAX_KEYS: usize = 10_000;
/// How long a rotated-away token keeps working, for requests already in flight.
const ROTATION_GRACE: &str = "interval '1 minute'";

#[derive(Clone, Debug)]
pub struct Settings {
    pub session_ttl: Duration,
    pub session_rotate: Duration,
    pub secure_cookie: bool,
    pub trust_proxy_headers: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(14 * 24 * 3600),
            session_rotate: Duration::from_secs(24 * 3600),
            secure_cookie: false,
            trust_proxy_headers: false,
        }
    }
}

impl From<&Config> for Settings {
    fn from(c: &Config) -> Self {
        Self {
            session_ttl: c.session_ttl,
            session_rotate: c.session_rotate,
            secure_cookie: c.environment == Environment::Production,
            trust_proxy_headers: c.trust_proxy_headers,
        }
    }
}

/// Shared per-process auth state, installed as a request extension by `app_with`.
pub struct Auth {
    settings: Settings,
    // ponytail: in-memory limiter, single process; move to Postgres/Redis if multi-instance
    attempts: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Auth {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            attempts: Mutex::default(),
        }
    }

    /// Fixed-window counters; every listed key counts the attempt.
    /// One fixed-window counter. When the table is full of live keys, new keys
    /// are refused rather than growing memory without bound.
    fn hit(&self, key: String, max: u32) -> Result<(), ApiError> {
        let now = Instant::now();
        let mut attempts = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        if attempts.len() >= LIMIT_MAX_KEYS {
            attempts.retain(|_, (start, _)| now - *start < LIMIT_WINDOW);
            if attempts.len() >= LIMIT_MAX_KEYS && !attempts.contains_key(&key) {
                return Err(ApiError::RateLimited(LIMIT_WINDOW.as_secs()));
            }
        }
        let (start, count) = attempts.entry(key).or_insert((now, 0));
        if now - *start >= LIMIT_WINDOW {
            (*start, *count) = (now, 0);
        }
        *count += 1;
        if *count > max {
            let left = LIMIT_WINDOW.saturating_sub(now - *start).as_secs();
            return Err(ApiError::RateLimited(left.max(1)));
        }
        Ok(())
    }

    /// Per-IP first, so one client cannot mint unbounded per-username keys.
    fn limit_attempt(&self, scope: &str, client: &Client, username: &str) -> Result<(), ApiError> {
        let ip = client.ip.as_deref().unwrap_or("unknown");
        self.hit(format!("{scope}:ip:{ip}"), LIMIT_PER_IP)?;
        let username: String = username.chars().take(64).collect();
        self.hit(format!("{scope}:user:{username}"), LIMIT_PER_USERNAME)
    }

    fn cookie(&self, token: &str, expires_at: OffsetDateTime) -> HeaderValue {
        let max_age = (expires_at - OffsetDateTime::now_utc())
            .whole_seconds()
            .max(0);
        self.cookie_value(token, max_age)
    }

    /// Cookie for a session created just now: Max-Age is exactly the session lifetime.
    fn new_cookie(&self, token: &str) -> HeaderValue {
        self.cookie_value(token, self.settings.session_ttl.as_secs() as i64)
    }

    fn clear_cookie(&self) -> HeaderValue {
        self.cookie_value("", 0)
    }

    fn cookie_value(&self, token: &str, max_age: i64) -> HeaderValue {
        let secure = if self.settings.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure}")
            .parse()
            .expect("cookie is a valid header value")
    }
}

/// The signed-in account, resolved by the [`session`] middleware.
#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub session_id: Uuid,
    pub account: Account,
}

impl CurrentUser {
    pub fn account_id(&self) -> Uuid {
        self.account.id
    }
    pub fn role(&self) -> Role {
        self.account.role
    }
    pub fn language(&self) -> Language {
        self.account.language
    }
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or(ApiError::Unauthorized)
    }
}

impl<S: Send + Sync> OptionalFromRequestParts<S> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Option<Self>, ApiError> {
        Ok(parts.extensions.get::<Self>().cloned())
    }
}

/// Client address (from the socket, or the last `X-Forwarded-For` hop when
/// `TRUST_PROXY_HEADERS=true`) and user agent, for rate limits and session records.
pub struct Client {
    ip: Option<String>,
    user_agent: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let trust_proxy = parts
            .extensions
            .get::<Arc<Auth>>()
            .is_some_and(|a| a.settings.trust_proxy_headers);
        let header = |name| parts.headers.get(name).and_then(|v| v.to_str().ok());
        let forwarded = trust_proxy
            .then(|| header("x-forwarded-for")?.rsplit(',').next())
            .flatten()
            .map(|ip| ip.trim().to_owned());
        let ip = forwarded.or_else(|| {
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip().to_string())
        });
        let user_agent = header(USER_AGENT.as_str()).map(|ua| ua.chars().take(256).collect());
        Ok(Self { ip, user_agent })
    }
}

/// Account of the current session.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct Account {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    /// Contact address only; never identifies an account.
    pub email: String,
    pub role: Role,
    pub language: Language,
}

#[derive(Serialize, ToSchema)]
pub struct SetupStatus {
    pub setup_required: bool,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    /// 3-64 characters: letters, digits, `.`, `_`, `-`. Case-insensitive.
    pub username: String,
    pub email: String,
    /// 12-256 characters.
    pub password: String,
    pub language: Language,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    /// 12-256 characters.
    pub new_password: String,
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    session_id: Uuid,
    expires_at: OffsetDateTime,
    rotate: bool,
    stale: bool,
    id: Uuid,
    username: String,
    display_name: String,
    email: String,
    role: String,
    language: String,
}

const ACCOUNT_COLUMNS: &str =
    "a.id, a.username, a.display_name, a.email, a.role::text AS role, a.language";

/// Resolves the session cookie into a [`CurrentUser`] request extension. Rejects
/// revoked, expired, and deactivated-account sessions on every request; rotates
/// the token once the session is older than `SESSION_ROTATE_HOURS`.
pub async fn session(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = cookie_token(request.headers()) else {
        return next.run(request).await;
    };
    let (user, cookie) = match resolve(&state, &auth, &token).await {
        Ok(Some(found)) => found,
        Ok(None) => return next.run(request).await,
        Err(error) => return error.into_response(),
    };
    request.extensions_mut().insert(user);
    let mut response = next.run(request).await;
    // Handlers that set the cookie themselves (login, logout, password change) win.
    if let Some(cookie) = cookie
        && !response.headers().contains_key(SET_COOKIE)
    {
        response.headers_mut().insert(SET_COOKIE, cookie);
    }
    response
}

async fn resolve(
    state: &AppState,
    auth: &Auth,
    token: &str,
) -> Result<Option<(CurrentUser, Option<HeaderValue>)>, ApiError> {
    let row: Option<SessionRow> = sqlx::query_as(&format!(
        "SELECT s.id AS session_id, s.expires_at,
                s.created_at < now() - $2 AS rotate,
                s.last_seen_at < now() - interval '1 hour' AS stale, {ACCOUNT_COLUMNS}
         FROM sessions s JOIN accounts a ON a.id = s.account_id
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now() AND a.is_active
           AND (s.rotated_at IS NULL OR s.rotated_at > now() - {ROTATION_GRACE})"
    ))
    .bind(token_hash(token))
    .bind(auth.settings.session_rotate)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let mut session_id = row.session_id;
    let mut cookie = None;
    if row.rotate {
        // Same absolute expiry. The old token stays valid for ROTATION_GRACE so
        // parallel in-flight requests (e.g. SSR loads) are not logged out; those
        // find it already rotated and are served unrotated.
        let new_token = new_token();
        let rotated: Option<Uuid> = sqlx::query_scalar(
            "WITH old AS (
                UPDATE sessions SET rotated_at = now()
                WHERE id = $1 AND revoked_at IS NULL AND rotated_at IS NULL
                RETURNING id, account_id, expires_at, user_agent, ip)
             INSERT INTO sessions (account_id, token_hash, expires_at, user_agent, ip, rotated_from)
             SELECT account_id, $2, expires_at, user_agent, ip, id FROM old RETURNING id",
        )
        .bind(row.session_id)
        .bind(token_hash(&new_token))
        .fetch_optional(&state.pool)
        .await?;
        if let Some(id) = rotated {
            session_id = id;
            cookie = Some(auth.cookie(&new_token, row.expires_at));
        }
    } else if row.stale {
        sqlx::query("UPDATE sessions SET last_seen_at = now() WHERE id = $1")
            .bind(row.session_id)
            .execute(&state.pool)
            .await?;
    }
    let account = Account {
        id: row.id,
        username: row.username,
        display_name: row.display_name,
        email: row.email,
        role: ident(row.role)?,
        language: ident(row.language)?,
    };
    Ok(Some((
        CurrentUser {
            session_id,
            account,
        },
        cookie,
    )))
}

/// Rejects state-changing requests that lack `X-Requested-With: tikkit`. Browsers
/// cannot add custom headers cross-site without a CORS preflight, which we never grant.
pub async fn csrf(request: Request, next: Next) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let ok = request
        .headers()
        .get(CSRF_HEADER)
        .is_some_and(|v| v == CSRF_VALUE);
    if !safe && !ok {
        return ApiError::CsrfRejected.into_response();
    }
    next.run(request).await
}

/// Whether first-admin setup is still open.
#[utoipa::path(get, path = "/api/setup", tag = "auth",
    responses((status = 200, body = SetupStatus)))]
pub async fn setup_status(State(state): State<AppState>) -> Result<Json<SetupStatus>, ApiError> {
    let setup_required = setup_open(&state).await?;
    Ok(Json(SetupStatus { setup_required }))
}

async fn setup_open(state: &AppState) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT setup_completed_at IS NULL FROM organization")
        .fetch_one(&state.pool)
        .await
}

/// Creates the first Admin and signs them in. Works exactly once per deployment.
#[utoipa::path(post, path = "/api/setup", tag = "auth", request_body = SetupRequest,
    responses(
        (status = 200, description = "Admin created; session cookie set", body = Account),
        (status = 409, description = "`setup_already_completed`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed`", body = crate::api::ErrorEnvelope),
        (status = 429, description = "`rate_limited`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn setup(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    Json(body): Json<SetupRequest>,
) -> Result<Response, ApiError> {
    let username = body.username.trim().to_owned();
    let normalized = username.to_lowercase();
    if !setup_open(&state).await? {
        return Err(ApiError::SetupAlreadyCompleted);
    }
    auth.limit_attempt("setup", &client, &normalized)?;
    let mut errors = FieldErrors::new();
    if let Err(code) = check_username(&normalized) {
        errors.insert("username".into(), vec![code]);
    }
    if !valid_email(&body.email) {
        errors.insert("email".into(), vec!["invalid"]);
    }
    if let Err(code) = check_password(&body.password, &normalized) {
        errors.insert("password".into(), vec![code]);
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let password = body.password;
    let hash = blocking(move || hash_password(&password)).await?;

    let mut tx = state.pool.begin().await?;
    // The row lock makes concurrent attempts wait; all but one then match no row.
    let claimed = sqlx::query(
        "UPDATE organization SET setup_completed_at = now() WHERE setup_completed_at IS NULL",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed != 1 {
        return Err(ApiError::SetupAlreadyCompleted);
    }
    let language = match body.language {
        Language::En => "en",
        Language::De => "de",
    };
    let account_id: Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (username, display_name, email, password_hash, role, language)
         VALUES ($1, $1, $2, $3, 'admin', $4) RETURNING id",
    )
    .bind(&username)
    .bind(body.email.trim())
    .bind(&hash)
    .bind(language)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| unique_violation(e, "username"))?;
    audit(
        &mut *tx,
        "setup_completed",
        Some(account_id),
        Some(account_id),
        json!({}),
    )
    .await?;
    let token = create_session(&mut tx, &auth, account_id, &client).await?;
    let account = load_account(&mut *tx, account_id).await?;
    tx.commit().await?;
    Ok(signed_in(&auth, &token, account))
}

/// Signs in with username and password. Every failure is the same
/// `invalid_credentials`, whether the account is unknown, deactivated, or the password wrong.
#[utoipa::path(post, path = "/api/auth/login", tag = "auth", request_body = LoginRequest,
    responses(
        (status = 200, description = "Signed in; a new session cookie is set", body = Account),
        (status = 401, description = "`invalid_credentials`", body = crate::api::ErrorEnvelope),
        (status = 429, description = "`rate_limited`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn login(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let normalized: String = body
        .username
        .trim()
        .to_lowercase()
        .chars()
        .take(64)
        .collect();
    auth.limit_attempt("login", &client, &normalized)?;
    let found: Option<(Uuid, String, bool)> = sqlx::query_as(
        "SELECT id, password_hash, is_active FROM accounts WHERE username_normalized = $1",
    )
    .bind(&normalized)
    .fetch_optional(&state.pool)
    .await?;
    // Verify against a dummy hash for unknown users so timing does not reveal them.
    let hash = found
        .as_ref()
        .map_or_else(|| DUMMY_HASH.clone(), |f| f.1.clone());
    let password = body.password;
    let verified = blocking(move || verify_password(&password, &hash)).await?;
    let failure = match &found {
        None => Some("unknown_username"),
        Some(_) if !verified => Some("wrong_password"),
        Some((_, _, false)) => Some("account_inactive"),
        Some(_) => None,
    };
    let subject = found.as_ref().map(|f| f.0);
    if let Some(reason) = failure {
        let metadata =
            // No typed username: for unknown users it may be a misplaced password.
            json!({ "reason": reason, "ip": client.ip });
        audit(&state.pool, "login_failed", None, subject, metadata).await?;
        return Err(ApiError::InvalidCredentials);
    }
    let account_id = subject.expect("checked above");
    let mut tx = state.pool.begin().await?;
    // The session this browser held before signing in ends here.
    if let Some(previous) = cookie_token(&headers) {
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sessions WHERE token_hash = $1")
            .bind(token_hash(&previous))
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(id) = id {
            revoke_session(&mut *tx, id).await?;
        }
    }
    let token = create_session(&mut tx, &auth, account_id, &client).await?;
    audit(
        &mut *tx,
        "login_succeeded",
        Some(account_id),
        Some(account_id),
        json!({ "ip": client.ip }),
    )
    .await?;
    let account = load_account(&mut *tx, account_id).await?;
    tx.commit().await?;
    Ok(signed_in(&auth, &token, account))
}

/// Revokes the current session and clears the cookie. Idempotent.
#[utoipa::path(post, path = "/api/auth/logout", tag = "auth",
    responses((status = 204, description = "Signed out; cookie cleared")))]
pub async fn logout(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    user: Option<CurrentUser>,
) -> Result<Response, ApiError> {
    if let Some(user) = user {
        let mut tx = state.pool.begin().await?;
        revoke_session(&mut *tx, user.session_id).await?;
        let id = Some(user.account_id());
        audit(&mut *tx, "logout", id, id, json!({})).await?;
        tx.commit().await?;
    }
    Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, auth.clear_cookie())]).into_response())
}

/// The signed-in account.
#[utoipa::path(get, path = "/api/auth/me", tag = "auth",
    responses(
        (status = 200, body = Account),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn me(user: CurrentUser) -> Json<Account> {
    Json(user.account)
}

/// Changes the password, revokes every session of this account, and issues a fresh one.
#[utoipa::path(post, path = "/api/auth/password", tag = "auth", request_body = ChangePasswordRequest,
    responses(
        (status = 204, description = "Password changed; new session cookie set"),
        (status = 401, description = "`unauthorized`", body = crate::api::ErrorEnvelope),
        (status = 422, description = "`validation_failed` (`current_password: incorrect`, `new_password: too_short|too_long|too_common`)", body = crate::api::ErrorEnvelope),
        (status = 429, description = "`rate_limited`", body = crate::api::ErrorEnvelope),
    ))]
pub async fn change_password(
    State(state): State<AppState>,
    Extension(auth): Extension<Arc<Auth>>,
    client: Client,
    user: CurrentUser,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<Response, ApiError> {
    let username = user.account.username.to_lowercase();
    auth.limit_attempt("password", &client, &username)?;
    if let Err(code) = check_password(&body.new_password, &username) {
        return Err(ApiError::Validation(FieldErrors::from([(
            "new_password".into(),
            vec![code],
        )])));
    }
    let account_id = user.account_id();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(&state.pool)
        .await?;
    let (current, new) = (body.current_password, body.new_password);
    let new_hash = blocking(move || verify_password(&current, &hash).then(|| hash_password(&new)))
        .await?
        .ok_or_else(|| {
            ApiError::Validation(FieldErrors::from([(
                "current_password".into(),
                vec!["incorrect"],
            )]))
        })?;

    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE accounts SET password_hash = $2 WHERE id = $1")
        .bind(account_id)
        .bind(&new_hash)
        .execute(&mut *tx)
        .await?;
    let revoked = sqlx::query(
        "UPDATE sessions SET revoked_at = now() WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let id = Some(account_id);
    audit(&mut *tx, "password_changed", id, id, json!({})).await?;
    audit(
        &mut *tx,
        "session_revoked",
        id,
        id,
        json!({ "reason": "password_changed", "count": revoked }),
    )
    .await?;
    let token = create_session(&mut tx, &auth, account_id, &client).await?;
    tx.commit().await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, auth.new_cookie(&token))],
    )
        .into_response())
}

fn signed_in(auth: &Auth, token: &str, account: Account) -> Response {
    ([(SET_COOKIE, auth.new_cookie(token))], Json(account)).into_response()
}

/// Revokes a session together with its rotation predecessor/successor, so
/// signing out with a token still in its rotation grace window ends both.
async fn revoke_session<'c>(db: impl PgExecutor<'c>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE sessions SET revoked_at = now()
         WHERE revoked_at IS NULL
           AND (id = $1 OR rotated_from = $1 OR id = (SELECT rotated_from FROM sessions WHERE id = $1))",
    )
    .bind(id)
    .execute(db)
    .await
    .map(drop)
}

/// Always a brand-new random token: a cookie presented before login is never adopted.
async fn create_session(
    tx: &mut sqlx::PgConnection,
    auth: &Auth,
    account_id: Uuid,
    client: &Client,
) -> Result<String, sqlx::Error> {
    let token = new_token();
    sqlx::query(
        "INSERT INTO sessions (account_id, token_hash, expires_at, user_agent, ip)
         VALUES ($1, $2, now() + $3, $4, $5)",
    )
    .bind(account_id)
    .bind(token_hash(&token))
    .bind(auth.settings.session_ttl)
    .bind(&client.user_agent)
    .bind(&client.ip)
    .execute(tx)
    .await?;
    Ok(token)
}

async fn load_account<'c>(db: impl PgExecutor<'c>, id: Uuid) -> Result<Account, ApiError> {
    let row: (Uuid, String, String, String, String, String) = sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM accounts a WHERE a.id = $1"
    ))
    .bind(id)
    .fetch_one(db)
    .await?;
    Ok(Account {
        id: row.0,
        username: row.1,
        display_name: row.2,
        email: row.3,
        role: ident(row.4)?,
        language: ident(row.5)?,
    })
}

/// Appends an audit event. Metadata must never hold passwords, tokens, or secrets.
pub async fn audit<'c>(
    db: impl PgExecutor<'c>,
    event_type: &str,
    actor: Option<Uuid>,
    subject: Option<Uuid>,
    metadata: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_events (event_type, actor_account_id, subject_account_id, metadata)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(event_type)
    .bind(actor)
    .bind(subject)
    .bind(metadata)
    .execute(db)
    .await
    .map(drop)
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix("tikkit_session="))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Parses a stored machine identifier (role, language) into its enum.
fn ident<T: DeserializeOwned>(value: String) -> Result<T, ApiError> {
    serde_json::from_value(Value::String(value)).map_err(|_| ApiError::Internal)
}

fn unique_violation(error: sqlx::Error, field: &str) -> ApiError {
    match &error {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            ApiError::Validation(FieldErrors::from([(field.into(), vec!["taken"])]))
        }
        _ => error.into(),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::Internal)
}

static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| hash_password("dummy password for timing"));

/// Argon2id with the crate's default (OWASP-recommended) parameters.
pub fn hash_password(password: &str) -> String {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .expect("argon2 hashing with default parameters")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// `normalized` is `lower(trim(username))`; returns a field error code.
pub fn check_username(normalized: &str) -> Result<(), &'static str> {
    match normalized.chars().count() {
        0..3 => Err("too_short"),
        65.. => Err("too_long"),
        _ if !normalized
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)) =>
        {
            Err("invalid")
        }
        _ => Ok(()),
    }
}

const COMMON_PASSWORDS: &[&str] = &[
    "password1234",
    "password12345",
    "password123456",
    "passwordpassword",
    "123456789012",
    "1234567890123",
    "12345678901234",
    "qwertyuiop12",
    "qwertyuiop123",
    "qwertyuiopasdf",
    "iloveyou1234",
    "letmein12345",
    "welcome12345",
    "changeme1234",
    "administrator",
    "adminadmin12",
    "tikkittikkit",
];

/// Length 12-256, not a well-known password, not one repeated character, not the username.
pub fn check_password(password: &str, normalized_username: &str) -> Result<(), &'static str> {
    let lower = password.to_lowercase();
    let first = password.chars().next();
    match password.chars().count() {
        0..12 => Err("too_short"),
        257.. => Err("too_long"),
        _ if COMMON_PASSWORDS.contains(&lower.as_str())
            || password.chars().all(|c| Some(c) == first)
            || lower.trim() == normalized_username =>
        {
            Err("too_common")
        }
        _ => Ok(()),
    }
}

fn valid_email(email: &str) -> bool {
    let email = email.trim();
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    email.len() <= 254
        && !local.is_empty()
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain.contains('.')
        && !domain.contains('@')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_validated_after_normalization() {
        let norm = |u: &str| u.trim().to_lowercase();
        assert_eq!(norm("  Alice "), "alice");
        assert_eq!(check_username(&norm("  Alice ")), Ok(()));
        assert_eq!(check_username("a.b_c-1"), Ok(()));
        assert_eq!(check_username("ab"), Err("too_short"));
        assert_eq!(check_username(&"a".repeat(65)), Err("too_long"));
        assert_eq!(check_username("al ice"), Err("invalid"));
        assert_eq!(check_username("alice@example"), Err("invalid"));
        assert_eq!(check_username("älice"), Err("invalid"));
    }

    #[test]
    fn password_policy() {
        assert_eq!(check_password("correct horse battery", "alice"), Ok(()));
        assert_eq!(check_password("short", "alice"), Err("too_short"));
        assert_eq!(check_password("", "alice"), Err("too_short"));
        assert_eq!(check_password(&"x".repeat(257), "alice"), Err("too_long"));
        assert_eq!(check_password("Password1234", "alice"), Err("too_common"));
        assert_eq!(check_password("            ", "alice"), Err("too_common"));
        assert_eq!(check_password("aaaaaaaaaaaaaa", "alice"), Err("too_common"));
        assert_eq!(
            check_password("Alice.Admin-1", "alice.admin-1"),
            Err("too_common")
        );
    }

    #[test]
    fn emails_need_basic_syntax_only() {
        assert!(valid_email("a@example.com"));
        assert!(!valid_email("a@example"));
        assert!(!valid_email("example.com"));
        assert!(!valid_email("a b@example.com"));
        assert!(!valid_email("a@@example.com"));
    }

    #[test]
    fn passwords_hash_and_verify() {
        let hash = hash_password("correct horse battery");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn rate_limits_are_per_key_fixed_windows() {
        let auth = Auth::new(Settings::default());
        let hit = |key: &str| auth.hit(key.to_owned(), 2);
        assert!(hit("ip:1").is_ok() && hit("ip:1").is_ok());
        assert!(matches!(hit("ip:1"), Err(ApiError::RateLimited(s)) if (1..=60).contains(&s)));
        assert!(hit("ip:2").is_ok());
    }

    #[test]
    fn rate_limiter_checks_ip_first_and_caps_its_memory() {
        let auth = Auth::new(Settings::default());
        let client = Client {
            ip: Some("10.0.0.1".into()),
            user_agent: None,
        };
        for i in 0..LIMIT_PER_IP {
            assert!(
                auth.limit_attempt("login", &client, &format!("u{i}"))
                    .is_ok()
            );
        }
        assert!(auth.limit_attempt("login", &client, "fresh").is_err());
        let keys = auth.attempts.lock().unwrap().len();
        assert_eq!(
            keys as u32,
            1 + LIMIT_PER_IP,
            "a blocked IP adds no username key"
        );

        let live = auth.attempts.lock().unwrap().len();
        for i in live..LIMIT_MAX_KEYS {
            auth.hit(format!("k{i}"), 1).unwrap();
        }
        assert!(matches!(
            auth.hit("new".into(), 1),
            Err(ApiError::RateLimited(60))
        ));
        assert!(
            auth.hit("k100".into(), 5).is_ok(),
            "existing keys keep counting"
        );
    }
}
