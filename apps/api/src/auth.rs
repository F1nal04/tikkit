//! First-admin setup, login/logout, server-side sessions, CSRF header check,
//! login rate limits, and security audit events. See docs/auth.md.

use std::{
    collections::HashMap,
    fmt,
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
    security::{Throttle, TrustedProxies, request_client_ip, same_origin},
};

pub const COOKIE_NAME: &str = "tikkit_session";
/// Required on every state-changing `/api/` request (CSRF defence on top of SameSite=Lax).
pub const CSRF_HEADER: &str = "x-requested-with";
pub const CSRF_VALUE: &str = "tikkit";

const LIMIT_WINDOW: Duration = Duration::from_secs(60);
/// Attempts (failed sign-ins) per scope and client network: an IPv4 address
/// or an IPv6 /64, the smallest network one subscriber normally gets.
const LIMIT_PER_IP: u32 = 10;
/// Failed attempts per scope, username, and client network.
const LIMIT_PER_USERNAME_AND_IP: u32 = 5;
/// Failed attempts per scope and username from all networks together: slows
/// a guessing botnet, and does not apply to a network the account has
/// signed in from successfully ([`KNOWN_GOOD_FOR`]), so nobody can lock the
/// legitimate user out of their own place.
const LIMIT_PER_USERNAME: u32 = 30;
/// How long a successful sign-in exempts its (username, network) pair from
/// [`LIMIT_PER_USERNAME`].
const KNOWN_GOOD_FOR: Duration = Duration::from_secs(30 * 24 * 3600);
const LIMIT_MAX_KEYS: usize = 10_000;
/// Audit rows one account may cause per window by repeating one denied action on one ticket.
const LIMIT_DENIALS: u32 = 5;
/// How long a rotated-away token keeps working, for requests already in flight.
const ROTATION_GRACE: &str = "interval '1 minute'";

#[derive(Clone)]
pub struct Settings {
    pub session_ttl: Duration,
    pub session_rotate: Duration,
    pub secure_cookie: bool,
    /// Peers whose `X-Forwarded-For` is honoured (`TRUSTED_PROXIES`).
    pub trusted_proxies: TrustedProxies,
    /// Send `Strict-Transport-Security` (the portal is served over https).
    pub hsts: bool,
    /// Deadline for one API request ([`crate::security::REQUEST_TIMEOUT`];
    /// tests shorten it).
    pub request_timeout: Duration,
    /// Origin for links in invitations and emails, without trailing slash.
    pub public_origin: String,
    /// Decrypts the stored SMTP password. Never logged.
    pub encryption_key: [u8; 32],
    /// Test-only PEM trust anchor for the SMTP client. Production leaves this
    /// empty so verification uses the default WebPKI roots. Never disables checks.
    pub smtp_trust_root_pem: Option<Vec<u8>>,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("session_ttl", &self.session_ttl)
            .field("session_rotate", &self.session_rotate)
            .field("secure_cookie", &self.secure_cookie)
            .field("trusted_proxies", &self.trusted_proxies)
            .field("hsts", &self.hsts)
            .field("request_timeout", &self.request_timeout)
            .field("public_origin", &self.public_origin)
            .field("encryption_key", &"<redacted>")
            .field(
                "smtp_trust_root_pem",
                &self.smtp_trust_root_pem.as_ref().map(|_| "<set>"),
            )
            .finish()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(14 * 24 * 3600),
            session_rotate: Duration::from_secs(24 * 3600),
            secure_cookie: false,
            trusted_proxies: TrustedProxies::default(),
            hsts: false,
            request_timeout: crate::security::REQUEST_TIMEOUT,
            public_origin: "http://localhost:3000".into(),
            encryption_key: [0; 32],
            smtp_trust_root_pem: None,
        }
    }
}

