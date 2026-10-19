//! Categories, priorities, conditional intake fields, and intake validation
//! against real PostgreSQL (skipped without TEST_DATABASE_URL).
//!
//! Not covered yet: submission through the API and category changes during an
//! AI attempt need the ticket endpoints (#16/#17) and AI jobs (milestone 3);
//! they build on `validate_intake` and its snapshot, which are tested here.

use std::collections::BTreeMap;

use axum::{Router, http::StatusCode};
use serde_json::{Value, json};
use tikkit_api::catalog::{IntakeAnswer, validate_intake};
use uuid::Uuid;

mod support;
use support::{
    Reply, TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

async fn admin_session(t: &TestDb, app: &Router) -> String {
    fixtures::bootstrap_admin(&t.pool).await;
    login(app, "admin", PASSWORD).await.session()
}

async fn send(app: &Router, method: &str, path: &str, body: Value, session: &str) -> Reply {
    call(app, method, path, Some(body), Some(session)).await
}

async fn create(app: &Router, path: &str, body: Value, session: &str) -> Value {
    let r = send(app, "POST", path, body, session).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    r.body
}

fn id(v: &Value) -> Uuid {
    v["id"].as_str().unwrap().parse().unwrap()
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|v| v[key].as_str().unwrap().to_owned())
        .collect()
}

fn fields(r: &Reply) -> &Value {
    &r.body["error"]["details"]["fields"]
}

#[tokio::test]
async fn only_admins_manage_the_catalog_and_every_account_reads_it() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let category = create(
        &app,
        "/api/categories",
        json!({ "name": "Hardware" }),
        &admin,
    )
    .await;
    let path = format!("/api/categories/{}", id(&category));

    for role in [Role::User, Role::Operator, Role::TeamLead] {
        let name = format!("{role:?}").to_lowercase();
        fixtures::account(&t.pool, role, &name).await;
        let s = login(&app, &name, PASSWORD).await.session();
        for (method, path, body) in [
            ("POST", "/api/categories", json!({ "name": "x" })),
            ("PATCH", path.as_str(), json!({ "archived": true })),
            ("PUT", "/api/categories/order", json!({ "ids": [] })),
            ("POST", "/api/priorities", json!({ "name": "x" })),
            ("PUT", "/api/priorities/order", json!({ "ids": [] })),
            (
                "POST",
                "/api/intake-fields",
                json!({ "label": "x", "kind": "text", "category_ids": [] }),
            ),
            ("PUT", "/api/intake-fields/order", json!({ "ids": [] })),
        ] {
            let r = send(&app, method, path, body, &s).await;
            assert_eq!(r.code(), "forbidden", "{role:?} {method} {path}");
        }
        let r = call(&app, "GET", "/api/catalog/all", None, Some(&s)).await;
        assert_eq!(r.code(), "forbidden");
        // The requester form's catalog is open to every role.
        let r = call(&app, "GET", "/api/catalog", None, Some(&s)).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(names(&r.body["categories"], "name"), ["Hardware"]);
    }
    let r = call(&app, "GET", "/api/catalog", None, None).await;
    assert_eq!(r.code(), "unauthorized");

    let audit: Vec<String> =
        sqlx::query_scalar("SELECT event_type FROM audit_events WHERE event_type LIKE 'catalog%'")
            .fetch_all(&t.pool)
            .await
            .unwrap();
    assert_eq!(
        audit,
        ["catalog_item_created"],
        "denied requests change nothing"
    );
}

