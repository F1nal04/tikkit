use serde_json::{Value, json};
use uuid::Uuid;
mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

#[tokio::test]
async fn settings_are_validated_persisted_audited_and_readable_to_signed_in_clients() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    fixtures::account(&db.pool, Role::User, "requester").await;
    let app = test_app(&db.pool);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let user = login(&app, "requester", PASSWORD).await.session();
    let defaults = call(&app, "GET", "/api/organization", None, Some(&user)).await;
    assert_eq!(defaults.status, 200, "{}", defaults.body);
    assert_eq!(defaults.body["allow_reopening"], false);
    assert_eq!(defaults.body["resolution_confirmation_hours"], 168);
    assert_eq!(
        defaults.body["requester_cancellation_policy"],
        "anytime_while_open"
    );
    assert_eq!(defaults.body["allow_requester_ai_skip"], false);
    assert_eq!(
        call(&app, "GET", "/api/organization", None, None)
            .await
            .status,
        401
    );
    let mut settings = defaults.body.clone();
    settings["name"] = json!("Service desk");
    settings["timezone"] = json!("Europe/Berlin");
    settings["default_language"] = json!("de");
    settings["allow_reopening"] = json!(true);
    settings["resolution_confirmation_hours"] = json!(24);
    settings["requester_cancellation_policy"] = json!("before_work_starts");
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/admin/settings",
            Some(settings.clone()),
            Some(&user)
        )
        .await
        .status,
        403
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/admin/settings",
            Some(settings.clone()),
            Some(&admin)
        )
        .await
        .status,
        200
    );
    assert_eq!(
        call(&app, "GET", "/api/organization", None, Some(&user))
            .await
            .body,
        settings
    );
    settings["timezone"] = json!("Not/AZone");
    settings["resolution_confirmation_hours"] = json!(0);
    let bad = call(
        &app,
        "PUT",
        "/api/admin/settings",
        Some(settings),
        Some(&admin),
    )
    .await;
    assert_eq!(bad.status, 422);
    assert!(bad.body["error"]["details"]["fields"]["timezone"].is_array());
    assert!(bad.body["error"]["details"]["fields"]["resolution_confirmation_hours"].is_array());
    for timezone in ["Factory", "localtime", "posixrules"] {
        let mut body = defaults.body.clone();
        body["timezone"] = json!(timezone);
        assert_eq!(
            call(&app, "PUT", "/api/admin/settings", Some(body), Some(&admin))
                .await
                .status,
            422,
            "{timezone}"
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE event_type = 'organization_settings_updated' AND actor_account_id IS NOT NULL").fetch_one(&db.pool).await.unwrap();
    assert_eq!(count, 1);
}

async fn edit(
    app: &axum::Router,
    admin: &str,
    id: Uuid,
    role: &str,
    team: Option<Uuid>,
    expertise: Option<&str>,
) -> support::Reply {
    call(
        app,
        "PUT",
        &format!("/api/accounts/{id}"),
        Some(json!({"role": role, "team_id": team, "expertise": expertise})),
        Some(admin),
    )
    .await
}

#[tokio::test]
async fn teams_and_roles_preserve_leadership_and_immediately_change_live_session_scope() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let aid = fixtures::bootstrap_admin(&db.pool).await;
    let lead = fixtures::account(&db.pool, Role::TeamLead, "lead").await;
    let next = fixtures::account(&db.pool, Role::TeamLead, "next").await;
    let staff = fixtures::account(&db.pool, Role::Operator, "staff").await;
    let app = test_app(&db.pool);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let staff_session = login(&app, "staff", PASSWORD).await.session();
    let create = call(
        &app,
        "POST",
        "/api/teams",
        Some(json!({"name":"Support", "lead_account_id":lead})),
        Some(&admin),
    )
    .await;
    assert_eq!(create.status, 201, "{}", create.body);
    let team: Uuid = create.body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        edit(
            &app,
            &admin,
            staff,
            "specialist",
            Some(team),
            Some("Networks")
        )
        .await
        .status,
        204
    );
    let scope = tikkit_api::authz::team_of(&db.pool, staff)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scope.team_id, team);
    assert_eq!(
        call(&app, "GET", "/api/auth/me", None, Some(&staff_session))
            .await
            .body["role"],
        "specialist"
    );
    // A deactivated member keeps its team, so editing it with its current team still works.
    let deactivate = format!("/api/accounts/{staff}/deactivate");
    assert_eq!(
        call(&app, "POST", &deactivate, None, Some(&admin))
            .await
            .status,
        204
    );
    assert_eq!(
        edit(&app, &admin, staff, "specialist", Some(team), Some("Wi-Fi"))
            .await
            .status,
        204
    );
    let reactivate = format!("/api/accounts/{staff}/reactivate");
    assert_eq!(
        call(&app, "POST", &reactivate, None, Some(&admin))
            .await
            .status,
        204
    );
    assert_eq!(
        edit(&app, &admin, aid, "admin", Some(team), None)
            .await
            .status,
        422
    );
    assert_eq!(
        edit(&app, &admin, lead, "operator", None, None)
            .await
            .code(),
        "active_team_lead"
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/accounts/{lead}/deactivate"),
            None,
            Some(&admin)
        )
        .await
        .code(),
        "active_team_lead"
    );
    let replace = call(
        &app,
        "PUT",
        &format!("/api/teams/{team}"),
        Some(json!({"name":"Support", "lead_account_id":next, "archived":false})),
        Some(&admin),
    )
    .await;
    assert_eq!(replace.status, 200, "{}", replace.body);
    assert!(
        !tikkit_api::authz::team_of(&db.pool, lead)
            .await
            .unwrap()
            .unwrap()
            .is_lead
    );
    assert_eq!(
        edit(&app, &admin, lead, "operator", None, None)
            .await
            .status,
        204
    );
    assert_eq!(
        edit(&app, &admin, staff, "admin", None, None).await.status,
        204
    );
    assert!(
        tikkit_api::authz::team_of(&db.pool, staff)
            .await
            .unwrap()
            .is_none()
    );
    let archive = call(
        &app,
        "PUT",
        &format!("/api/teams/{team}"),
        Some(json!({"name":"Support", "lead_account_id":null, "archived":true})),
        Some(&admin),
    )
    .await;
    assert_eq!(archive.status, 200, "{}", archive.body);
    assert_eq!(
        edit(&app, &admin, next, "user", None, None).await.status,
        204
    );
    let changes: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE event_type IN ('account_updated','team_created','team_updated','team_membership_changed')").fetch_one(&db.pool).await.unwrap();
    assert!(changes >= 7);
}