impl From<&Config> for Settings {
    fn from(c: &Config) -> Self {
        Self {
            session_ttl: c.session_ttl,
            session_rotate: c.session_rotate,
            secure_cookie: c.environment == Environment::Production,
            trusted_proxies: c.trusted_proxies.clone(),
            hsts: c.public_origin.scheme() == "https",
            request_timeout: crate::security::REQUEST_TIMEOUT,
            public_origin: c.public_origin.as_str().trim_end_matches('/').into(),
            encryption_key: c.encryption_key,
            smtp_trust_root_pem: None,
        }
    }
}

/// Shared per-process auth state, installed as a request extension by `app_with`.
pub struct Auth {
    pub(crate) settings: Settings,
    // ponytail: in-memory limiter, single process; move to Postgres/Redis if multi-instance
    attempts: Limiter,
    /// Per-account route budgets ([`crate::security::Throttle`]), kept apart so
    /// they can never fill the table that guards sign-in.
    actions: Limiter,
    /// `username|network` pairs that signed in successfully, with the time.
    known_good: Mutex<HashMap<String, Instant>>,
}

/// Fixed-window counters keyed by string, bounded to [`LIMIT_MAX_KEYS`] keys.
#[derive(Default)]
struct Limiter(Mutex<HashMap<String, (Instant, u32)>>);

impl Auth {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            attempts: Limiter::default(),
            actions: Limiter::default(),
            known_good: Mutex::default(),
        }
    }

    fn hit(&self, key: String, max: u32) -> Result<(), ApiError> {
        self.attempts.hit(key, max)
    }

    /// One request against `throttle`'s per-minute budget for `account`.
    pub(crate) fn limit_account(&self, throttle: Throttle, account: Uuid) -> Result<(), ApiError> {
        let (scope, max) = throttle.budget();
        self.actions
            .hit(format!("{scope}:{}", account.simple()), max)
    }
}

impl Limiter {
    /// Counts one request and refuses it once the window holds more than `max`.
    fn hit(&self, key: String, max: u32) -> Result<(), ApiError> {
        let now = Instant::now();
        let mut table = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (start, count) = Self::entry(&mut table, key, now);
        *count += 1;
        if *count > max {
            return Err(ApiError::RateLimited(Self::left(*start, now)));
        }
        Ok(())
    }

    /// Refuses without counting when the window already holds `max`.
    fn check(&self, key: &str, max: u32) -> Result<(), ApiError> {
        let now = Instant::now();
        let table = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match table.get(key) {
            Some(&(start, count)) if now - start < LIMIT_WINDOW && count >= max => {
                Err(ApiError::RateLimited(Self::left(start, now)))
            }
            _ => Ok(()),
        }
    }

    /// Counts without deciding (a failure recorded after the fact).
    fn count(&self, key: String) {
        let now = Instant::now();
        let mut table = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *Self::entry(&mut table, key, now).1 += 1;
    }

    /// The live counter for `key`. A full table first drops expired windows,
    /// then the oldest one: memory stays bounded and new clients are never
    /// refused for want of room (an evicted counter just starts again).
    fn entry(
        table: &mut HashMap<String, (Instant, u32)>,
        key: String,
        now: Instant,
    ) -> (&mut Instant, &mut u32) {
        if table.len() >= LIMIT_MAX_KEYS && !table.contains_key(&key) {
            table.retain(|_, (start, _)| now - *start < LIMIT_WINDOW);
            if table.len() >= LIMIT_MAX_KEYS
                && let Some(oldest) = table
                    .iter()
                    .min_by_key(|(_, (start, _))| *start)
                    .map(|(k, _)| k.clone())
            {
                table.remove(&oldest);
            }
        }
        let (start, count) = table.entry(key).or_insert((now, 0));
        if now - *start >= LIMIT_WINDOW {
            (*start, *count) = (now, 0);
        }
        (start, count)
    }

    fn left(start: Instant, now: Instant) -> u64 {
        LIMIT_WINDOW.saturating_sub(now - start).as_secs().max(1)
    }
}