#[tokio::test]
async fn categories_and_priorities_are_created_edited_reordered_and_archived() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;

    let hardware = create(
        &app,
        "/api/categories",
        json!({ "name": " Hardware " }),
        &admin,
    )
    .await;
    assert_eq!(hardware["name"], "Hardware");
    assert_eq!(
        hardware["skip_ai"], false,
        "new categories follow the AI policy"
    );
    let access = create(
        &app,
        "/api/categories",
        json!({ "name": "Access", "skip_ai": true }),
        &admin,
    )
    .await;
    assert_eq!(access["skip_ai"], true);

    let r = send(
        &app,
        "POST",
        "/api/categories",
        json!({ "name": "hardware" }),
        &admin,
    )
    .await;
    assert_eq!(*fields(&r), json!({ "name": ["taken"] }));
    let r = send(
        &app,
        "POST",
        "/api/categories",
        json!({ "name": "  " }),
        &admin,
    )
    .await;
    assert_eq!(*fields(&r), json!({ "name": ["required"] }));
    let long = "x".repeat(101);
    let r = send(
        &app,
        "POST",
        "/api/categories",
        json!({ "name": long }),
        &admin,
    )
    .await;
    assert_eq!(*fields(&r), json!({ "name": ["too_long"] }));
    let r = send(
        &app,
        "POST",
        "/api/categories",
        json!({ "name": "x", "team_id": null }),
        &admin,
    )
    .await;
    assert_eq!(r.code(), "unknown_field", "a category never selects a team");

    let hw = format!("/api/categories/{}", id(&hardware));
    let r = send(
        &app,
        "PATCH",
        &hw,
        json!({ "name": "Devices", "skip_ai": true }),
        &admin,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (&r.body["name"], &r.body["skip_ai"]),
        (&json!("Devices"), &json!(true))
    );
    let missing = format!("/api/categories/{}", Uuid::new_v4());
    let r = send(&app, "PATCH", &missing, json!({ "name": "x" }), &admin).await;
    assert_eq!(r.code(), "not_found");

    // Reorder needs exactly the current set; a stale list conflicts.
    let order = json!({ "ids": [id(&access), id(&hardware)] });
    let r = send(&app, "PUT", "/api/categories/order", order, &admin).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let r = send(
        &app,
        "PUT",
        "/api/categories/order",
        json!({ "ids": [id(&access)] }),
        &admin,
    )
    .await;
    assert_eq!(r.code(), "conflict");
    let dup = json!({ "ids": [id(&access), id(&access)] });
    assert_eq!(
        send(&app, "PUT", "/api/categories/order", dup, &admin)
            .await
            .code(),
        "conflict"
    );

    // Archive: gone from the form, still listed for Admins; the name is free again.
    let r = send(&app, "PATCH", &hw, json!({ "archived": true }), &admin).await;
    assert_eq!(r.body["archived"], true);
    let form = call(&app, "GET", "/api/catalog", None, Some(&admin)).await;
    assert_eq!(names(&form.body["categories"], "name"), ["Access"]);
    let all = call(&app, "GET", "/api/catalog/all", None, Some(&admin)).await;
    assert_eq!(
        names(&all.body["categories"], "name"),
        ["Access", "Devices"]
    );
    assert_eq!(all.body["categories"][1]["archived"], true);
    create(
        &app,
        "/api/categories",
        json!({ "name": "Devices" }),
        &admin,
    )
    .await;
    let r = send(&app, "PATCH", &hw, json!({ "archived": false }), &admin).await;
    assert_eq!(
        *fields(&r),
        json!({ "name": ["taken"] }),
        "restore cannot duplicate"
    );

    // Priorities: admin-defined levels and order.
    let low = create(&app, "/api/priorities", json!({ "name": "Low" }), &admin).await;
    let high = create(&app, "/api/priorities", json!({ "name": "High" }), &admin).await;
    let order = json!({ "ids": [id(&high), id(&low)] });
    let r = send(&app, "PUT", "/api/priorities/order", order, &admin).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let p = format!("/api/priorities/{}", id(&low));
    let r = send(
        &app,
        "PATCH",
        &p,
        json!({ "name": "Normal", "archived": true }),
        &admin,
    )
    .await;
    assert_eq!(
        (&r.body["name"], &r.body["archived"]),
        (&json!("Normal"), &json!(true))
    );
    let all = call(&app, "GET", "/api/catalog/all", None, Some(&admin)).await;
    assert_eq!(names(&all.body["priorities"], "name"), ["High", "Normal"]);
    let form = call(&app, "GET", "/api/catalog", None, Some(&admin)).await;
    assert_eq!(names(&form.body["priorities"], "name"), ["High"]);

    let audit: Vec<(String, Value)> = sqlx::query_as(
        "SELECT event_type, metadata FROM audit_events
         WHERE event_type LIKE 'catalog%' ORDER BY id",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    let admin_id = fixtures_admin_id(&t).await;
    let actors: Vec<Option<Uuid>> = sqlx::query_scalar(
        "SELECT actor_account_id FROM audit_events WHERE event_type LIKE 'catalog%'",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert!(actors.iter().all(|a| *a == Some(admin_id)));
    assert_eq!(audit[0].0, "catalog_item_created");
    assert_eq!(audit[0].1["kind"], "category");
    assert_eq!(audit[0].1["item"]["name"], "Hardware");
    assert!(
        audit
            .iter()
            .any(|(e, m)| e == "catalog_reordered" && m["kind"] == "priority")
    );
    assert!(
        audit
            .iter()
            .any(|(e, m)| e == "catalog_item_updated" && m["item"]["skip_ai"] == true)
    );
}

async fn fixtures_admin_id(t: &TestDb) -> Uuid {
    sqlx::query_scalar("SELECT id FROM accounts WHERE username = 'admin'")
        .fetch_one(&t.pool)
        .await
        .unwrap()
}

/// The software example: a "Program" select with known choices and a free-text alternative.
async fn software_catalog(app: &Router, admin: &str) -> (Uuid, Uuid, Value) {
    let software = create(app, "/api/categories", json!({ "name": "Software" }), admin).await;
    let normal = create(app, "/api/priorities", json!({ "name": "Normal" }), admin).await;
    let program = create(
        app,
        "/api/intake-fields",
        json!({
            "label": "Program",
            "guidance": "Which program is affected?",
            "kind": "select",
            "required": true,
            "allow_other": true,
            "category_ids": [id(&software)],
            "choices": [{ "label": "Word" }, { "label": "Excel" }],
        }),
        admin,
    )
    .await;
    (id(&software), id(&normal), program)
}

#[tokio::test]
async fn conditional_fields_keep_stable_choice_ids() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let (software, _, program) = software_catalog(&app, &admin).await;
    assert_eq!(program["kind"], "select");
    assert_eq!(program["category_ids"], json!([software]));
    assert_eq!(names(&program["choices"], "label"), ["Word", "Excel"]);
    let word = id(&program["choices"][0]);
    let excel = id(&program["choices"][1]);
    // Once omitted, Excel retains the same sort order as renamed Word. Make
    // its tie-breaker win so active-first ordering cannot pass by UUID luck.
    sqlx::query("UPDATE intake_field_choices SET created_at = created_at - interval '1 second' WHERE id = $1")
        .bind(excel)
        .execute(&t.pool)
        .await
        .unwrap();

    // Invalid definitions.
    let base = json!({ "label": "Version", "kind": "text", "category_ids": [software] });
    for (patch, expected) in [
        (
            json!({ "allow_other": true }),
            json!({ "allow_other": ["invalid"] }),
        ),
        (
            json!({ "choices": [{ "label": "a" }] }),
            json!({ "choices": ["invalid"] }),
        ),
        (
            json!({ "category_ids": [Uuid::new_v4()] }),
            json!({ "category_ids": ["invalid"] }),
        ),
        (
            json!({ "kind": "select" }),
            json!({ "choices": ["required"] }),
        ),
        (
            json!({ "kind": "select", "choices": [{ "label": "A" }, { "label": "a " }] }),
            json!({ "choices.1.label": ["taken"] }),
        ),
        (
            json!({ "kind": "select", "allow_other": true, "choices": [{ "id": word, "label": "A" }] }),
            json!({ "choices.0.id": ["invalid"] }),
        ),
        (
            json!({ "kind": "checkbox" }),
            json!({ "kind": ["invalid"] }),
        ),
    ] {
        let mut body = base.clone();
        body.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let r = send(&app, "POST", "/api/intake-fields", body, &admin).await;
        assert_eq!(*fields(&r), expected, "{}", r.body);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM intake_fields")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "failed creations roll back");

    // Rename Word, drop Excel (archived, not deleted), add Outlook, reorder.
    let path = format!("/api/intake-fields/{}", id(&program));
    let body = json!({ "choices": [{ "label": "Outlook" }, { "id": word, "label": "MS Word" }] });
    let r = send(&app, "PATCH", &path, body, &admin).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let choices = &r.body["choices"];
    assert_eq!(names(choices, "label"), ["Outlook", "MS Word", "Excel"]);
    assert_eq!(id(&choices[1]), word, "renaming keeps the ID");
    assert_eq!(
        (id(&choices[2]), &choices[2]["archived"]),
        (excel, &json!(true))
    );

    // The form sees only active choices; restoring brings Excel back with its ID.
    let form = call(&app, "GET", "/api/catalog", None, Some(&admin)).await;
    assert_eq!(
        names(&form.body["fields"][0]["choices"], "label"),
        ["Outlook", "MS Word"]
    );
    let body = json!({ "choices": [
        { "id": id(&choices[0]), "label": "Outlook" },
        { "id": word, "label": "MS Word" },
        { "id": excel, "label": "Excel" },
    ] });
    let r = send(&app, "PATCH", &path, body, &admin).await;
    assert_eq!(r.body["choices"][2]["archived"], false);

    // Without choices and without the free-text alternative a select is unanswerable.
    let body = json!({ "allow_other": false, "choices": [] });
    let r = send(&app, "PATCH", &path, body, &admin).await;
    assert_eq!(*fields(&r), json!({ "choices": ["required"] }));

    // A field whose only category is archived disappears from the form.
    let category = format!("/api/categories/{software}");
    send(
        &app,
        "PATCH",
        &category,
        json!({ "archived": true }),
        &admin,
    )
    .await;
    let form = call(&app, "GET", "/api/catalog", None, Some(&admin)).await;
    assert_eq!(form.body["fields"], json!([]));
    let all = call(&app, "GET", "/api/catalog/all", None, Some(&admin)).await;
    assert_eq!(all.body["fields"][0]["category_ids"], json!([software]));

    // Field order and archival.
    let other = create(
        &app,
        "/api/intake-fields",
        json!({ "label": "Asset tag", "kind": "text", "category_ids": [] }),
        &admin,
    )
    .await;
    let order = json!({ "ids": [id(&other), id(&program)] });
    let r = send(&app, "PUT", "/api/intake-fields/order", order, &admin).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(&app, "PATCH", &path, json!({ "archived": true }), &admin).await;
    assert_eq!(r.body["archived"], true);
    let all = call(&app, "GET", "/api/catalog/all", None, Some(&admin)).await;
    assert_eq!(
        names(&all.body["fields"], "label"),
        ["Asset tag", "Program"]
    );
}

fn answers(list: &[(Uuid, Value)]) -> BTreeMap<Uuid, IntakeAnswer> {
    list.iter()
        .map(|(id, v)| (*id, serde_json::from_value(v.clone()).unwrap()))
        .collect()
}

#[tokio::test]
async fn intake_validation_uses_only_active_visible_fields() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let (software, normal, program) = software_catalog(&app, &admin).await;
    let field = id(&program);
    let word = id(&program["choices"][0]);
    let excel = id(&program["choices"][1]);
    let hardware = id(&create(
        &app,
        "/api/categories",
        json!({ "name": "Hardware" }),
        &admin,
    )
    .await);
    let exempt = create(
        &app,
        "/api/categories",
        json!({ "name": "Password reset", "skip_ai": true }),
        &admin,
    )
    .await;
    let serial = create(
        &app,
        "/api/intake-fields",
        json!({ "label": "Serial number", "kind": "text", "category_ids": [hardware] }),
        &admin,
    )
    .await;
    let serial = id(&serial);
    let mut conn = t.pool.acquire().await.unwrap();
    let mut check = async |category, priority, list: &[(Uuid, Value)]| {
        validate_intake(&mut conn, category, priority, &answers(list)).await
    };
    let errors = |r: Result<_, tikkit_api::api::ApiError>| match r {
        Err(tikkit_api::api::ApiError::Validation(f)) => json!(f),
        other => panic!("expected validation_failed, got {other:?}"),
    };

    // A known program, and a custom program name.
    let s = check(software, normal, &[(field, json!({ "choice_id": word }))])
        .await
        .unwrap();
    assert_eq!(
        (
            s.category_name.as_str(),
            s.priority_name.as_str(),
            s.skip_ai
        ),
        ("Software", "Normal", false)
    );
    assert_eq!(s.answers.len(), 1);
    assert_eq!(
        (s.answers[0].label.as_str(), s.answers[0].value.as_str()),
        ("Program", "Word")
    );
    assert_eq!(s.answers[0].choice_id, Some(word));
    let s = check(
        software,
        normal,
        &[(field, json!({ "text": " Photoshop " }))],
    )
    .await
    .unwrap();
    assert_eq!(
        (s.answers[0].choice_id, s.answers[0].value.as_str()),
        (None, "Photoshop")
    );

    // Required while visible; answers to hidden fields are dropped, not stored.
    let e = errors(check(software, normal, &[(field, json!({ "text": "  " }))]).await);
    assert_eq!(e, json!({ format!("answers.{field}"): ["required"] }));
    let s = check(hardware, normal, &[(field, json!({ "choice_id": word }))])
        .await
        .unwrap();
    assert!(
        s.answers.is_empty(),
        "the program field is hidden for Hardware"
    );
    let s = check(
        software,
        normal,
        &[
            (field, json!({ "choice_id": word })),
            (serial, json!({ "text": "X1" })),
        ],
    )
    .await
    .unwrap();
    assert_eq!(s.answers.len(), 1, "serial number is hidden for Software");

    // Unknown and archived IDs.
    let unknown = Uuid::new_v4();
    let e = errors(check(unknown, unknown, &[(unknown, json!({ "text": "x" }))]).await);
    assert_eq!(
        e,
        json!({
            "category_id": ["invalid"],
            "priority_id": ["invalid"],
            format!("answers.{unknown}"): ["unknown_field"],
        })
    );
    let e = errors(check(software, normal, &[(field, json!({ "choice_id": serial }))]).await);
    assert_eq!(
        e,
        json!({ format!("answers.{field}"): ["invalid"] }),
        "choice of another field"
    );
    let e = errors(
        check(
            software,
            normal,
            &[(field, json!({ "choice_id": word, "text": "x" }))],
        )
        .await,
    );
    assert_eq!(e, json!({ format!("answers.{field}"): ["invalid"] }));
    let e = errors(check(hardware, normal, &[(serial, json!({ "choice_id": word }))]).await);
    assert_eq!(
        e,
        json!({ format!("answers.{serial}"): ["invalid"] }),
        "text fields take text"
    );
    let e = errors(
        check(
            hardware,
            normal,
            &[(serial, json!({ "text": "x".repeat(501) }))],
        )
        .await,
    );
    assert_eq!(e, json!({ format!("answers.{serial}"): ["too_long"] }));

    // Archive the Excel choice and disable the free-text alternative.
    let path = format!("/api/intake-fields/{field}");
    let body = json!({ "allow_other": false, "choices": [{ "id": word, "label": "Word" }] });
    assert_eq!(
        send(&app, "PATCH", &path, body, &admin).await.status,
        StatusCode::OK
    );
    let e = errors(check(software, normal, &[(field, json!({ "choice_id": excel }))]).await);
    assert_eq!(
        e,
        json!({ format!("answers.{field}"): ["invalid"] }),
        "archived choice"
    );
    let e = errors(check(software, normal, &[(field, json!({ "text": "Photoshop" }))]).await);
    assert_eq!(
        e,
        json!({ format!("answers.{field}"): ["invalid"] }),
        "no free text"
    );

    // An archived field is no longer required, and its answer is dropped.
    send(&app, "PATCH", &path, json!({ "archived": true }), &admin).await;
    let s = check(software, normal, &[(field, json!({ "choice_id": word }))])
        .await
        .unwrap();
    assert!(s.answers.is_empty());

    // "Skip AI automatically" travels with the snapshot, fixed at submission.
    let s = check(id(&exempt), normal, &[]).await.unwrap();
    assert!(s.skip_ai);

    // Archived category and priority are rejected for new tickets.
    send(
        &app,
        "PATCH",
        &format!("/api/categories/{software}"),
        json!({ "archived": true }),
        &admin,
    )
    .await;
    send(
        &app,
        "PATCH",
        &format!("/api/priorities/{normal}"),
        json!({ "archived": true }),
        &admin,
    )
    .await;
    let e = errors(check(software, normal, &[]).await);
    assert_eq!(
        e,
        json!({ "category_id": ["invalid"], "priority_id": ["invalid"] })
    );
}