#[tokio::test]
async fn every_management_endpoint_denies_every_non_admin_role() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let admin = fixtures::bootstrap_admin(&db.pool).await;
    let app = test_app(&db.pool);
    for (role, name) in [
        (Role::User, "usr"),
        (Role::Operator, "ops"),
        (Role::Specialist, "spc"),
        (Role::TeamLead, "lead"),
    ] {
        fixtures::account(&db.pool, role, name).await;
        let session = login(&app, name, PASSWORD).await.session();
        for (method, path, body) in [
            ("GET", "/api/teams".into(), None),
            (
                "POST",
                "/api/teams".into(),
                Some(json!({"name":"Forged", "lead_account_id":admin})),
            ),
            (
                "PUT",
                format!("/api/teams/{admin}"),
                Some(json!({"name":"Forged", "lead_account_id":admin,"archived":false})),
            ),
            (
                "PUT",
                format!("/api/accounts/{admin}"),
                Some(json!({"role":"admin","team_id":null,"expertise":null})),
            ),
            ("GET", "/api/accounts".into(), None),
            ("GET", "/api/invitations".into(), None),
            ("POST", "/api/invitations".into(), Some(json!({}))),
            ("POST", format!("/api/invitations/{admin}/revoke"), None),
            ("POST", format!("/api/accounts/{admin}/deactivate"), None),
            ("POST", format!("/api/accounts/{admin}/reactivate"), None),
            ("GET", "/api/admin/settings".into(), None),
            (
                "PUT",
                "/api/admin/settings".into(),
                Some(
                    json!({"name":"Desk", "timezone":"UTC", "default_language":"en", "allow_reopening":false, "requester_cancellation_policy":"anytime_while_open", "resolution_confirmation_hours":168, "allow_requester_ai_skip":false}),
                ),
            ),
            ("POST", format!("/api/invitations/{admin}/reissue"), None),
        ] {
            let r = call(&app, method, &path, body, Some(&session)).await;
            assert_eq!(r.status, 403, "{name} {method} {path}: {}", r.body);
        }
    }
}