impl Auth {
    /// Whether another denied `action` of `account` on `ticket` may still be
    /// written to the audit log in this window. Repeats beyond it change nothing
    /// for the caller; they are just not recorded again.
    pub(crate) fn allow_denial_record(&self, account: Uuid, ticket: Uuid, action: &str) -> bool {
        let key = format!("denied:{}:{}:{action}", account.simple(), ticket.simple());
        self.hit(key, LIMIT_DENIALS).is_ok()
    }

    /// Per client network only, for endpoints keyed by an unguessable token
    /// rather than a username. Every request counts.
    pub(crate) fn limit_ip(&self, scope: &str, client: &Client) -> Result<(), ApiError> {
        self.hit(format!("{scope}:ip:{}", client.network()), LIMIT_PER_IP)
    }

    /// Every request counts (setup, invitation acceptance, recovery): checked
    /// like [`Auth::check_attempt`], then recorded like a failure.
    pub(crate) fn limit_attempt(
        &self,
        scope: &str,
        client: &Client,
        username: &str,
    ) -> Result<(), ApiError> {
        self.check_attempt(scope, client, username)?;
        self.record_failure(scope, client, username);
        Ok(())
    }

    /// Whether a credential attempt may proceed. Counts nothing, so only
    /// failures ([`Auth::record_failure`]) spend the budgets, and answers the
    /// same for known and unknown usernames.
    pub(crate) fn check_attempt(
        &self,
        scope: &str,
        client: &Client,
        username: &str,
    ) -> Result<(), ApiError> {
        let [ip, pair, user] = attempt_keys(scope, client, username);
        self.attempts.check(&ip, LIMIT_PER_IP)?;
        self.attempts.check(&pair, LIMIT_PER_USERNAME_AND_IP)?;
        if !self.is_known_good(client, username) {
            self.attempts.check(&user, LIMIT_PER_USERNAME)?;
        }
        Ok(())
    }

    /// One failed credential attempt against every budget it belongs to.
    pub(crate) fn record_failure(&self, scope: &str, client: &Client, username: &str) {
        for key in attempt_keys(scope, client, username) {
            self.attempts.count(key);
        }
    }

