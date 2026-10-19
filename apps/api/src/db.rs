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

/// Compares applied migrations with the embedded ones: `ok`, `pending`
/// (database is behind this build) or `incompatible` (unknown, modified, or failed).
pub async fn migration_status(pool: &PgPool) -> Result<&'static str, sqlx::Error> {
    let applied: Vec<(i64, Vec<u8>, bool)> =
        sqlx::query_as("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    let expected = MIGRATOR.iter().map(|m| (m.version, &*m.checksum));
    let mut applied_iter = applied.iter();
    for (version, checksum) in expected {
        match applied_iter.next() {
            Some((v, c, true)) if *v == version && c.as_slice() == checksum => {}
            None => return Ok("pending"),
            Some(_) => return Ok("incompatible"),
        }
    }
    Ok(if applied_iter.next().is_some() {
        "incompatible"
    } else {
        "ok"
    })
}
