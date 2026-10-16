//! Team membership and leadership invariants enforced by the database (issue #6).

mod common;

use std::time::Duration;

use common::*;
use sqlx::PgPool;
use uuid::Uuid;

async fn current_leads(pool: &PgPool, team: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT account_id FROM team_memberships WHERE team_id = $1 AND is_lead AND ended_at IS NULL",
    )
    .bind(team)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn end_membership<'e, E>(executor: E, account: Uuid) -> Result<u64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "UPDATE team_memberships SET ended_at = now() WHERE account_id = $1 AND ended_at IS NULL",
    )
    .bind(account)
    .execute(executor)
    .await
    .map(|r| r.rows_affected())
}

#[tokio::test]
async fn an_account_has_at_most_one_current_team() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let lead2 = create_account(&test.pool, Role::TeamLead, "lead2", "lead2@example.test").await;
    let other = create_team_with_lead(&test.pool, "Network", &lead2).await;

    let second = add_membership(&test.pool, other.id, f.operator.id, false).await;
    assert_constraint(second, "team_memberships_one_current_per_account");

    // Moving teams = end the current membership, then insert the new one.
    let mut tx = test.pool.begin().await.unwrap();
    assert_eq!(end_membership(&mut *tx, f.operator.id).await.unwrap(), 1);
    add_membership(&mut *tx, other.id, f.operator.id, false)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let history: i64 =
        sqlx::query_scalar("SELECT count(*) FROM team_memberships WHERE account_id = $1")
            .bind(f.operator.id)
            .fetch_one(&test.pool)
            .await
            .unwrap();
    assert_eq!(history, 2, "historical membership is preserved");
}

#[tokio::test]
async fn admins_cannot_be_members_or_leads() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    assert_constraint(
        add_membership(&test.pool, f.team.id, f.admin.id, false).await,
        "team_memberships_no_admin",
    );

    // Promoting a current member to Admin is rejected until the membership ends.
    let promote = sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = $1")
        .bind(f.operator.id)
        .execute(&test.pool)
        .await;
    assert_constraint(promote, "team_memberships_no_admin");
    end_membership(&test.pool, f.operator.id).await.unwrap();
    sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = $1")
        .bind(f.operator.id)
        .execute(&test.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn leads_must_have_the_team_lead_role() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let operator = create_account(&test.pool, Role::Operator, "op2", "op2@example.test").await;

    let mut tx = test.pool.begin().await.unwrap();
    let team: Uuid = sqlx::query_scalar("INSERT INTO teams (name) VALUES ('Apps') RETURNING id")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_constraint(
        add_membership(&mut *tx, team, operator.id, true).await,
        "team_memberships_lead_role",
    );
    drop(tx);

    // A leading account cannot be demoted or deactivated while leading.
    let demote = sqlx::query("UPDATE accounts SET role = 'operator' WHERE id = $1")
        .bind(f.team_lead.id)
        .execute(&test.pool)
        .await;
    assert_constraint(demote, "team_memberships_lead_role");
    let deactivate = sqlx::query(
        "UPDATE accounts SET status = 'deactivated', deactivated_at = now() WHERE id = $1",
    )
    .bind(f.team_lead.id)
    .execute(&test.pool)
    .await;
    assert_constraint(deactivate, "team_memberships_lead_active");

    // Deactivated accounts cannot join a team.
    let user = create_account(&test.pool, Role::Operator, "gone", "gone@example.test").await;
    sqlx::query("UPDATE accounts SET status = 'deactivated', deactivated_at = now() WHERE id = $1")
        .bind(user.id)
        .execute(&test.pool)
        .await
        .unwrap();
    assert_constraint(
        add_membership(&test.pool, f.team.id, user.id, false).await,
        "team_memberships_active_account",
    );
}

#[tokio::test]
async fn active_team_without_lead_is_rejected_at_commit() {
    let test = TestDb::new().await;
    let mut tx = test.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO teams (name) VALUES ('Leaderless')")
        .execute(&mut *tx)
        .await
        .expect("the check is deferred until commit");
    let commit = tx.commit().await;
    assert_constraint(commit, "teams_exactly_one_lead");

    let teams: i64 = sqlx::query_scalar("SELECT count(*) FROM teams")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!(teams, 0);

    // Ending the only lead without a replacement is rejected too.
    let f = RoleFixtures::create(&test.pool).await;
    let orphan = end_membership(&test.pool, f.team_lead.id).await;
    assert_constraint(orphan, "teams_exactly_one_lead");
    assert_eq!(
        current_leads(&test.pool, f.team.id).await,
        vec![f.team_lead.id]
    );
}

#[tokio::test]
async fn creating_a_team_with_its_lead_is_atomic() {
    let test = TestDb::new().await;
    let lead = create_account(&test.pool, Role::TeamLead, "lead", "lead@example.test").await;
    let team = create_team_with_lead(&test.pool, "Field Support", &lead).await;
    assert_eq!(current_leads(&test.pool, team.id).await, vec![lead.id]);

    // Active team names are unique (case-insensitive).
    let lead2 = create_account(&test.pool, Role::TeamLead, "lead2", "lead2@example.test").await;
    let mut tx = test.pool.begin().await.unwrap();
    let dup = sqlx::query("INSERT INTO teams (name) VALUES ('field support')")
        .execute(&mut *tx)
        .await;
    assert_constraint(dup, "teams_active_name_key");
    drop(tx);
    assert!(
        current_leads(&test.pool, team.id).await.len() == 1
            && sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM team_memberships WHERE account_id = $1"
            )
            .bind(lead2.id)
            .fetch_one(&test.pool)
            .await
            .unwrap()
                == 0
    );
}