    /// A successful sign-in: this network is the account's own from now on.
    pub(crate) fn remember_success(&self, client: &Client, username: &str) {
        let now = Instant::now();
        let mut known = self.known_good.lock().unwrap_or_else(|e| e.into_inner());
        if known.len() >= LIMIT_MAX_KEYS {
            known.retain(|_, at| now - *at < KNOWN_GOOD_FOR);
            if known.len() >= LIMIT_MAX_KEYS
                && let Some(oldest) = known
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(k, _)| k.clone())
            {
                known.remove(&oldest);
            }
        }
        known.insert(known_good_key(client, username), now);
    }

    fn is_known_good(&self, client: &Client, username: &str) -> bool {
        let known = self.known_good.lock().unwrap_or_else(|e| e.into_inner());
        known
            .get(&known_good_key(client, username))
            .is_some_and(|at| at.elapsed() < KNOWN_GOOD_FOR)
    }

    fn cookie(&self, token: &str, expires_at: OffsetDateTime) -> HeaderValue {
        let max_age = (expires_at - OffsetDateTime::now_utc())
            .whole_seconds()
            .max(0);
        self.cookie_value(token, max_age)
    }

    /// Cookie for a session created just now: Max-Age is exactly the session lifetime.
    pub(crate) fn new_cookie(&self, token: &str) -> HeaderValue {
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

/// `[network, username+network, username]` budget keys of one attempt.
fn attempt_keys(scope: &str, client: &Client, username: &str) -> [String; 3] {
    let username: String = username.chars().take(64).collect();
    let net = client.network();
    [
        format!("{scope}:ip:{net}"),
        format!("{scope}:pair:{username}|{net}"),
        format!("{scope}:user:{username}"),
    ]
}

fn known_good_key(client: &Client, username: &str) -> String {
    let username: String = username.chars().take(64).collect();
    format!("{username}|{}", client.network())
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

/// Client address (the socket peer, or the client a `TRUSTED_PROXIES` peer
/// forwarded for; see [`TrustedProxies::client_ip`]) and user agent, for rate
/// limits, session records, and audit events.
pub struct Client {
    pub(crate) ip: Option<String>,
    user_agent: Option<String>,
}

impl Client {
    /// The rate-limit network of this client: its IPv4 address, or its IPv6
    /// /64 (one subscriber's usual allocation, which it can fill with
    /// addresses at will). IPv4-mapped IPv6 counts as IPv4.
    pub(crate) fn network(&self) -> String {
        match self.ip.as_deref().map(str::parse::<std::net::IpAddr>) {
            Some(Ok(std::net::IpAddr::V6(v6))) if v6.to_ipv4_mapped().is_none() => {
                let net = std::net::Ipv6Addr::from(u128::from(v6) & !((1u128 << 64) - 1));
                format!("{net}/64")
            }
            Some(Ok(std::net::IpAddr::V6(v6))) => v6.to_ipv4_mapped().unwrap().to_string(),
            Some(Ok(ip)) => ip.to_string(),
            _ => "unknown".into(),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let none = TrustedProxies::default();
        let trusted = parts
            .extensions
            .get::<Arc<Auth>>()
            .map_or(&none, |a| &a.settings.trusted_proxies);
        let connect = parts.extensions.get::<ConnectInfo<SocketAddr>>();
        let ip = request_client_ip(&parts.headers, connect, trusted).map(|ip| ip.to_string());
        let header = |name| parts.headers.get(name).and_then(|v| v.to_str().ok());
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
             SELECT old.account_id, $2, old.expires_at, old.user_agent, old.ip, old.id
             FROM old JOIN accounts a ON a.id = old.account_id
             WHERE a.is_active AND a.anonymized_at IS NULL FOR SHARE OF a
             RETURNING id",
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

/// Rejects state-changing requests that lack `X-Requested-With: tikkit`, or
/// whose browser metadata (`Origin`, `Sec-Fetch-Site`) names another site than
/// `PUBLIC_ORIGIN`. Browsers cannot add custom headers cross-site without a
/// CORS preflight, which we never grant; the origin check is the second layer.
pub async fn csrf(Extension(auth): Extension<Arc<Auth>>, request: Request, next: Next) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let ok = request
        .headers()
        .get(CSRF_HEADER)
        .is_some_and(|v| v == CSRF_VALUE)
        && same_origin(request.headers(), &auth.settings.public_origin);
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

/// Records a bootstrap attempt after setup was completed (a replay). Rate
/// limited per client so the closed endpoint cannot be used to flood the log.
async fn setup_rejected(state: &AppState, auth: &Auth, client: &Client) -> Result<(), sqlx::Error> {
    if auth.limit_ip("setup_rejected", client).is_err() {
        return Ok(());
    }
    let metadata = json!({ "reason": "already_completed", "ip": client.ip });
    audit(&state.pool, "setup_rejected", None, None, metadata).await
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
        setup_rejected(&state, &auth, &client).await?;
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
        drop(tx);
        setup_rejected(&state, &auth, &client).await?;
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
    auth.check_attempt("login", &client, &normalized)?;
    let found: Option<(Uuid, String, bool)> = sqlx::query_as(
        "SELECT id, password_hash, is_active FROM accounts WHERE username_normalized = $1",
    )
    .bind(&normalized)
    .fetch_optional(&state.pool)
    .await?;
    // Verify against a dummy hash for unknown users so timing does not reveal
    // them, and for accounts whose stored hash is unusable on purpose (an
    // anonymized account's `!`), which would otherwise fail without any work.
    let (hash, usable) = verification_hash(found.as_ref().map(|f| f.1.as_str()));
    let password = body.password;
    let verified = blocking(move || verify_password(&password, &hash)).await? && usable;
    let failure = match &found {
        None => Some("unknown_username"),
        Some(_) if !verified => Some("wrong_password"),
        Some((_, _, false)) => Some("account_inactive"),
        Some(_) => None,
    };
    let subject = found.as_ref().map(|f| f.0);
    if let Some(reason) = failure {
        auth.record_failure("login", &client, &normalized);
        let metadata =
            // No typed username: for unknown users it may be a misplaced password.
            json!({ "reason": reason, "ip": client.ip });
        audit(&state.pool, "login_failed", None, subject, metadata).await?;
        return Err(ApiError::InvalidCredentials);
    }
    let account_id = subject.expect("checked above");
    auth.remember_success(&client, &normalized);
    let mut tx = state.pool.begin().await?;
    end_presented_session(&mut tx, &headers).await?;
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
    auth.check_attempt("password", &client, &username)?;
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
            auth.record_failure("password", &client, &username);
            ApiError::Validation(FieldErrors::from([(
                "current_password".into(),
                vec!["incorrect"],
            )]))
        })?;

    let mut tx = state.pool.begin().await?;
    lock_active_account(&mut tx, account_id).await?;
    sqlx::query("UPDATE accounts SET password_hash = $2 WHERE id = $1")
        .bind(account_id)
        .bind(&new_hash)
        .execute(&mut *tx)
        .await?;
    let revoked = revoke_all_sessions(&mut *tx, account_id).await?;
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

pub(crate) fn signed_in(auth: &Auth, token: &str, account: Account) -> Response {
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

/// The session this browser held before signing in ends here (no fixation).
pub(crate) async fn end_presented_session(
    tx: &mut sqlx::PgConnection,
    headers: &HeaderMap,
) -> Result<(), sqlx::Error> {
    let Some(previous) = cookie_token(headers) else {
        return Ok(());
    };
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sessions WHERE token_hash = $1")
        .bind(token_hash(&previous))
        .fetch_optional(&mut *tx)
        .await?;
    match id {
        Some(id) => revoke_session(tx, id).await,
        None => Ok(()),
    }
}

/// Revokes every live session of one account (never of others sharing its email).
pub(crate) async fn revoke_all_sessions<'c>(
    db: impl PgExecutor<'c>,
    account_id: Uuid,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "UPDATE sessions SET revoked_at = now() WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .execute(db)
    .await
    .map(|r| r.rows_affected())
}

/// The write protocol for an account's personal data (docs/retention.md).
/// Every transaction that stores something for an account (profile, password,
/// session, draft, upload, saved view) calls this first: it takes a share lock
/// on the account row and refuses unless the account is active. Anonymization
/// locks the same row `FOR UPDATE` before it deletes anything, so a request
/// that was authenticated while the account was still active either commits
/// before the erasure looks, or waits and is refused; it can never write
/// after it. (`lifecycle::lock_actor` is the same lock for ticket writes.)
pub async fn lock_active_account(db: &mut sqlx::PgConnection, id: Uuid) -> Result<(), ApiError> {
    let active: Option<bool> = sqlx::query_scalar(
        "SELECT is_active AND anonymized_at IS NULL FROM accounts WHERE id = $1 FOR SHARE",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    if active == Some(true) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// Always a brand-new random token: a cookie presented before login is never adopted.
/// Only for an active account (see [`lock_active_account`]): otherwise
/// `RowNotFound`.
pub(crate) async fn create_session(
    tx: &mut sqlx::PgConnection,
    auth: &Auth,
    account_id: Uuid,
    client: &Client,
) -> Result<String, sqlx::Error> {
    let token = new_token();
    sqlx::query(
        "INSERT INTO sessions (account_id, token_hash, expires_at, user_agent, ip)
         SELECT a.id, $2, now() + $3, $4, $5 FROM accounts a
         WHERE a.id = $1 AND a.is_active AND a.anonymized_at IS NULL FOR SHARE",
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

pub(crate) async fn load_account<'c>(
    db: impl PgExecutor<'c>,
    id: Uuid,
) -> Result<Account, ApiError> {
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

/// Appends an audit event ([`crate::audit::record`]) by an account, or by an
/// anonymous caller when `actor` is `None`; background jobs use
/// [`crate::audit::system`]. Metadata must never hold passwords, tokens, or secrets.
pub async fn audit<'c>(
    db: impl PgExecutor<'c>,
    event_type: &str,
    actor: Option<Uuid>,
    subject: Option<Uuid>,
    metadata: Value,
) -> Result<(), sqlx::Error> {
    let entry = crate::audit::Entry {
        action: event_type,
        who: actor.map_or(
            crate::audit::Who::Service(crate::audit::ANONYMOUS),
            crate::audit::Who::Account,
        ),
        subject,
        ticket: None,
        metadata,
    };
    crate::audit::record(db, entry).await
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

pub(crate) fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The machine identifier of a role or language, as stored in the database.
pub(crate) fn ident_str<T: Serialize>(value: T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        _ => unreachable!("identifiers serialize as strings"),
    }
}

/// Parses a stored machine identifier (role, language) into its enum.
pub(crate) fn ident<T: DeserializeOwned>(value: String) -> Result<T, ApiError> {
    serde_json::from_value(Value::String(value)).map_err(|_| ApiError::Internal)
}

pub(crate) fn unique_violation(error: sqlx::Error, field: &str) -> ApiError {
    match &error {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            ApiError::Validation(FieldErrors::from([(field.into(), vec!["taken"])]))
        }
        _ => error.into(),
    }
}

pub(crate) async fn blocking<T: Send + 'static>(
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

/// The hash a sign-in verifies against, and whether a match may count: the
/// stored one if it is a real password hash, otherwise the dummy (same cost,
/// never a success).
fn verification_hash(stored: Option<&str>) -> (String, bool) {
    match stored {
        Some(hash) if PasswordHash::new(hash).is_ok() => (hash.to_owned(), true),
        _ => (DUMMY_HASH.clone(), false),
    }
}

/// Usernames starting with this belong to anonymized accounts
/// (docs/retention.md). Nobody can register one, so the placeholder of an
/// erased account is always free; the database enforces the same
/// (`accounts_username_reserved`).
pub const RESERVED_USERNAME_PREFIX: &str = "anonymized-";

/// `normalized` is `lower(trim(username))`; returns a field error code.
pub fn check_username(normalized: &str) -> Result<(), &'static str> {
    match normalized.chars().count() {
        0..3 => Err("too_short"),
        65.. => Err("too_long"),
        _ if normalized.starts_with(RESERVED_USERNAME_PREFIX) => Err("reserved"),
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

pub(crate) fn valid_email(email: &str) -> bool {
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
        assert_eq!(check_username(&norm("Anonymized-1")), Err("reserved"));
        assert_eq!(check_username("anonymized-"), Err("reserved"));
        assert_eq!(check_username("anonymized"), Ok(()));
        assert_eq!(check_username("not-anonymized-1"), Ok(()));
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
    fn unusable_hashes_cost_a_full_verification_and_never_match() {
        let real = hash_password("correct horse battery");
        assert_eq!(verification_hash(Some(&real)), (real.clone(), true));
        for unusable in [Some("!"), Some(""), Some("not a hash"), None] {
            let (hash, usable) = verification_hash(unusable);
            assert!(!usable, "{unusable:?}");
            assert_eq!(
                hash, *DUMMY_HASH,
                "{unusable:?}: same Argon2 work as an unknown user"
            );
        }
    }

    #[test]
    fn rate_limits_are_per_key_fixed_windows() {
        let auth = Auth::new(Settings::default());
        let hit = |key: &str| auth.hit(key.to_owned(), 2);
        assert!(hit("ip:1").is_ok() && hit("ip:1").is_ok());
        assert!(matches!(hit("ip:1"), Err(ApiError::RateLimited(s)) if (1..=60).contains(&s)));
        assert!(hit("ip:2").is_ok());
    }

    fn client(ip: &str) -> Client {
        Client {
            ip: Some(ip.into()),
            user_agent: None,
        }
    }

    #[test]
    fn only_failures_count_and_each_network_has_its_own_budget() {
        let auth = Auth::new(Settings::default());
        let home = client("198.51.100.7");
        // Successful attempts never spend the budget.
        for _ in 0..50 {
            assert!(auth.check_attempt("login", &home, "alice").is_ok());
        }
        for _ in 0..LIMIT_PER_USERNAME_AND_IP {
            assert!(auth.check_attempt("login", &home, "alice").is_ok());
            auth.record_failure("login", &home, "alice");
        }
        assert!(matches!(
            auth.check_attempt("login", &home, "alice"),
            Err(ApiError::RateLimited(s)) if (1..=60).contains(&s)
        ));
        // The same username from another network is unaffected.
        assert!(
            auth.check_attempt("login", &client("203.0.113.9"), "alice")
                .is_ok()
        );
        // Per network across usernames.
        let spray = client("192.0.2.1");
        for i in 0..LIMIT_PER_IP {
            assert!(
                auth.check_attempt("login", &spray, &format!("u{i}"))
                    .is_ok()
            );
            auth.record_failure("login", &spray, &format!("u{i}"));
        }
        assert!(auth.check_attempt("login", &spray, "fresh").is_err());
    }

    #[test]
    fn ipv6_clients_are_limited_per_64() {
        let auth = Auth::new(Settings::default());
        for i in 0..LIMIT_PER_IP {
            let rotating = client(&format!("2001:db8:1:2::{:x}", i + 1));
            auth.record_failure("login", &rotating, &format!("u{i}"));
        }
        let same_64 = client("2001:db8:1:2:ffff::1");
        assert!(auth.check_attempt("login", &same_64, "another").is_err());
        assert!(
            auth.check_attempt("login", &client("2001:db8:1:3::1"), "another")
                .is_ok()
        );
        assert_eq!(
            client("2001:db8:1:2:aaaa::1").network(),
            "2001:db8:1:2::/64"
        );
        assert_eq!(client("::ffff:198.51.100.7").network(), "198.51.100.7");
        assert_eq!(
            Client {
                ip: None,
                user_agent: None
            }
            .network(),
            "unknown"
        );
    }

    #[test]
    fn a_distributed_lockout_spares_the_owners_own_network() {
        let auth = Auth::new(Settings::default());
        let home = client("198.51.100.7");
        auth.remember_success(&home, "alice");
        // Many networks, each within its own budgets, exhaust the username ceiling.
        for i in 0..LIMIT_PER_USERNAME {
            let attacker = client(&format!("203.0.113.{}", i % 200 + 1));
            auth.record_failure("login", &attacker, "alice");
        }
        assert!(
            auth.check_attempt("login", &client("192.0.2.200"), "alice")
                .is_err()
        );
        assert!(
            auth.check_attempt("login", &home, "alice").is_ok(),
            "known-good network"
        );
        // The ceiling applies to unknown usernames exactly like known ones.
        for i in 0..LIMIT_PER_USERNAME {
            let attacker = client(&format!("203.0.113.{}", i % 200 + 1));
            auth.record_failure("login", &attacker, "nobody");
        }
        assert!(
            auth.check_attempt("login", &client("192.0.2.200"), "nobody")
                .is_err()
        );
    }

    #[test]
    fn a_full_table_evicts_the_oldest_window_instead_of_refusing() {
        let limiter = Limiter::default();
        for i in 0..LIMIT_MAX_KEYS {
            limiter.hit(format!("k{i}"), 5).unwrap();
        }
        assert!(limiter.hit("new client".into(), 5).is_ok());
        let table = limiter.0.lock().unwrap();
        assert_eq!(table.len(), LIMIT_MAX_KEYS);
        assert!(table.contains_key("new client"));
    }
}
