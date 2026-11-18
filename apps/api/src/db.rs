use std::time::Duration;

use sqlx::{
    Error, PgPool,
    migrate::{MigrateError, Migrator},
    postgres::PgPoolOptions,
};

/// Migrations embedded from `apps/api/migrations` at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> Result<PgPool, Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await
}

pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR.run(pool).await
}

/// Stable log fields. Driver errors can contain SQL, data, or credentials.
pub fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::Configuration(_) => "configuration",
        Error::InvalidArgument(_) => "invalid_argument",
        Error::Database(_) => "database",
        Error::Io(_) => "io",
        Error::Tls(_) => "tls",
        Error::Protocol(_) => "protocol",
        Error::PoolTimedOut => "pool_timed_out",
        Error::PoolClosed => "pool_closed",
        Error::RowNotFound => "row_not_found",
        Error::WorkerCrashed => "worker_crashed",
        Error::ColumnDecode { .. } | Error::Decode(_) => "decode",
        Error::Encode(_) => "encode",
        _ => "other",
    }
}

pub fn log_migration_failure(error: &MigrateError) {
    let (kind, version, cause) = match error {
        MigrateError::Execute(error) => ("execute", None, Some(error)),
        MigrateError::ExecuteMigration(error, version) => ("execute", Some(*version), Some(error)),
        MigrateError::Source(_) => ("source", None, None),
        MigrateError::VersionMissing(version) => ("version_missing", Some(*version), None),
        MigrateError::VersionMismatch(version) => ("version_mismatch", Some(*version), None),
        MigrateError::VersionNotPresent(version) => ("version_not_present", Some(*version), None),
        MigrateError::VersionTooOld(version, _) => ("version_too_old", Some(*version), None),
        MigrateError::VersionTooNew(version, _) => ("version_too_new", Some(*version), None),
        MigrateError::Dirty(version) => ("dirty", Some(*version), None),
        _ => ("other", None, None),
    };
    let sqlstate = cause
        .and_then(Error::as_database_error)
        .and_then(|db| db.code());
    tracing::error!(
        error_kind = kind,
        migration_version = version,
        database_error_kind = cause.map(error_kind),
        sqlstate = sqlstate.as_deref(),
        "database migration failed"
    );
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
