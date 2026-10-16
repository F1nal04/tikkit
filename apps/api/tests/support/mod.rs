//! Shared helpers for integration tests in `apps/api/tests/*.rs`; include with `mod support;`.
//! See docs/testing.md.
#![allow(dead_code)] // each test binary uses a different subset

use std::str::FromStr;

use axum::Router;
use sqlx::{
    Connection, Executor, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tikkit_api::{AppState, auth::Settings, config::Config, db};
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

    /// An account created by accepting an invitation. Plain insert until the
    /// invitations table exists; #10 extends this to create and accept one.
    pub async fn invited_account(pool: &PgPool, role: Role, username: &str) -> Uuid {
        account(pool, role, username).await
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
