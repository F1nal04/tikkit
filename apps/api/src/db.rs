//! PostgreSQL access (issue #6): pool creation, embedded migrations, and the
//! schema compatibility check shared by startup and readiness.

use std::{collections::HashMap, fmt, time::Duration};

use sqlx::{
    Connection, PgConnection, PgPool,
    migrate::{MigrateError, Migrator},
    postgres::PgPoolOptions,
};

use crate::config::Config;

/// All migrations in `apps/api/migrations`, embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// How long a request may wait for a pooled connection.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_INITIAL_DELAY: Duration = Duration::from_millis(250);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(5);

/// Database failures that prevent startup or readiness. Messages never
/// contain credentials.
#[derive(Debug)]
pub enum DbError {
    Unavailable {
        target: String,
        attempts: u32,
        waited: Duration,
        detail: String,
    },
    Schema(SchemaProblem),
    Migration(String),
    Query(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable {
                target,
                attempts,
                waited,
                detail,
            } => write!(
                f,
                "could not connect to PostgreSQL at {target} after {attempts} attempt(s) over {}s: {detail}. \
                 Check that the database is running and DATABASE_URL is correct \
                 (for local development: docker compose -f compose.dev.yml up -d)",
                waited.as_secs()
            ),
            Self::Schema(problem) => write!(f, "{problem}"),
            Self::Migration(detail) => write!(f, "applying database migrations failed: {detail}"),
            Self::Query(detail) => write!(f, "database query failed: {detail}"),
        }
    }
}

impl std::error::Error for DbError {}

/// Pool settings shared by eager and lazy pools.
pub fn pool_options(config: &Config) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .min_connections(0)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(Some(Duration::from_secs(600)))
        .test_before_acquire(true)
}

/// A pool that connects on first use. Never fails; useful for tests and for
/// serving readiness while the database is down.
pub fn connect_lazy(config: &Config) -> PgPool {
    pool_options(config).connect_lazy_with(config.database_url.connect_options())
}

/// Connects at startup. Transient failures (connection refused, timeouts)
/// are retried with exponential backoff until `DATABASE_STARTUP_TIMEOUT_SECS`
/// has elapsed; errors reported by the server itself (wrong password, missing
/// database) fail immediately.
pub async fn connect_with_retry(config: &Config) -> Result<PgPool, DbError> {
    let started = tokio::time::Instant::now();
    let deadline = started + config.database_startup_timeout;
    let target = describe_target(config);
    let options = config.database_url.connect_options();
    let mut delay = RETRY_INITIAL_DELAY;
    let mut attempts = 0;
    loop {
        attempts += 1;
        // A single probe connection surfaces the real error; the pool would
        // only report "timed out".
        let probe = tokio::time::timeout(ACQUIRE_TIMEOUT, PgConnection::connect_with(&options))
            .await
            .unwrap_or_else(|_| {
                Err(sqlx::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connection attempt timed out",
                )))
            });
        let error = match probe {
            Ok(connection) => {
                let _ = connection.close().await;
                match pool_options(config).connect_with(options.clone()).await {
                    Ok(pool) => {
                        if attempts > 1 {
                            tracing::info!(attempts, "connected to PostgreSQL");
                        }
                        return Ok(pool);
                    }
                    Err(error) => error,
                }
            }
            Err(error) => error,
        };
        let detail = config.database_url.scrub(&error.to_string());
        let now = tokio::time::Instant::now();
        let permanent = matches!(
            error,
            sqlx::Error::Database(_) | sqlx::Error::Configuration(_)
        );
        if permanent || now + delay > deadline {
            return Err(DbError::Unavailable {
                target,
                attempts,
                waited: now - started,
                detail,
            });
        }
        tracing::warn!(
            attempt = attempts,
            retry_in_ms = delay.as_millis() as u64,
            error = %detail,
            "PostgreSQL unavailable, retrying"
        );
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX_DELAY);
    }
}

/// `host:port/database` without credentials, for messages.
pub fn describe_target(config: &Config) -> String {
    let options = config.database_url.connect_options();
    format!(
        "{}:{}/{}",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or("(default)")
    )
}

/// Comparison between applied migrations and the ones built into this binary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaStatus {
    pub applied: Vec<i64>,
    pub pending: Vec<i64>,
    /// Applied in the database but unknown to this binary (newer schema).
    pub unknown: Vec<i64>,
    /// Applied, but the file contents differ from this binary's.
    pub checksum_mismatch: Vec<i64>,
    /// Recorded as failed (`success = false`).
    pub failed: Vec<i64>,
}

/// Why the schema cannot be used by this binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaProblem {
    Pending(Vec<i64>),
    Incompatible(String),
}

