//! Proves the shared test support: disposable databases, the HTTP app, and role fixtures.

mod support;

use axum::{body::Body, http::Request};
use support::{
    TestDb,
    fixtures::{self, Role},
    test_app, test_config,
};
use tower::ServiceExt;

#[tokio::test]
async fn fixtures_cover_all_roles_teams_and_entry_paths() {
    let Some(t) = TestDb::new().await else { return };
    let pool = &t.pool;
    let admin = fixtures::bootstrap_admin(pool).await;
    let invited = fixtures::invited_account(pool, Role::User, "invited").await;
    let user = fixtures::account(pool, Role::User, "user").await;
    let operator = fixtures::account(pool, Role::Operator, "operator").await;
    let specialist = fixtures::account(pool, Role::Specialist, "specialist").await;
    let lead = fixtures::account(pool, Role::TeamLead, "lead").await;
    let team = fixtures::team(pool, "Service Desk", lead).await.unwrap();
    fixtures::member(pool, team, operator).await.unwrap();
    fixtures::member(pool, team, specialist).await.unwrap();

    let roles: Vec<(String, String)> = sqlx::query_as(
        "SELECT username, role FROM accounts WHERE password_hash = $1 ORDER BY username",
    )
    .bind(fixtures::PASSWORD_HASH)
    .fetch_all(pool)
    .await
    .unwrap();
    let expected = [
        ("admin", "admin"),
        ("invited", "user"),
        ("lead", "team_lead"),
        ("operator", "operator"),
        ("specialist", "specialist"),
        ("user", "user"),
    ];
    assert_eq!(
        roles,
        expected.map(|(u, r)| (u.to_owned(), r.to_owned())).to_vec()
    );
    let members: i64 =
        sqlx::query_scalar("SELECT count(*) FROM team_memberships WHERE team_id = $1")
            .bind(team)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(members, 3);
    assert!(fixtures::member(pool, team, admin).await.is_err());
    assert!(fixtures::member(pool, team, user).await.is_ok());
    assert_ne!(invited, user);
}

#[tokio::test]
async fn each_test_database_is_isolated_and_dropped() {
    let Some(a) = TestDb::new().await else { return };
    let b = TestDb::new().await.unwrap();
    fixtures::account(&a.pool, Role::User, "only_in_a").await;
    let in_b: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&b.pool)
        .await
        .unwrap();
    assert_eq!(in_b, 0);

    let name = b.name.clone();
    assert!(a.exists(&name).await);
    drop(b);
    assert!(!a.exists(&name).await, "{name} was not dropped");
}

#[tokio::test]
async fn test_app_serves_health_and_readiness() {
    test_config(); // panics if the shared test configuration stops validating
    let Some(t) = TestDb::new().await else { return };
    for (path, status) in [
        ("/api/health", 200),
        ("/api/ready", 200),
        ("/api/nope", 404),
    ] {
        let response = test_app(&t.pool)
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "{path}");
    }
}
