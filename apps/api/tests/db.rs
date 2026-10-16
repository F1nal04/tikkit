//! PostgreSQL integration tests. Each test gets its own throwaway database.
//! Set TEST_DATABASE_URL (or DATABASE_URL); without it these tests are skipped.

use std::str::FromStr;

use sqlx::{
    Connection, Executor, PgConnection, PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tikkit_api::db;
use uuid::Uuid;

struct TestDb {
    pool: PgPool,
    admin: PgConnectOptions,
    name: String,
}

impl TestDb {
    async fn new() -> Option<Self> {
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
        Some(Self { pool, admin, name })
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

async fn account(pool: &PgPool, username: &str, role: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO accounts (username, display_name, email, password_hash, role)
         VALUES ($1, $1, 'shared@example.com', 'x', $2) RETURNING id",
    )
    .bind(username)
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Creates a team with `lead` as member and lead in one transaction.
async fn team(pool: &PgPool, name: &str, lead: Uuid) -> Result<Uuid, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO teams (name, lead_account_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(name)
    .bind(lead)
    .fetch_one(&mut *tx)
    .await?;
    join(&mut tx, lead, id).await?;
    tx.commit().await?;
    Ok(id)
}

async fn join(conn: &mut PgConnection, account: Uuid, team: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO team_memberships (account_id, role, team_id) SELECT id, role, $2 FROM accounts WHERE id = $1")
        .bind(account)
        .bind(team)
        .execute(conn)
        .await
        .map(drop)
}

fn sql_state(result: Result<impl std::fmt::Debug, sqlx::Error>) -> String {
    match result {
        Err(sqlx::Error::Database(e)) => e.code().unwrap().into_owned(),
        other => panic!("expected database error, got {other:?}"),
    }
}

#[tokio::test]
async fn migrations_apply_to_fresh_database_and_rerun_cleanly() {
    let Some(t) = TestDb::new().await else { return };
    db::migrate(&t.pool).await.unwrap();
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(applied as usize, db::MIGRATOR.migrations.len());
}

#[tokio::test]
async fn usernames_are_unique_after_normalization_but_emails_may_repeat() {
    let Some(t) = TestDb::new().await else { return };
    account(&t.pool, "Alice", "admin").await;
    // Same email, different username: a separate account.
    account(&t.pool, "alice.ops", "operator").await;
    let insert = |name: &'static str| {
        sqlx::query("INSERT INTO accounts (username, display_name, email, password_hash) VALUES ($1, 'A', 'a@example.com', 'x')")
            .bind(name)
            .execute(&t.pool)
    };
    assert_eq!(sql_state(insert("ALICE").await), "23505");
    // Whitespace (including NFKC-folded no-break space) and control characters are rejected.
    for bad in [
        " alice",
        "al ice",
        "al\u{a0}ice",
        "al\tice",
        "al\u{7}ice",
        "",
    ] {
        assert_eq!(sql_state(insert(bad).await), "23514", "{bad:?}");
    }
    // Precomposed and decomposed forms are the same username.
    insert("Jos\u{e9}").await.unwrap();
    assert_eq!(sql_state(insert("Jose\u{301}").await), "23505");
}

#[tokio::test]
async fn admins_cannot_be_members_or_leads() {
    let Some(t) = TestDb::new().await else { return };
    let admin = account(&t.pool, "admin", "admin").await;
    let lead = account(&t.pool, "lead", "team_lead").await;
    let team_id = team(&t.pool, "Network", lead).await.unwrap();

    let mut conn = t.pool.acquire().await.unwrap();
    assert_eq!(sql_state(join(&mut conn, admin, team_id).await), "23514");
    assert_eq!(sql_state(team(&t.pool, "Admins", admin).await), "23503");
    // A member cannot be promoted to admin while in a team.
    let promote = sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = $1")
        .bind(lead)
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(promote), "23514");
}

#[tokio::test]
async fn an_account_belongs_to_at_most_one_team_and_history_is_kept() {
    let Some(t) = TestDb::new().await else { return };
    let a = team(&t.pool, "A", account(&t.pool, "lead_a", "team_lead").await)
        .await
        .unwrap();
    let b = team(&t.pool, "B", account(&t.pool, "lead_b", "team_lead").await)
        .await
        .unwrap();
    let op = account(&t.pool, "op", "operator").await;
    let mut conn = t.pool.acquire().await.unwrap();
    join(&mut conn, op, a).await.unwrap();
    assert_eq!(sql_state(join(&mut conn, op, b).await), "23505");

    sqlx::query("UPDATE team_memberships SET team_id = $1 WHERE account_id = $2")
        .bind(b)
        .bind(op)
        .execute(&t.pool)
        .await
        .unwrap();
    let history: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM team_membership_history WHERE account_id = $1 ORDER BY id",
    )
    .bind(op)
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(history, ["joined", "left", "joined"]);
    let rewrite = sqlx::query("DELETE FROM team_membership_history")
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(rewrite), "23001");
    let truncate = sqlx::query("TRUNCATE team_membership_history")
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(truncate), "23001");
}

