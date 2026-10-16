//! `/api/v1/admin` account directory and team endpoints over HTTP (issue #11).

mod common;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery staple";

struct Reply {
    status: StatusCode,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or_default()
    }
}

async fn call(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut builder = Request::builder().method(method.clone()).uri(path);
    if method != Method::GET {
        builder = builder.header(header::ORIGIN, DEV_ORIGIN);
    }
    if let Some(token) = token {
        builder = builder.header(header::COOKIE, format!("tikkit_session={token}"));
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    Reply { status, body }
}

async fn login(app: &Router, username: &str) -> String {
    let reply = app
        .clone()
        .oneshot(
            Request::post("/api/v1/auth/login")
                .header(header::ORIGIN, DEV_ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "username": username, "password": PASSWORD }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let cookie = reply.headers()[header::SET_COOKIE].to_str().unwrap();
    cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_owned()
}

/// One loginable account per role; the lead leads `team`, the Operator and
/// Specialist are members; plus an unaffiliated Team Lead (`spare_lead`)
/// and an unaffiliated Operator (`spare_operator`).
struct World {
    app: Router,
    accounts: Vec<(Role, Account)>,
    team: Team,
    spare_lead: Account,
    spare_operator: Account,
}

impl World {
    async fn create(test: &TestDb) -> Self {
        let pool = &test.pool;
        let mut accounts = Vec::new();
        for role in Role::ALL {
            let name = role.as_str().replace('_', ".");
            accounts.push((
                role,
                create_account_with_password(
                    pool,
                    role,
                    &name,
                    &format!("{name}@example.test"),
                    PASSWORD,
                )
                .await,
            ));
        }
        let get = |r: Role| accounts.iter().find(|(x, _)| *x == r).unwrap().1.clone();
        let team = create_team_with_lead(pool, "Service Desk", &get(Role::TeamLead)).await;
        for r in [Role::Operator, Role::Specialist] {
            add_membership(pool, team.id, get(r).id, false)
                .await
                .unwrap();
        }
        let spare_lead = create_account(pool, Role::TeamLead, "spare.lead", "s@example.test").await;
        let spare_operator =
            create_account(pool, Role::Operator, "spare.op", "so@example.test").await;
        Self {
            app: test.app(),
            accounts,
            team,
            spare_lead,
            spare_operator,
        }
    }

    fn account(&self, role: Role) -> &Account {
        &self.accounts.iter().find(|(r, _)| *r == role).unwrap().1
    }

    /// Every admin endpoint with a valid request against existing resources.
    fn requests(&self) -> Vec<(Method, String, Option<Value>)> {
        let team = self.team.id;
        let user = self.account(Role::User).id;
        let operator = self.account(Role::Operator).id;
        vec![
            (Method::GET, "/api/v1/admin/accounts".into(), None),
            (
                Method::GET,
                "/api/v1/admin/accounts?role=operator&q=op".into(),
                None,
            ),
            (Method::GET, format!("/api/v1/admin/accounts/{user}"), None),
            (Method::GET, "/api/v1/admin/teams".into(), None),
            (Method::GET, format!("/api/v1/admin/teams/{team}"), None),
            (
                Method::PATCH,
                format!("/api/v1/admin/accounts/{user}/role"),
                Some(json!({ "role": "specialist", "specialization": "Printers" })),
            ),
            (
                Method::PATCH,
                format!("/api/v1/admin/teams/{team}"),
                Some(json!({ "name": "Help Desk" })),
            ),
            (
                Method::POST,
                format!("/api/v1/admin/teams/{team}/members"),
                Some(json!({ "accountId": self.spare_operator.id })),
            ),
            (
                Method::DELETE,
                format!("/api/v1/admin/teams/{team}/members/{operator}"),
                None,
            ),
            (
                Method::POST,
                format!("/api/v1/admin/teams/{team}/lead"),
                Some(
                    json!({ "accountId": self.spare_lead.id, "previousLead": "remove_from_team" }),
                ),
            ),
            (
                Method::POST,
                "/api/v1/admin/teams".into(),
                Some(
                    json!({ "name": "Network", "leadAccountId": self.account(Role::TeamLead).id }),
                ),
            ),
            (
                Method::POST,
                format!("/api/v1/admin/teams/{team}/archive"),
                None,
            ),
        ]
    }
}

async fn audit_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action NOT LIKE 'auth.%'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn admin_endpoints_are_admin_only_for_every_role() {
    let test = TestDb::new().await;
    let world = World::create(&test).await;

    // Anonymous: 401 everywhere.
    for (method, path, body) in world.requests() {
        let reply = call(&world.app, method.clone(), &path, None, body).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(reply.code(), "unauthenticated");
    }
    // Every non-admin role: 403, nothing changes.
    for role in [Role::User, Role::Operator, Role::Specialist, Role::TeamLead] {
        let token = login(&world.app, &world.account(role).username).await;
        for (method, path, body) in world.requests() {
            let reply = call(&world.app, method.clone(), &path, Some(&token), body).await;
            assert_eq!(
                reply.status,
                StatusCode::FORBIDDEN,
                "{role:?} {method} {path}: {}",
                reply.body
            );
            assert_eq!(reply.code(), "forbidden");
        }
    }
    assert_eq!(audit_count(&test.pool).await, 0);

    // Admin: every request succeeds, in order.
    let token = login(&world.app, &world.account(Role::Admin).username).await;
    let expected = [200, 200, 200, 200, 200, 200, 200, 200, 200, 200, 201, 200];
    for ((method, path, body), status) in world.requests().into_iter().zip(expected) {
        let reply = call(&world.app, method.clone(), &path, Some(&token), body).await;
        assert_eq!(
            reply.status.as_u16(),
            status,
            "{method} {path}: {}",
            reply.body
        );
    }
    assert_eq!(audit_count(&test.pool).await, 7);
}

#[tokio::test]
async fn responses_have_the_documented_shape() {
    let test = TestDb::new().await;
    let world = World::create(&test).await;
    let token = login(&world.app, &world.account(Role::Admin).username).await;
    let lead = world.account(Role::TeamLead);

    let reply = call(
        &world.app,
        Method::GET,
        &format!("/api/v1/admin/accounts/{}", lead.id),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let mut keys: Vec<&str> = reply
        .body
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "createdAt",
            "deactivatedAt",
            "displayName",
            "email",
            "id",
            "language",
            "role",
            "specialization",
            "status",
            "team",
            "updatedAt",
            "username"
        ]
    );
    assert_eq!(reply.body["role"], "team_lead");
    assert_eq!(
        reply.body["team"],
        json!({ "id": world.team.id, "name": "Service Desk", "isLead": true })
    );
    assert!(reply.body.get("passwordHash").is_none());

    let page = call(
        &world.app,
        Method::GET,
        "/api/v1/admin/accounts?limit=2",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(page.body["items"].as_array().unwrap().len(), 2);
    let cursor = page.body["nextCursor"].as_str().unwrap();
    let next = call(
        &world.app,
        Method::GET,
        &format!("/api/v1/admin/accounts?limit=100&cursor={cursor}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(next.body["items"].as_array().unwrap().len(), 5);
    assert_eq!(next.body["nextCursor"], Value::Null);

    let team = call(
        &world.app,
        Method::GET,
        &format!("/api/v1/admin/teams/{}", world.team.id),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(team.body["status"], "active");
    assert_eq!(team.body["lead"]["id"], json!(lead.id));
    assert_eq!(team.body["members"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn unknown_or_malformed_ids_are_not_found() {
    let test = TestDb::new().await;
    let world = World::create(&test).await;
    let token = login(&world.app, &world.account(Role::Admin).username).await;
    let missing = Uuid::now_v7();
    let team = world.team.id;
    let cases = [
        (
            Method::GET,
            format!("/api/v1/admin/accounts/{missing}"),
            None,
        ),
        (
            Method::GET,
            "/api/v1/admin/accounts/not-a-uuid".into(),
            None,
        ),
        (
            Method::PATCH,
            format!("/api/v1/admin/accounts/{missing}/role"),
            Some(json!({ "role": "operator" })),
        ),
        (Method::GET, format!("/api/v1/admin/teams/{missing}"), None),
        (Method::GET, "/api/v1/admin/teams/123".into(), None),
        (
            Method::PATCH,
            format!("/api/v1/admin/teams/{missing}"),
            Some(json!({ "name": "X" })),
        ),
        (
            Method::POST,
            format!("/api/v1/admin/teams/{missing}/lead"),
            Some(json!({ "accountId": world.spare_lead.id })),
        ),
        (
            Method::POST,
            format!("/api/v1/admin/teams/{missing}/members"),
            Some(json!({ "accountId": world.spare_operator.id })),
        ),
        (
            Method::DELETE,
            format!("/api/v1/admin/teams/{team}/members/{missing}"),
            None,
        ),
        (
            Method::DELETE,
            format!(
                "/api/v1/admin/teams/{missing}/members/{}",
                world.spare_operator.id
            ),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/admin/teams/{missing}/archive"),
            None,
        ),
    ];
    for (method, path, body) in cases {
        let reply = call(&world.app, method.clone(), &path, Some(&token), body).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{method} {path}: {}",
            reply.body
        );
        assert_eq!(reply.code(), "not_found");
    }
    // Unknown account ids in bodies are validation errors on the field.
    let reply = call(
        &world.app,
        Method::POST,
        &format!("/api/v1/admin/teams/{team}/members"),
        Some(&token),
        Some(json!({ "accountId": missing })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reply.body["error"]["fields"][0]["field"], "accountId");
}

#[tokio::test]
async fn last_active_admin_is_protected_over_http() {
    let test = TestDb::new().await;
    let world = World::create(&test).await;
    let admin = world.account(Role::Admin);
    let token = login(&world.app, &admin.username).await;
    let reply = call(
        &world.app,
        Method::PATCH,
        &format!("/api/v1/admin/accounts/{}/role", admin.id),
        Some(&token),
        Some(json!({ "role": "operator" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "last_admin");
    // Still an Admin.
    let me = call(
        &world.app,
        Method::GET,
        "/api/v1/admin/teams",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(me.status, StatusCode::OK);
}

#[tokio::test]
async fn role_changes_apply_to_active_sessions_immediately() {
    let test = TestDb::new().await;
    let world = World::create(&test).await;
    let first = world.account(Role::Admin);
    let second = create_account_with_password(
        &test.pool,
        Role::Admin,
        "second.admin",
        "second@example.test",
        PASSWORD,
    )
    .await;
    let first_token = login(&world.app, &first.username).await;
    let second_token = login(&world.app, &second.username).await;
    let probe = |token: String| {
        let app = world.app.clone();
        async move {
            call(
                &app,
                Method::GET,
                "/api/v1/admin/accounts",
                Some(&token),
                None,
            )
            .await
            .status
        }
    };
    assert_eq!(probe(second_token.clone()).await, StatusCode::OK);

    let reply = call(
        &world.app,
        Method::PATCH,
        &format!("/api/v1/admin/accounts/{}/role", second.id),
        Some(&first_token),
        Some(json!({ "role": "operator" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["role"], "operator");

    // The demoted Admin's existing session: still signed in, no admin access.
    assert_eq!(probe(second_token.clone()).await, StatusCode::FORBIDDEN);
    let session = call(
        &world.app,
        Method::GET,
        "/api/v1/auth/session",
        Some(&second_token),
        None,
    )
    .await;
    assert_eq!(session.status, StatusCode::OK);
    assert_eq!(session.body["account"]["role"], "operator");

    // Promotion applies just as immediately.
    let user_token = login(&world.app, &world.account(Role::User).username).await;
    assert_eq!(probe(user_token.clone()).await, StatusCode::FORBIDDEN);
    call(
        &world.app,
        Method::PATCH,
        &format!(
            "/api/v1/admin/accounts/{}/role",
            world.account(Role::User).id
        ),
        Some(&first_token),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(probe(user_token).await, StatusCode::OK);
}

#[tokio::test]
async fn shared_email_admin_and_team_lead_are_independent() {
    let test = TestDb::new().await;
    let shared = "it@example.test";
    let admin =
        create_account_with_password(&test.pool, Role::Admin, "it.admin", shared, PASSWORD).await;
    let lead =
        create_account_with_password(&test.pool, Role::TeamLead, "it.lead", shared, PASSWORD).await;
    let team = create_team_with_lead(&test.pool, "Ops", &lead).await;
    let app = test.app();
    let admin_token = login(&app, &admin.username).await;
    let lead_token = login(&app, &lead.username).await;

    // The lead account gets no admin rights from the shared mailbox.
    let reply = call(
        &app,
        Method::GET,
        "/api/v1/admin/accounts",
        Some(&lead_token),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    // The Admin account cannot join the team; the lead cannot become Admin.
    let join = call(
        &app,
        Method::POST,
        &format!("/api/v1/admin/teams/{}/members", team.id),
        Some(&admin_token),
        Some(json!({ "accountId": admin.id })),
    )
    .await;
    assert_eq!(join.code(), "team_membership_conflict");
    let promote = call(
        &app,
        Method::PATCH,
        &format!("/api/v1/admin/accounts/{}/role", lead.id),
        Some(&admin_token),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promote.code(), "team_lead_required");

    let list = call(
        &app,
        Method::GET,
        "/api/v1/admin/accounts?q=it%40example",
        Some(&admin_token),
        None,
    )
    .await;
    let items = list.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "both accounts are listed separately");
    let by_name = |name: &str| items.iter().find(|i| i["username"] == name).unwrap();
    assert_eq!(by_name("it.admin")["role"], "admin");
    assert_eq!(by_name("it.admin")["team"], Value::Null);
    assert_eq!(by_name("it.lead")["role"], "team_lead");
    assert_eq!(by_name("it.lead")["team"]["isLead"], true);
}

#[tokio::test]
async fn invalid_inputs_are_rejected() {
    let test = TestDb::new().await;
    let world = World::create(&test).await;
    let token = login(&world.app, &world.account(Role::Admin).username).await;
    let user = world.account(Role::User).id;
    let role_path = format!("/api/v1/admin/accounts/{user}/role");

    for body in [
        json!({ "role": "superuser" }),
        json!({ "role": "Admin" }),
        json!({}),
        json!({ "role": "operator", "isAdmin": true }),
        json!({ "role": "operator", "team": world.team.id }),
    ] {
        let reply = call(
            &world.app,
            Method::PATCH,
            &role_path,
            Some(&token),
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{body}: {}",
            reply.body
        );
        assert_eq!(reply.code(), "invalid_request");
    }
    let reply = call(
        &world.app,
        Method::POST,
        "/api/v1/admin/teams",
        Some(&token),
        Some(json!({ "name": "X", "leadAccountId": world.spare_lead.id, "members": [] })),
    )
    .await;
    assert_eq!(reply.code(), "invalid_request");
    let reply = call(
        &world.app,
        Method::POST,
        &format!("/api/v1/admin/teams/{}/lead", world.team.id),
        Some(&token),
        Some(json!({ "accountId": world.spare_lead.id, "previousLead": "co_lead" })),
    )
    .await;
    assert_eq!(reply.code(), "invalid_request");

    // Specialization on a non-Specialist role → 422.
    let reply = call(
        &world.app,
        Method::PATCH,
        &role_path,
        Some(&token),
        Some(json!({ "role": "operator", "specialization": "Printers" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Query validation.
    for (query, status) in [
        ("role=root", StatusCode::UNPROCESSABLE_ENTITY),
        ("status=gone", StatusCode::UNPROCESSABLE_ENTITY),
        ("limit=0", StatusCode::UNPROCESSABLE_ENTITY),
        ("cursor=%%%", StatusCode::UNPROCESSABLE_ENTITY),
        ("sort=email", StatusCode::BAD_REQUEST),
    ] {
        let reply = call(
            &world.app,
            Method::GET,
            &format!("/api/v1/admin/accounts?{query}"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(reply.status, status, "{query}: {}", reply.body);
    }

    // Unsafe requests without the Origin header fail the CSRF check.
    let reply = world
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/admin/teams/{}/archive", world.team.id))
                .header(header::COOKIE, format!("tikkit_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FORBIDDEN);
    assert_eq!(audit_count(&test.pool).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT role FROM accounts WHERE id = $1")
            .bind(user)
            .fetch_one(&test.pool)
            .await
            .unwrap(),
        "user"
    );
}