#[tokio::test]
async fn lead_can_be_replaced_in_one_transaction() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let successor = create_account(&test.pool, Role::TeamLead, "successor", "s@example.test").await;
    add_membership(&test.pool, f.team.id, successor.id, false)
        .await
        .unwrap();

    let mut tx = test.pool.begin().await.unwrap();
    end_membership(&mut *tx, f.team_lead.id).await.unwrap();
    end_membership(&mut *tx, successor.id).await.unwrap();
    add_membership(&mut *tx, f.team.id, successor.id, true)
        .await
        .unwrap();
    // The previous lead stays on the team as a regular member.
    add_membership(&mut *tx, f.team.id, f.team_lead.id, false)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        current_leads(&test.pool, f.team.id).await,
        vec![successor.id]
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM team_memberships WHERE team_id = $1")
        .bind(f.team.id)
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!(rows, 6, "ended rows are kept as history");

    // Now the old lead can be demoted.
    sqlx::query("UPDATE accounts SET role = 'operator' WHERE id = $1")
        .bind(f.team_lead.id)
        .execute(&test.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn co_leads_are_rejected() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let second = create_account(&test.pool, Role::TeamLead, "second", "second@example.test").await;
    assert_constraint(
        add_membership(&test.pool, f.team.id, second.id, true).await,
        "team_memberships_one_current_lead_per_team",
    );
}

#[tokio::test]
async fn concurrent_lead_appointments_have_one_winner() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let a = create_account(&test.pool, Role::TeamLead, "cand.a", "a@example.test").await;
    let b = create_account(&test.pool, Role::TeamLead, "cand.b", "b@example.test").await;

    // Each transaction replaces the current lead with its own candidate.
    let appoint = |candidate: Uuid| {
        let pool = test.pool.clone();
        let team = f.team.id;
        let old = f.team_lead.id;
        async move {
            let mut tx = pool.begin().await?;
            let ended = end_membership(&mut *tx, old).await?;
            add_membership(&mut *tx, team, candidate, true).await?;
            // Hold the transaction open so the two overlap.
            tokio::time::sleep(Duration::from_millis(200)).await;
            tx.commit().await?;
            Ok::<u64, sqlx::Error>(ended)
        }
    };
    let (ra, rb) = tokio::join!(tokio::spawn(appoint(a.id)), tokio::spawn(appoint(b.id)));
    let results = [ra.unwrap(), rb.unwrap()];
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "{results:?}");

    let leads = current_leads(&test.pool, f.team.id).await;
    assert_eq!(leads.len(), 1);
    assert!(leads[0] == a.id || leads[0] == b.id);
}

#[tokio::test]
async fn concurrent_admin_promotion_and_membership_have_one_winner() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let candidate = create_account(&test.pool, Role::Operator, "both", "both@example.test").await;

    let join = {
        let pool = test.pool.clone();
        let team = f.team.id;
        let id = candidate.id;
        async move {
            let mut tx = pool.begin().await?;
            add_membership(&mut *tx, team, id, false).await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            tx.commit().await
        }
    };
    let promote = {
        let pool = test.pool.clone();
        let id = candidate.id;
        async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut tx = pool.begin().await?;
            sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await
        }
    };
    let (rj, rp) = tokio::join!(tokio::spawn(join), tokio::spawn(promote));
    let (rj, rp) = (rj.unwrap(), rp.unwrap());
    assert!(
        rj.is_ok() != rp.is_ok(),
        "exactly one must win: {rj:?} / {rp:?}"
    );

    let (role, members): (String, i64) = sqlx::query_as(
        "SELECT a.role, (SELECT count(*) FROM team_memberships m WHERE m.account_id = a.id AND m.ended_at IS NULL) \
         FROM accounts a WHERE a.id = $1",
    )
    .bind(candidate.id)
    .fetch_one(&test.pool)
    .await
    .unwrap();
    assert!(!(role == "admin" && members > 0));
}

#[tokio::test]
async fn archived_teams_have_no_members_and_history_is_immutable() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;

    let archive_with_members =
        sqlx::query("UPDATE teams SET status = 'archived', archived_at = now() WHERE id = $1")
            .bind(f.team.id)
            .execute(&test.pool)
            .await;
    assert_constraint(archive_with_members, "teams_archived_without_members");

    let mut tx = test.pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE team_memberships SET ended_at = now() WHERE team_id = $1 AND ended_at IS NULL",
    )
    .bind(f.team.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE teams SET status = 'archived', archived_at = now() WHERE id = $1")
        .bind(f.team.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_constraint(
        add_membership(&test.pool, f.team.id, f.operator.id, false).await,
        "team_memberships_active_team",
    );
    let rewrite = sqlx::query("UPDATE team_memberships SET is_lead = NOT is_lead")
        .execute(&test.pool)
        .await;
    assert_constraint(rewrite, "team_memberships_history");
    let delete = sqlx::query("DELETE FROM team_memberships")
        .execute(&test.pool)
        .await;
    assert_constraint(delete, "team_memberships_history");
}
