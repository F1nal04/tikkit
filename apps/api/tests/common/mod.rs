//! Shared integration-test harness (issue #5, API part).
//!
//! Every [`TestDb`] creates its own uniquely named PostgreSQL database on the
//! server named by `TEST_DATABASE_URL`, applies all migrations, and drops the
//! database again (`WITH (FORCE)`) when it goes out of scope, including when
//! the test panics. Tests therefore never share data, can run in parallel, and
//! do not depend on execution order.
//!
//! If the server is unreachable the test fails with instructions instead of
//! being skipped.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::str::FromStr;

use axum::Router;
use sqlx::{
    Connection, Executor, PgConnection, PgPool, Postgres,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tikkit_api::{
    AppState, Config,
    auth::{AuthSettings, PasswordHasher, username},
    db::MIGRATOR,
};
use uuid::Uuid;

pub const DEFAULT_TEST_DATABASE_URL: &str = "postgres://tikkit:tikkit@127.0.0.1:5432/postgres";

/// A syntactically valid argon2id hash that matches no password. Accounts
/// created with [`create_account`] use it and therefore cannot log in; use
/// [`create_account_with_password`] for accounts that sign in.
pub const DUMMY_PASSWORD_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$dGVzdC1zYWx0$dGVzdC1oYXNoLW5vdC1yZWFs";

/// Name prefix of every disposable database. Leftovers from killed runs can
/// be removed with `SELECT 'DROP DATABASE "' || datname || '" WITH (FORCE);'
/// FROM pg_database WHERE datname LIKE 'tikkit_test_%'`.
pub const TEST_DATABASE_PREFIX: &str = "tikkit_test_";

fn server_options() -> (PgConnectOptions, String) {
    let url =
        std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| DEFAULT_TEST_DATABASE_URL.to_owned());
    let options = PgConnectOptions::from_str(&url)
        .unwrap_or_else(|e| panic!("TEST_DATABASE_URL is not a valid PostgreSQL URL: {e}"));
    let display = format!(
        "{}:{}/{}",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or("(default)")
    );
    (options, display)
}

pub struct TestDb {
    pub pool: PgPool,
    name: String,
    server: PgConnectOptions,
    cleaned_up: bool,
}

impl TestDb {
    /// Creates and migrates a fresh database.
    pub async fn new() -> Self {
        let db = Self::empty().await;
        MIGRATOR
            .run(&db.pool)
            .await
            .expect("migrations apply to an empty database");
        db
    }

    /// Creates a fresh database without running migrations.
    pub async fn empty() -> Self {
        let (server, display) = server_options();
        let mut admin = PgConnection::connect_with(&server)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "cannot reach the test PostgreSQL server at {display}: {e}\n\
                 Start PostgreSQL 16 (e.g. `docker compose -f compose.dev.yml up -d`) or set \
                 TEST_DATABASE_URL to a server where this user may CREATE DATABASE."
                )
            });
        let name = format!("{TEST_DATABASE_PREFIX}{}", Uuid::new_v4().simple());
        admin
            .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
            .await
            .unwrap_or_else(|e| panic!("cannot create test database {name}: {e}"));
        let _ = admin.close().await;

        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(server.clone().database(&name))
            .await
            .expect("connect to the new test database");
        Self {
            pool,
            name,
            server,
            cleaned_up: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Connection options for this database (e.g. to open a second pool that
    /// simulates an API restart).
    pub fn connect_options(&self) -> PgConnectOptions {
        self.server.clone().database(&self.name)
    }

    /// Connection options for the maintenance database on the test server.
    pub fn server_connect_options(&self) -> PgConnectOptions {
        self.server.clone()
    }

    /// A `postgres://` URL for this database, for building a [`Config`].
    pub fn url(&self) -> String {
        let base = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| DEFAULT_TEST_DATABASE_URL.to_owned());
        let mut url = url::Url::parse(&base).expect("TEST_DATABASE_URL is a URL");
        url.set_path(&format!("/{}", self.name));
        url.to_string()
    }

    /// Development configuration pointing at this database.
    pub fn config(&self) -> Config {
        config_with_database_url(&self.url())
    }

    /// Development configuration with extra/overridden variables, e.g.
    /// `&[("TIKKIT_ENV", "production"), ("PUBLIC_ORIGIN", "https://…")]`.
    pub fn config_with(&self, overrides: &[(&str, &str)]) -> Config {
        config_with_overrides(&self.url(), overrides)
    }

    /// Production configuration (https origin [`PRODUCTION_ORIGIN`]).
    pub fn production_config(&self) -> Config {
        self.config_with(&[
            ("TIKKIT_ENV", "production"),
            ("PUBLIC_ORIGIN", PRODUCTION_ORIGIN),
            (
                "TIKKIT_ENCRYPTION_KEY",
                "CsjNdhpLwWqa7gYbpMji99S5DQC2Y7ttfW5aisLe/Nc=",
            ),
        ])
    }

    /// State with the development config and fast (test-only) password
    /// hashing; otherwise the production auth policy.
    pub fn state(&self) -> AppState {
        self.state_with(self.config(), AuthSettings::insecure_fast_for_tests())
    }

    pub fn state_with(&self, config: Config, settings: AuthSettings) -> AppState {
        AppState::new(self.pool.clone(), config).with_auth_settings(settings)
    }

    pub fn app(&self) -> Router {
        tikkit_api::app(self.state())
    }

    /// Drops the database now (also happens automatically on drop).
    pub async fn cleanup(mut self) {
        self.pool.close().await;
        drop_database(self.server.clone(), self.name.clone()).await;
        self.cleaned_up = true;
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if self.cleaned_up {
            return;
        }
        let server = self.server.clone();
        let name = self.name.clone();
        // Drop cannot be async and may run inside (or outside) a Tokio
        // runtime, so clean up on a dedicated thread with its own runtime.
        let handle = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime")
                .block_on(drop_database(server, name));
        });
        let _ = handle.join();
    }
}