#[tokio::test]
async fn concurrent_admin_demotions_have_one_winner_and_preserve_last_admin() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let first = fixtures::bootstrap_admin(&db.pool).await;
    let second = fixtures::account(&db.pool, Role::Admin, "second").await;
    let app = test_app(&db.pool);
    let a = login(&app, "admin", PASSWORD).await.session();
    let b = login(&app, "second", PASSWORD).await.session();
    let (a, b) = tokio::join!(
        edit(&app, &a, first, "user", None, None),
        edit(&app, &b, second, "user", None, None)
    );
    assert_eq!(
        [a.status.as_u16(), b.status.as_u16()]
            .into_iter()
            .filter(|s| *s == 204)
            .count(),
        1
    );
    assert!([a.code(), b.code()].contains(&"last_active_admin"));
}

#[tokio::test]
async fn invitation_history_is_token_free_and_reissue_has_one_winner() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let app = test_app(&db.pool);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let created = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({"role":"specialist"})),
        Some(&admin),
    )
    .await;
    let id = created.body["invitation"]["id"].as_str().unwrap();
    let path = format!("/api/invitations/{id}/reissue");
    let (a, b) = tokio::join!(
        call(&app, "POST", &path, None, Some(&admin)),
        call(&app, "POST", &path, None, Some(&admin))
    );
    assert_eq!(
        [a.status.as_u16(), b.status.as_u16()]
            .into_iter()
            .filter(|s| *s == 201)
            .count(),
        1
    );
    let history = call(
        &app,
        "GET",
        "/api/invitations?include_history=true",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(history.body["items"].as_array().unwrap().len(), 2);
    let old = history.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .unwrap();
    assert_eq!(old["status"], "revoked");
    assert!(
        history.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("url").is_none() && i.get("token_hash").is_none())
    );
    let token = created.body["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .split('?')
        .next()
        .unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/invitations/lookup",
            Some(json!({"token":token})),
            None
        )
        .await
        .code(),
        "invitation_invalid"
    );
    let replacement = if a.status == 201 { a } else { b };
    let replacement_token = replacement.body["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .split('?')
        .next()
        .unwrap();
    let accepted = call(&app,"POST","/api/invitations/accept",Some(json!({"token":replacement_token,"username":"accepted","email":"shared@example.com","password":PASSWORD,"language":"en"})),None).await;
    assert_eq!(accepted.status, 200, "{}", accepted.body);
    assert_eq!(
        call(
            &app,
            "POST",
            &format!(
                "/api/invitations/{}/reissue",
                replacement.body["invitation"]["id"].as_str().unwrap()
            ),
            None,
            Some(&admin)
        )
        .await
        .status,
        409
    );
    let expired = call(
        &app,
        "POST",
        "/api/invitations",
        Some(json!({})),
        Some(&admin),
    )
    .await;
    let expired_id: Uuid = expired.body["invitation"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query("UPDATE invitations SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(expired_id)
        .execute(&db.pool)
        .await
        .unwrap();
    let history = call(
        &app,
        "GET",
        "/api/invitations?include_history=true",
        None,
        Some(&admin),
    )
    .await;
    let rows = history.body["items"].as_array().unwrap();
    assert!(rows.iter().any(|i| i["status"] == "expired"));
    let accepted_row = rows.iter().find(|i| i["status"] == "accepted").unwrap();
    assert_eq!(accepted_row["username"], "accepted");
    assert_eq!(accepted_row["account_id"], accepted.body["id"]);
    let audit: Vec<Value> = sqlx::query_scalar("SELECT metadata FROM audit_events")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert!(!format!("{audit:?}").contains(token));
}

#[tokio::test]
async fn removal_clears_assignment_audits_it_and_revokes_staff_access_but_not_requester_access() {
    use tikkit_api::{
        api::TicketStatus,
        auth::CurrentUser,
        authz::{self, Actor, TicketScope, TicketView},
    };
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let lead = fixtures::account(&db.pool, Role::TeamLead, "lead").await;
    let staff = fixtures::account(&db.pool, Role::Operator, "staff").await;
    let requester = fixtures::account(&db.pool, Role::User, "requester").await;
    let team = fixtures::team(&db.pool, "Desk", lead).await.unwrap();
    fixtures::member(&db.pool, team, staff).await.unwrap();
    let ticket: Uuid=sqlx::query_scalar("WITH c AS (INSERT INTO categories(name) VALUES ('IT') RETURNING id), p AS (INSERT INTO priorities(name) VALUES ('Normal') RETURNING id) INSERT INTO tickets(requester_account_id,category_id,priority_id,team_id,assignee_account_id,status,subject,description) SELECT $1,c.id,p.id,$2,$3,'in_progress','Test','Test' FROM c,p RETURNING id").bind(requester).bind(team).bind(staff).fetch_one(&db.pool).await.unwrap();
    let app = test_app(&db.pool);
    let admin = login(&app, "admin", PASSWORD).await.session();
    let session = login(&app, "staff", PASSWORD).await.session();
    let me = call(&app, "GET", "/api/auth/me", None, Some(&session)).await;
    let user = CurrentUser {
        session_id: Uuid::new_v4(),
        account: serde_json::from_value(me.body).unwrap(),
    };
    let scope = TicketScope {
        requester_account_id: requester,
        team_id: Some(team),
        assignee_account_id: Some(staff),
        status: TicketStatus::InProgress,
    };
    assert_eq!(
        authz::ticket_view(&Actor::load(&db.pool, &user).await.unwrap(), &scope),
        Some(TicketView::Staff)
    );
    assert_eq!(
        edit(&app, &admin, staff, "operator", None, None)
            .await
            .status,
        204
    );
    assert_eq!(
        call(&app, "GET", "/api/auth/me", None, Some(&session))
            .await
            .status,
        200
    );
    let actor = Actor::load(&db.pool, &user).await.unwrap();
    assert_eq!(authz::ticket_view(&actor, &scope), None);
    let own = TicketScope {
        requester_account_id: staff,
        ..scope
    };
    assert_eq!(
        authz::ticket_view(&actor, &own),
        Some(TicketView::Requester)
    );
    assert!(
        !authz::active_team_members(&db.pool, team)
            .await
            .unwrap()
            .contains(&staff)
    );
    let assignee: Option<Uuid> =
        sqlx::query_scalar("SELECT assignee_account_id FROM tickets WHERE id=$1")
            .bind(ticket)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(assignee, None);
    let events: i64=sqlx::query_scalar("SELECT count(*) FROM ticket_events WHERE ticket_id=$1 AND event_type='assignment_changed' AND actor_account_id IS NOT NULL").bind(ticket).fetch_one(&db.pool).await.unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
async fn competing_team_creations_cannot_share_a_lead_and_restore_requires_an_eligible_lead() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fixtures::bootstrap_admin(&db.pool).await;
    let lead = fixtures::account(&db.pool, Role::TeamLead, "lead").await;
    let admin = login(&test_app(&db.pool), "admin", PASSWORD)
        .await
        .session();
    let app = test_app(&db.pool);
    let body = json!({"name":"One","lead_account_id":lead});
    let other = json!({"name":"Two","lead_account_id":lead});
    let (a, b) = tokio::join!(
        call(&app, "POST", "/api/teams", Some(body), Some(&admin)),
        call(&app, "POST", "/api/teams", Some(other), Some(&admin))
    );
    assert_eq!(
        [a.status.as_u16(), b.status.as_u16()]
            .into_iter()
            .filter(|s| *s == 201)
            .count(),
        1
    );
    let winner = if a.status == 201 { a } else { b };
    let id = winner.body["id"].as_str().unwrap();
    let path = format!("/api/teams/{id}");
    assert_eq!(
        call(
            &app,
            "PUT",
            &path,
            Some(json!({"name":"One","lead_account_id":null,"archived":true})),
            Some(&admin)
        )
        .await
        .status,
        200
    );
    let restored = call(
        &app,
        "PUT",
        &path,
        Some(json!({"name":"One","lead_account_id":lead,"archived":false})),
        Some(&admin),
    )
    .await;
    assert_eq!(restored.status, 200, "{}", restored.body);
    assert!(
        tikkit_api::authz::is_lead_of(&db.pool, lead, id.parse().unwrap())
            .await
            .unwrap()
    );
}
