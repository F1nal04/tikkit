//! Shared helpers for integration tests in `apps/api/tests/*.rs`; include with `mod support;`.
//! See docs/testing.md.
#![allow(dead_code)] // each test binary uses a different subset

use std::str::FromStr;

use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::{
    Connection, Executor, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tikkit_api::{AppState, auth::Settings, config::Config, db};
use tower::ServiceExt;
use uuid::Uuid;

/// A freshly created and migrated database, dropped when this value is dropped
/// (also on panic). Set TEST_DATABASE_URL (or DATABASE_URL) to a role that may
/// create databases; without it `new` returns `None` and the test should return early.
pub struct TestDb {
    pub pool: PgPool,
    pub name: String,
    admin: PgConnectOptions,
}

impl TestDb {
    pub async fn new() -> Option<Self> {
        dotenvy::dotenv().ok();
        let Ok(url) = std::env::var("TEST_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
        else {
            eprintln!("skipping: set TEST_DATABASE_URL to run PostgreSQL integration tests");
            return None;
        };
        let admin = PgConnectOptions::from_str(&url).expect("valid TEST_DATABASE_URL");
        let name = format!("tikkit_test_{}", Uuid::new_v4().simple());
        let mut conn = PgConnection::connect_with(&admin).await.unwrap();
        conn.execute(format!("CREATE DATABASE {name}").as_str())
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        db::migrate(&pool).await.unwrap();
        Some(Self { pool, name, admin })
    }

    /// Whether a database with this name exists on the test server.
    pub async fn exists(&self, name: &str) -> bool {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

impl Drop for TestDb {
    // Runs even when the test panics. FORCE terminates the pool's open sessions.
    fn drop(&mut self) {
        let (admin, name) = (self.admin.clone(), self.name.clone());
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let mut conn = PgConnection::connect_with(&admin).await.unwrap();
                    conn.execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
                        .await
                        .unwrap();
                })
        })
        .join()
        .unwrap();
    }
}

/// Valid configuration with fixed, non-secret test values.
pub fn test_config() -> Config {
    Config::from_vars(|name| {
        let value = match name {
            "DATABASE_URL" => "postgres://tikkit:tikkit@127.0.0.1:5432/tikkit",
            "PUBLIC_ORIGIN" => "http://localhost:3000",
            "SESSION_SECRET" | "ENCRYPTION_KEY" => {
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
            }
            _ => return None,
        };
        Some(value.to_owned())
    })
    .unwrap()
}

/// The real application router on `pool` with development auth settings
/// (no `Secure` cookie); drive it with `tower::ServiceExt::oneshot`. Clones share
/// one rate limiter. State-changing requests need `x-requested-with: tikkit`.
pub fn test_app(pool: &PgPool) -> Router {
    test_app_with(pool, Settings::default())
}

pub fn test_app_with(pool: &PgPool, settings: Settings) -> Router {
    tikkit_api::app_with(AppState { pool: pool.clone() }, settings)
}

/// One HTTP exchange with the app, decoded for assertions.
pub struct Reply {
    pub status: StatusCode,
    pub cookie: Option<String>,
    pub retry_after: Option<String>,
    pub body: Value,
}

impl Reply {
    pub fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }
    /// `name=value` part of the session cookie.
    pub fn session(&self) -> String {
        let cookie = self.cookie.as_deref().expect("set-cookie");
        cookie.split(';').next().unwrap().to_owned()
    }
}

pub async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Reply {
    call_from(app, method, path, body, cookie, [127, 0, 0, 1]).await
}