async fn drop_database(server: PgConnectOptions, name: String) {
    match PgConnection::connect_with(&server).await {
        Ok(mut admin) => {
            if let Err(e) = admin
                .execute(format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#).as_str())
                .await
            {
                eprintln!("warning: failed to drop test database {name}: {e}");
            }
            let _ = admin.close().await;
        }
        Err(e) => eprintln!("warning: cannot connect to drop test database {name}: {e}"),
    }
}

pub fn config_with_database_url(database_url: &str) -> Config {
    config_with_overrides(database_url, &[])
}

/// Public origin of the development test configuration.
pub const DEV_ORIGIN: &str = "http://localhost:3000";
/// Public origin of [`TestDb::production_config`].
pub const PRODUCTION_ORIGIN: &str = "https://help.example.test";

pub fn config_with_overrides(database_url: &str, overrides: &[(&str, &str)]) -> Config {
    let mut vars: std::collections::HashMap<String, String> = [
        ("DATABASE_URL", database_url),
        ("TIKKIT_ENV", "development"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    for (k, v) in overrides {
        vars.insert((*k).to_owned(), (*v).to_owned());
    }
    Config::from_source(move |name| vars.get(name).cloned()).expect("valid test configuration")
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Operator,
    Specialist,
    TeamLead,
    Admin,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::User,
        Role::Operator,
        Role::Specialist,
        Role::TeamLead,
        Role::Admin,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Operator => "operator",
            Role::Specialist => "specialist",
            Role::TeamLead => "team_lead",
            Role::Admin => "admin",
        }
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub username: String,
    pub username_normalized: String,
    pub email: String,
    pub role: String,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Team {
    pub id: Uuid,
    pub name: String,
}

/// Inserts an active account that cannot log in (see
/// [`DUMMY_PASSWORD_HASH`]). `username` is stored as given and normalized
/// like the API does (trim, NFKC, lowercase).
///
/// Invited-account fixtures: once #10 adds the invitations table, build
/// `create_invited_account` on top of this (create the invitation, then the
/// account, then consume the invitation in one transaction).
pub async fn create_account<'e, E>(executor: E, role: Role, username: &str, email: &str) -> Account
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    insert_account(executor, role, username, email, DUMMY_PASSWORD_HASH).await
}

/// Like [`create_account`], but with a real argon2id hash of `password`
/// (cheap test parameters), so the account can log in through the API.
pub async fn create_account_with_password<'e, E>(
    executor: E,
    role: Role,
    username: &str,
    email: &str,
    password: &str,
) -> Account
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let hash = PasswordHasher::new(AuthSettings::insecure_fast_for_tests().hash_params)
        .hash(password)
        .expect("hash test password");
    insert_account(executor, role, username, email, &hash).await
}

async fn insert_account<'e, E>(
    executor: E,
    role: Role,
    username: &str,
    email: &str,
    password_hash: &str,
) -> Account
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    sqlx::query_as(
        "INSERT INTO accounts (id, username, username_normalized, email, display_name, role, \
                               specialization, password_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, username, username_normalized, email, role",
    )
    .bind(Uuid::now_v7())
    .bind(username.trim())
    .bind(username::normalize(username))
    .bind(email)
    .bind(format!("Test {username}"))
    .bind(role.as_str())
    .bind((role == Role::Specialist).then_some("Networking"))
    .bind(password_hash)
    .fetch_one(executor)
    .await
    .unwrap_or_else(|e| panic!("create_account({username}): {e}"))
}

/// Creates an active team and appoints `lead` (a team_lead account) in one
/// transaction, as the schema requires.
pub async fn create_team_with_lead(pool: &PgPool, name: &str, lead: &Account) -> Team {
    let mut tx = pool.begin().await.unwrap();
    let team: Team =
        sqlx::query_as("INSERT INTO teams (id, name) VALUES ($1, $2) RETURNING id, name")
            .bind(Uuid::now_v7())
            .bind(name)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    add_membership(&mut *tx, team.id, lead.id, true)
        .await
        .unwrap();
    tx.commit()
        .await
        .unwrap_or_else(|e| panic!("create_team_with_lead({name}): {e}"));
    team
}