#[tokio::test]
async fn team_leads_are_unique_members_with_the_team_lead_role() {
    let Some(t) = TestDb::new().await else { return };
    let lead = account(&t.pool, "lead", "team_lead").await;
    let team_id = team(&t.pool, "Network", lead).await.unwrap();

    // One lead account cannot lead a second team.
    let mut tx = t.pool.begin().await.unwrap();
    let second = sqlx::query("INSERT INTO teams (name, lead_account_id) VALUES ('Second', $1)")
        .bind(lead)
        .execute(&mut *tx)
        .await;
    assert_eq!(sql_state(second), "23505");
    drop(tx);

    // The lead must hold the team_lead role.
    let op = account(&t.pool, "op", "operator").await;
    let mut conn = t.pool.acquire().await.unwrap();
    join(&mut conn, op, team_id).await.unwrap();
    let set_lead = sqlx::query("UPDATE teams SET lead_account_id = $1 WHERE id = $2")
        .bind(op)
        .bind(team_id)
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(set_lead), "23503");

    // A current lead cannot be demoted.
    let demote = sqlx::query("UPDATE accounts SET role = 'operator' WHERE id = $1")
        .bind(lead)
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(demote), "23514");

    // A current lead of an active team cannot be deactivated; other members can.
    let deactivate = |id: Uuid| {
        sqlx::query("UPDATE accounts SET is_active = false WHERE id = $1")
            .bind(id)
            .execute(&t.pool)
    };
    assert_eq!(sql_state(deactivate(lead).await), "23514");
    deactivate(op).await.unwrap();

    // The lead must be a member of the same team (checked at commit).
    let other_lead = account(&t.pool, "other", "team_lead").await;
    let mut tx = t.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO teams (name, lead_account_id) VALUES ('NoMember', $1)")
        .bind(other_lead)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(sql_state(tx.commit().await), "23503");

    // Leaving the team while still leading it fails; nothing was half-created.
    let leave = sqlx::query("DELETE FROM team_memberships WHERE account_id = $1")
        .bind(lead)
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(leave), "23503");
    let teams: i64 = sqlx::query_scalar("SELECT count(*) FROM teams")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(teams, 1);
}