#[tokio::test]
async fn archiving_keeps_existing_tickets_intact() {
    let Some(t) = TestDb::new().await else { return };
    let app = test_app(&t.pool);
    let admin = admin_session(&t, &app).await;
    let (software, normal, _) = software_catalog(&app, &admin).await;
    let requester = fixtures::account(&t.pool, Role::User, "requester").await;
    // Ticket endpoints arrive with #16/#17; the table and its foreign keys exist already.
    let ticket: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, category_id, priority_id, status, subject, description)
         VALUES ($1, $2, $3, 'queued', 'Word crashes', 'On start') RETURNING id",
    )
    .bind(requester)
    .bind(software)
    .bind(normal)
    .fetch_one(&t.pool)
    .await
    .unwrap();

    for path in [
        format!("/api/categories/{software}"),
        format!("/api/priorities/{normal}"),
    ] {
        let r = send(&app, "PATCH", &path, json!({ "archived": true }), &admin).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }
    let row: (String, String, bool) = sqlx::query_as(
        "SELECT c.name, p.name, c.archived_at IS NOT NULL FROM tickets t
         JOIN categories c ON c.id = t.category_id JOIN priorities p ON p.id = t.priority_id
         WHERE t.id = $1",
    )
    .bind(ticket)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(row, ("Software".into(), "Normal".into(), true));
    let form = call(&app, "GET", "/api/catalog", None, Some(&admin)).await;
    assert_eq!(form.body["categories"], json!([]));
    assert_eq!(form.body["priorities"], json!([]));
}