pub async fn add_membership<'e, E>(
    executor: E,
    team_id: Uuid,
    account_id: Uuid,
    is_lead: bool,
) -> Result<Uuid, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar(
        "INSERT INTO team_memberships (id, team_id, account_id, is_lead) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(team_id)
    .bind(account_id)
    .bind(is_lead)
    .fetch_one(executor)
    .await
}

/// One account per role. The Team Lead leads `team`; the Operator and
/// Specialist are members of it; User and Admin have no team.
pub struct RoleFixtures {
    pub user: Account,
    pub operator: Account,
    pub specialist: Account,
    pub team_lead: Account,
    pub admin: Account,
    pub team: Team,
}

impl RoleFixtures {
    pub async fn create(pool: &PgPool) -> Self {
        let user = create_account(pool, Role::User, "user", "user@example.test").await;
        let operator =
            create_account(pool, Role::Operator, "operator", "operator@example.test").await;
        let specialist = create_account(
            pool,
            Role::Specialist,
            "specialist",
            "specialist@example.test",
        )
        .await;
        let team_lead =
            create_account(pool, Role::TeamLead, "team.lead", "lead@example.test").await;
        let admin = create_account(pool, Role::Admin, "admin", "admin@example.test").await;
        let team = create_team_with_lead(pool, "Service Desk", &team_lead).await;
        add_membership(pool, team.id, operator.id, false)
            .await
            .unwrap();
        add_membership(pool, team.id, specialist.id, false)
            .await
            .unwrap();
        Self {
            user,
            operator,
            specialist,
            team_lead,
            admin,
            team,
        }
    }

    pub fn by_role(&self, role: Role) -> &Account {
        match role {
            Role::User => &self.user,
            Role::Operator => &self.operator,
            Role::Specialist => &self.specialist,
            Role::TeamLead => &self.team_lead,
            Role::Admin => &self.admin,
        }
    }
}

/// "First setup" fixture: creates the initial Admin and marks organization
/// bootstrap as completed, exactly as first setup will (#9).
pub async fn complete_first_setup(pool: &PgPool) -> Account {
    let mut tx = pool.begin().await.unwrap();
    let admin = create_account(&mut *tx, Role::Admin, "first.admin", "it@example.test").await;
    let updated = sqlx::query(
        "UPDATE organization_settings SET bootstrap_completed_at = now(), bootstrap_admin_account_id = $1 \
         WHERE bootstrap_completed_at IS NULL",
    )
    .bind(admin.id)
    .execute(&mut *tx)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(updated, 1, "first setup already completed");
    tx.commit().await.unwrap();
    admin
}

/// Inserts a category and a priority, returning their ids.
pub async fn create_category_and_priority(pool: &PgPool) -> (Uuid, Uuid) {
    let suffix = Uuid::new_v4().simple().to_string();
    let category: Uuid =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ($1) RETURNING id")
            .bind(format!("Hardware {suffix}"))
            .fetch_one(pool)
            .await
            .unwrap();
    let priority: Uuid = sqlx::query_scalar(
        "INSERT INTO priorities (name, sort_order) VALUES ($1, 10) RETURNING id",
    )
    .bind(format!("Normal {suffix}"))
    .fetch_one(pool)
    .await
    .unwrap();
    (category, priority)
}

/// Inserts a submitted ticket in the general queue and returns (id, reference).
pub async fn create_queued_ticket(pool: &PgPool, requester: &Account) -> (Uuid, i64) {
    let (category, priority) = create_category_and_priority(pool).await;
    sqlx::query_as(
        "INSERT INTO tickets (id, requester_account_id, category_id, subject, description, \
                              requested_priority_id, priority_id, intake_snapshot, status, submitted_at) \
         VALUES ($1, $2, $3, 'Printer offline', 'The printer on floor 2 is offline.', $4, $4, \
                 jsonb_build_object('category', 'Hardware', 'priority', 'Normal'), 'queued', now()) \
         RETURNING id, reference",
    )
    .bind(Uuid::now_v7())
    .bind(requester.id)
    .bind(category)
    .bind(priority)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The PostgreSQL constraint name carried by a database error, if any.
pub fn constraint_of(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(|e| e.constraint().map(str::to_owned))
}

/// Asserts that `result` failed with a database error naming `constraint`.
#[track_caller]
pub fn assert_constraint<T: std::fmt::Debug>(result: Result<T, sqlx::Error>, constraint: &str) {
    match result {
        Ok(value) => panic!("expected violation of {constraint}, got Ok({value:?})"),
        Err(error) => assert_eq!(
            constraint_of(&error).as_deref(),
            Some(constraint),
            "unexpected error: {error}"
        ),
    }
}