#[tokio::test]
async fn tickets_keep_stable_references_events_and_foreign_keys() {
    let Some(t) = TestDb::new().await else { return };
    let requester = account(&t.pool, "req", "user").await;
    let lead = account(&t.pool, "lead", "team_lead").await;
    let team_id = team(&t.pool, "Desk", lead).await.unwrap();
    let category: Uuid =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Hardware') RETURNING id")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    let priority: Uuid =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    let insert = "INSERT INTO tickets (requester_account_id, category_id, priority_id, status, subject, description)
                  VALUES ($1, $2, $3, 'queued', 'Printer', 'Jammed') RETURNING id, reference";
    let row = sqlx::query(insert)
        .bind(requester)
        .bind(category)
        .bind(priority)
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let (ticket, reference): (Uuid, i64) = (row.get(0), row.get(1));

    sqlx::query("INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status) VALUES ($1, $2, 'submitted', 'draft', 'queued')")
        .bind(ticket)
        .bind(requester)
        .execute(&t.pool)
        .await
        .unwrap();
    let rewrite = sqlx::query("UPDATE ticket_events SET event_type = 'x'")
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(rewrite), "23001");
    for truncate in ["TRUNCATE ticket_events", "TRUNCATE tickets CASCADE"] {
        assert_eq!(
            sql_state(sqlx::query(truncate).execute(&t.pool).await),
            "23001"
        );
    }
    let bad_status = sqlx::query("UPDATE tickets SET status = 'resolved' WHERE id = $1")
        .bind(ticket)
        .execute(&t.pool)
        .await;
    assert_eq!(sql_state(bad_status), "23514");

    // Unknown foreign keys are rejected.
    let orphan = sqlx::query(insert)
        .bind(Uuid::new_v4())
        .bind(category)
        .bind(priority)
        .fetch_one(&t.pool)
        .await;
    assert_eq!(sql_state(orphan), "23503");

    // Assignees must be members of the ticket's team; requesters never handle their own ticket.
    let outsider = account(&t.pool, "outsider", "operator").await;
    let mut tx = t.pool.begin().await.unwrap();
    sqlx::query("UPDATE tickets SET team_id = $1, assignee_account_id = $2 WHERE id = $3")
        .bind(team_id)
        .bind(outsider)
        .bind(ticket)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(sql_state(tx.commit().await), "23503");
    let self_assign = sqlx::query(
        "UPDATE tickets SET team_id = $1, assignee_account_id = requester_account_id WHERE id = $2",
    )
    .bind(team_id)
    .bind(ticket)
    .execute(&t.pool)
    .await;
    assert_eq!(sql_state(self_assign), "23514");
    sqlx::query("UPDATE tickets SET team_id = $1, assignee_account_id = $2 WHERE id = $3")
        .bind(team_id)
        .bind(lead)
        .bind(ticket)
        .execute(&t.pool)
        .await
        .unwrap();

    // Rolled-back work leaves no trace; the reference never changes.
    let mut tx = t.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO ticket_messages (ticket_id, author_account_id, kind, body) VALUES ($1, $2, 'public_reply', 'hi')")
        .bind(ticket)
        .bind(requester)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let (messages, stored_reference): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM ticket_messages), reference FROM tickets WHERE id = $1",
    )
    .bind(ticket)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!((messages, stored_reference), (0, reference));
}

#[tokio::test]
async fn concurrent_membership_and_admin_promotion_cannot_both_commit() {
    let Some(t) = TestDb::new().await else { return };
    let lead = account(&t.pool, "lead", "team_lead").await;
    let team_id = team(&t.pool, "Desk", lead).await.unwrap();
    let op = account(&t.pool, "op", "operator").await;

    // Both transactions take their REPEATABLE READ snapshot before either writes.
    let mut joiner = t.pool.begin().await.unwrap();
    let mut promoter = t.pool.begin().await.unwrap();
    for tx in [&mut joiner, &mut promoter] {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("SELECT count(*) FROM team_memberships")
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    join(&mut joiner, op, team_id).await.unwrap();
    // The promotion blocks on the membership's key lock until the join commits.
    let promotion = tokio::spawn(async move {
        sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = $1")
            .bind(op)
            .execute(&mut *promoter)
            .await?;
        promoter.commit().await
    });
    joiner.commit().await.unwrap();
    let state = sql_state(promotion.await.unwrap());
    assert!(["40001", "23514"].contains(&state.as_str()), "{state}");

    let admin_members: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM team_memberships m JOIN accounts a ON a.id = m.account_id WHERE a.role = 'admin'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(admin_members, 0);
}