impl fmt::Display for SchemaProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending(versions) => write!(
                f,
                "the database schema is behind this release: {} migration(s) pending ({}). \
                 Run `tikkit-api migrate` or start with DATABASE_AUTO_MIGRATE=true",
                versions.len(),
                join(versions)
            ),
            Self::Incompatible(reason) => write!(f, "incompatible database schema: {reason}"),
        }
    }
}

fn join(versions: &[i64]) -> String {
    versions
        .iter()
        .map(|v| format!("{v:04}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl SchemaStatus {
    /// Problems that no migration run of this binary can fix.
    pub fn incompatibility(&self) -> Option<SchemaProblem> {
        let mut reasons = Vec::new();
        if !self.unknown.is_empty() {
            reasons.push(format!(
                "the database has migration(s) {} that this release does not know; it was migrated by a newer Tikkit version. Run that version or restore a matching backup",
                join(&self.unknown)
            ));
        }
        if !self.checksum_mismatch.is_empty() {
            reasons.push(format!(
                "applied migration(s) {} differ from the files built into this release; released migrations must never be edited",
                join(&self.checksum_mismatch)
            ));
        }
        if !self.failed.is_empty() {
            reasons.push(format!(
                "migration(s) {} previously failed part-way; inspect the database and restore from backup",
                join(&self.failed)
            ));
        }
        (!reasons.is_empty()).then(|| SchemaProblem::Incompatible(reasons.join("; ")))
    }

    /// `None` when the schema exactly matches this binary.
    pub fn problem(&self) -> Option<SchemaProblem> {
        self.incompatibility().or_else(|| {
            (!self.pending.is_empty()).then(|| SchemaProblem::Pending(self.pending.clone()))
        })
    }

    pub fn is_current(&self) -> bool {
        self.problem().is_none()
    }
}

/// Reads `_sqlx_migrations` and compares it with [`MIGRATOR`].
pub async fn schema_status(pool: &PgPool) -> Result<SchemaStatus, sqlx::Error> {
    schema_status_for(pool, &MIGRATOR).await
}

pub async fn schema_status_for(
    pool: &PgPool,
    migrator: &Migrator,
) -> Result<SchemaStatus, sqlx::Error> {
    let table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    let applied_rows: Vec<(i64, Vec<u8>, bool)> = if table_exists {
        sqlx::query_as("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?
    } else {
        Vec::new()
    };

    let known: HashMap<i64, &[u8]> = migrator
        .iter()
        .filter(|m| m.migration_type.is_up_migration())
        .map(|m| (m.version, m.checksum.as_ref()))
        .collect();

    let mut status = SchemaStatus::default();
    for (version, checksum, success) in &applied_rows {
        status.applied.push(*version);
        if !success {
            status.failed.push(*version);
        }
        match known.get(version) {
            None => status.unknown.push(*version),
            Some(expected) if *expected != checksum.as_slice() => {
                status.checksum_mismatch.push(*version)
            }
            Some(_) => {}
        }
    }
    let mut pending: Vec<i64> = known
        .keys()
        .copied()
        .filter(|v| !applied_rows.iter().any(|(applied, _, _)| applied == v))
        .collect();
    pending.sort_unstable();
    status.pending = pending;
    Ok(status)
}

/// Startup schema handling: refuse incompatible schemas, apply pending
/// migrations when `auto_migrate` is set, otherwise fail with instructions.
pub async fn prepare_schema(pool: &PgPool, auto_migrate: bool) -> Result<SchemaStatus, DbError> {
    let status = schema_status(pool)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
    if let Some(problem) = status.incompatibility() {
        return Err(DbError::Schema(problem));
    }
    if status.pending.is_empty() {
        return Ok(status);
    }
    if !auto_migrate {
        return Err(DbError::Schema(SchemaProblem::Pending(status.pending)));
    }
    migrate(pool).await
}

/// Applies all pending migrations (guarded by a PostgreSQL advisory lock, so
/// concurrent starters are safe) and returns the resulting status.
pub async fn migrate(pool: &PgPool) -> Result<SchemaStatus, DbError> {
    let before = schema_status(pool)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
    if let Some(problem) = before.incompatibility() {
        return Err(DbError::Schema(problem));
    }
    MIGRATOR.run(pool).await.map_err(|error| match error {
        MigrateError::VersionMissing(v) => DbError::Schema(SchemaProblem::Incompatible(format!(
            "migration {v:04} is applied but unknown to this release"
        ))),
        other => DbError::Migration(other.to_string()),
    })?;
    for version in &before.pending {
        tracing::info!(version, "applied database migration");
    }
    schema_status(pool)
        .await
        .map_err(|e| DbError::Query(e.to_string()))
}
