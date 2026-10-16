use std::time::Duration;

use anyhow::Context;
use sqlx::{PgPool, migrate::Migrator, postgres::PgPoolOptions};

/// Migrations embedded from `apps/api/migrations` at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    // The error context never includes the URL, which may contain a password.
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await
        .context("cannot connect to PostgreSQL; check DATABASE_URL and that the server is running")
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await.context(
        "database migration failed; the schema may be newer than this build, \
         partially applied, or an applied migration was modified",
    )
}