pub async fn call_from(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    ip: [u8; 4],
) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("x-requested-with", "tikkit");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let mut request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, 1234))));
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let header = |name| {
        parts
            .headers
            .get(name)
            .map(|v: &axum::http::HeaderValue| v.to_str().unwrap().to_owned())
    };
    let (cookie, retry_after) = (header("set-cookie"), header("retry-after"));
    let status = parts.status;
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    Reply {
        status,
        cookie,
        retry_after,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

/// Signs in through `POST /api/auth/login`.
pub async fn login(app: &Router, username: &str, password: &str) -> Reply {
    let body = json!({ "username": username, "password": password });
    call(app, "POST", "/api/auth/login", Some(body), None).await
}

pub mod fixtures {
    use sqlx::{PgExecutor, PgPool};
    pub use tikkit_api::api::Role;
    use uuid::Uuid;

    /// Password of every fixture account; sign in with `POST /api/auth/login`.
    pub const PASSWORD: &str = "correct horse battery staple!";
    /// Argon2id hash of [`PASSWORD`], precomputed so fixtures stay fast.
    pub const PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$nRhnZ6rgmgEaPuxLd8YUoA$zty/+hkq7huelI0TWA7Npoi2D4sbeM0NELyGfaCDmS4";

    /// An active account. Every fixture account shares one email on purpose:
    /// email is contact info only and must never identify an account.
    pub async fn account(pool: &PgPool, role: Role, username: &str) -> Uuid {
        let role = serde_json::to_value(role).unwrap();
        sqlx::query_scalar(
            "INSERT INTO accounts (username, display_name, email, password_hash, role)
             VALUES ($1, $1, 'shared@example.com', $2, $3) RETURNING id",
        )
        .bind(username)
        .bind(PASSWORD_HASH)
        .bind(role.as_str())
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// The first-setup Admin: inserts it and closes setup, as `POST /api/setup` would.
    pub async fn bootstrap_admin(pool: &PgPool) -> Uuid {
        let id = account(pool, Role::Admin, "admin").await;
        sqlx::query("UPDATE organization SET setup_completed_at = now()")
            .execute(pool)
            .await
            .unwrap();
        id
    }

    /// An account created through the real invitation flow: an invitation for
    /// `role`, issued by an existing Admin (e.g. [`bootstrap_admin`]), accepted
    /// with `POST /api/invitations/accept`. Shares the fixture email and password.
    pub async fn invited_account(pool: &PgPool, role: Role, username: &str) -> Uuid {
        let token = Uuid::new_v4().to_string();
        let role = serde_json::to_value(role).unwrap();
        let issued = sqlx::query(
            "INSERT INTO invitations (token_hash, role, created_by, expires_at)
             SELECT sha256(convert_to($1, 'UTF8')), $2, id, now() + interval '1 day'
             FROM accounts WHERE role = 'admin' AND is_active ORDER BY created_at LIMIT 1",
        )
        .bind(&token)
        .bind(role.as_str())
        .execute(pool)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!(issued, 1, "invited_account needs an active Admin first");
        let body = serde_json::json!({
            "token": token, "username": username, "email": "shared@example.com",
            "password": PASSWORD, "language": "en",
        });
        let app = super::test_app(pool);
        let r = super::call(&app, "POST", "/api/invitations/accept", Some(body), None).await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["id"].as_str().unwrap().parse().unwrap()
    }

    /// A team with `lead` (an active `TeamLead` account) as member and lead, in one
    /// transaction. `lead_role`/`lead_active` use their column defaults.
    pub async fn team(pool: &PgPool, name: &str, lead: Uuid) -> Result<Uuid, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO teams (name, lead_account_id) VALUES ($1, $2) RETURNING id",
        )
        .bind(name)
        .bind(lead)
        .fetch_one(&mut *tx)
        .await?;
        member(&mut *tx, id, lead).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Adds `account` to `team`. Accepts a pool, connection, or transaction.
    pub async fn member(
        db: impl PgExecutor<'_>,
        team: Uuid,
        account: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO team_memberships (account_id, role, team_id)
             SELECT id, role, $2 FROM accounts WHERE id = $1",
        )
        .bind(account)
        .bind(team)
        .execute(db)
        .await
        .map(drop)
    }
}
