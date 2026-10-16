//! Team-scope helpers and the last-active-Admin guard against real PostgreSQL.

mod support;

use std::time::Duration;

use sqlx::PgPool;
use support::{
    TestDb,
    fixtures::{Role, account, member, team},
};
use tikkit_api::{
    api::ApiError,
    authz::{TeamScope, active_team_members, is_lead_of, team_of},
};
use uuid::Uuid;

fn sql_state(result: Result<impl std::fmt::Debug, sqlx::Error>) -> String {
    match result {
        Err(sqlx::Error::Database(e)) => e.code().unwrap().into_owned(),
        other => panic!("expected database error, got {other:?}"),
    }
}

async fn exec(pool: &PgPool, sql: &str, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(sql).bind(id).execute(pool).await.map(drop)
}

async fn active_admins(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin' AND is_active")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn team_helpers_respect_membership_leadership_activity_and_archival() {
    let Some(t) = TestDb::new().await else { return };
    let pool = &t.pool;
    let lead = account(pool, Role::TeamLead, "lead").await;
    let op = account(pool, Role::Operator, "op").await;
    let gone = account(pool, Role::Specialist, "gone").await;
    let loner = account(pool, Role::Operator, "loner").await;
    let desk = team(pool, "Desk", lead).await.unwrap();
    member(pool, desk, op).await.unwrap();
    member(pool, desk, gone).await.unwrap();
    exec(
        pool,
        "UPDATE accounts SET is_active = false WHERE id = $1",
        gone,
    )
    .await
    .unwrap();

    let scope = |is_lead| {
        Some(TeamScope {
            team_id: desk,
            is_lead,
        })
    };
    assert_eq!(team_of(pool, lead).await.unwrap(), scope(true));
    assert_eq!(team_of(pool, op).await.unwrap(), scope(false));
    assert_eq!(team_of(pool, loner).await.unwrap(), None);
    assert!(is_lead_of(pool, lead, desk).await.unwrap());
    assert!(!is_lead_of(pool, op, desk).await.unwrap());
    let mut expected = vec![lead, op];
    expected.sort();
    assert_eq!(active_team_members(pool, desk).await.unwrap(), expected);

    // An archived team grants nothing.
    exec(
        pool,
        "UPDATE teams SET archived_at = now(), lead_account_id = NULL, lead_role = NULL,
                          lead_active = NULL WHERE id = $1",
        desk,
    )
    .await
    .unwrap();
    assert_eq!(team_of(pool, op).await.unwrap(), None);
    assert!(!is_lead_of(pool, lead, desk).await.unwrap());
    assert!(active_team_members(pool, desk).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_last_active_admin_cannot_be_demoted_or_deactivated() {
    let Some(t) = TestDb::new().await else { return };
    let pool = &t.pool;
    let first = account(pool, Role::Admin, "first").await;
    let demote = "UPDATE accounts SET role = 'operator' WHERE id = $1";
    let deactivate = "UPDATE accounts SET is_active = false WHERE id = $1";

    for sql in [demote, deactivate] {
        let err = exec(pool, sql, first).await.unwrap_err();
        assert_eq!(sql_state(Err::<(), _>(err)), "23514");
        let err = exec(pool, sql, first).await.unwrap_err();
        assert!(matches!(ApiError::from(err), ApiError::LastActiveAdmin));
    }
    // Other updates of the last Admin are fine.
    exec(
        pool,
        "UPDATE accounts SET display_name = 'Boss' WHERE id = $1",
        first,
    )
    .await
    .unwrap();

    // With a second active Admin one may go; an inactive Admin does not count.
    let second = account(pool, Role::Admin, "second").await;
    exec(pool, deactivate, second).await.unwrap();
    assert!(exec(pool, demote, first).await.is_err());
    exec(
        pool,
        "UPDATE accounts SET is_active = true WHERE id = $1",
        second,
    )
    .await
    .unwrap();
    exec(pool, demote, first).await.unwrap();
    assert!(exec(pool, demote, second).await.is_err());
    // One statement demoting every Admin is rejected as a whole.
    exec(
        pool,
        "UPDATE accounts SET role = 'admin' WHERE id = $1",
        first,
    )
    .await
    .unwrap();
    assert!(
        sqlx::query("UPDATE accounts SET role = 'user' WHERE role = 'admin'")
            .execute(pool)
            .await
            .is_err()
    );
    assert_eq!(active_admins(pool).await, 2);
}

#[tokio::test]
async fn concurrent_removal_of_the_last_two_admins_has_one_winner() {
    for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
        let Some(t) = TestDb::new().await else { return };
        let pool = &t.pool;
        let a = account(pool, Role::Admin, "a").await;
        let b = account(pool, Role::Admin, "b").await;

        // Both take their snapshot before either writes.
        let mut first = pool.begin().await.unwrap();
        let mut second = pool.begin().await.unwrap();
        for tx in [&mut first, &mut second] {
            sqlx::query(&format!("SET TRANSACTION ISOLATION LEVEL {isolation}"))
                .execute(&mut **tx)
                .await
                .unwrap();
            sqlx::query("SELECT count(*) FROM accounts")
                .execute(&mut **tx)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE accounts SET is_active = false WHERE id = $1")
            .bind(a)
            .execute(&mut *first)
            .await
            .unwrap();
        let loser = tokio::spawn(async move {
            sqlx::query("UPDATE accounts SET role = 'user' WHERE id = $1")
                .bind(b)
                .execute(&mut *second)
                .await?;
            second.commit().await
        });
        // Wait until the second transaction is blocked behind the first.
        while !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE NOT granted)",
        )
        .fetch_one(pool)
        .await
        .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        first.commit().await.unwrap();
        let state = sql_state(loser.await.unwrap());
        assert!(
            ["23514", "40001"].contains(&state.as_str()),
            "{isolation}: {state}"
        );
        assert_eq!(active_admins(pool).await, 1, "{isolation}");
    }
}
