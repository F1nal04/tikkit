//! Conversation privacy, append-only content, and serialized sends against PostgreSQL.
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

mod support;
use support::{
    TestDb, call,
    fixtures::{self, PASSWORD, Role},
    login, test_app,
};

struct World {
    db: TestDb,
    app: Router,
    requester: Uuid,
    op: Uuid,
    peer: Uuid,
    team: Uuid,
    other_team: Uuid,
    category: Uuid,
    priority: Uuid,
}

async fn world() -> Option<World> {
    let db = TestDb::new().await?;
    let p = &db.pool;
    fixtures::bootstrap_admin(p).await;
    let requester = fixtures::account(p, Role::User, "requester").await;
    let op = fixtures::account(p, Role::Operator, "op").await;
    let peer = fixtures::account(p, Role::Specialist, "peer").await;
    let lead = fixtures::account(p, Role::TeamLead, "lead").await;
    let other = fixtures::account(p, Role::TeamLead, "other").await;
    let team = fixtures::team(p, "Desk", lead).await.unwrap();
    let other_team = fixtures::team(p, "Network", other).await.unwrap();
    fixtures::member(p, team, op).await.unwrap();
    fixtures::member(p, team, peer).await.unwrap();
    let category =
        sqlx::query_scalar("INSERT INTO categories (name) VALUES ('Software') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    let priority =
        sqlx::query_scalar("INSERT INTO priorities (name) VALUES ('Normal') RETURNING id")
            .fetch_one(p)
            .await
            .unwrap();
    let app = test_app(p);
    Some(World {
        db,
        app,
        requester,
        op,
        peer,
        team,
        other_team,
        category,
        priority,
    })
}

impl World {
    async fn cookie(&self, name: &str) -> String {
        login(&self.app, name, PASSWORD).await.session()
    }
    async fn ticket(
        &self,
        requester: Uuid,
        status: &str,
        team: Option<Uuid>,
        assignee: Option<Uuid>,
    ) -> Uuid {
        sqlx::query_scalar("INSERT INTO tickets (requester_account_id, category_id, priority_id, status, team_id, assignee_account_id, subject, description, proposal_version, confirmation_deadline, resolution_source, closure_reason)
          VALUES ($1,$2,$3,$4,$5,$6,'Printer','Jammed', CASE WHEN $4 IN ('closed','awaiting_confirmation') THEN 1 ELSE 0 END,
          CASE WHEN $4 = 'awaiting_confirmation' THEN now() + interval '1 hour' END,
          CASE $4 WHEN 'closed' THEN 'operator' WHEN 'ai_solved' THEN 'ai' END,
          CASE WHEN $4 IN ('closed','ai_solved') THEN 'requester_confirmed' END) RETURNING id")
          .bind(requester).bind(self.category).bind(self.priority).bind(status).bind(team).bind(assignee).fetch_one(&self.db.pool).await.unwrap()
    }
    async fn send(
        &self,
        ticket: Uuid,
        endpoint: &str,
        cookie: &str,
        body: Value,
    ) -> support::Reply {
        call(
            &self.app,
            "POST",
            &format!("/api/tickets/{ticket}/{endpoint}"),
            Some(body),
            Some(cookie),
        )
        .await
    }
    async fn list(&self, ticket: Uuid, cookie: &str) -> support::Reply {
        call(
            &self.app,
            "GET",
            &format!("/api/tickets/{ticket}/messages"),
            None,
            Some(cookie),
        )
        .await
    }
    async fn pending(&self, uploader: Uuid) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO screenshots (id,uploader_account_id,filename,content_type,byte_size,width,height) VALUES ($1,$2,'secret.png','image/png',3,1,1)")
          .bind(id).bind(uploader).execute(&self.db.pool).await.unwrap();
        let dir = support::screenshot_dir(&self.db.pool);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(id.to_string()), [1, 2, 3]).unwrap();
        id
    }
}
fn body(text: &str) -> Value {
    json!({"idempotency_key": Uuid::new_v4(), "body": text})
}

#[tokio::test]
async fn private_notes_and_their_images_never_reach_any_requester_response() {
    let Some(w) = world().await else { return };
    let ticket = w
        .ticket(w.requester, "in_progress", Some(w.team), Some(w.op))
        .await;
    let requester = w.cookie("requester").await;
    let op = w.cookie("op").await;
    let public_image = w.pending(w.op).await;
    let private_image = w.pending(w.op).await;
    let mut public = body("Public answer");
    public["screenshot_ids"] = json!([public_image]);
    let r = w.send(ticket, "replies", &op, public).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let mut note = body("PRIVATE diagnostic secret");
    note["screenshot_ids"] = json!([private_image]);
    let r = w.send(ticket, "notes", &op, note).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let note_id = r.body["id"].as_str().unwrap().to_owned();
    for name in ["requester", "op", "lead", "admin", "other"] {
        let cookie = w.cookie(name).await;
        let r = w.list(ticket, &cookie).await;
        if name == "other" {
            assert_eq!(r.status, 404);
            continue;
        }
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(
            r.body["items"].as_array().unwrap().len(),
            if name == "requester" { 1 } else { 2 }
        );
    }
    let page = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{ticket}/messages?limit=1"),
        None,
        Some(&requester),
    )
    .await;
    assert_eq!(page.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        page.body["next_cursor"],
        Value::Null,
        "hidden notes must not create extra pages"
    );
    for uri in [
        format!("/api/tickets/{ticket}/messages"),
        format!("/api/tickets/{ticket}"),
        "/api/tickets".into(),
        "/api/ticket-drafts".into(),
    ] {
        let r = call(&w.app, "GET", &uri, None, Some(&requester)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let text = r.body.to_string();
        for secret in ["PRIVATE", &note_id, &private_image.to_string()] {
            assert!(!text.contains(secret), "{uri}: {text}");
        }
    }
    for (image, expected) in [(private_image, 404), (public_image, 200)] {
        for suffix in ["", "/preview", "/download"] {
            let uri = format!("/api/screenshots/{image}{suffix}");
            let r = call(&w.app, "GET", &uri, None, Some(&requester)).await;
            assert_eq!(r.status, expected, "{uri}: {}", r.body);
        }
    }
    let events: Vec<(String, Value)> = sqlx::query_as(
        "SELECT event_type,payload FROM ticket_events WHERE ticket_id=$1 ORDER BY id",
    )
    .bind(ticket)
    .fetch_all(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, "public_reply_added");
    assert_eq!(events[1].0, "internal_note_added");
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("diagnostic")
    );
    assert!(events[0].1.get("message_id").is_some());
    // Staff submitting their own ticket still get only the requester representation.
    let own = w.ticket(w.op, "queued", Some(w.team), None).await;
    let admin = w.cookie("admin").await;
    assert_eq!(
        w.send(own, "notes", &admin, body("PRIVATE own ticket"))
            .await
            .status,
        201
    );
    assert_eq!(w.list(own, &op).await.body["items"], json!([]));
}

#[tokio::test]
async fn staff_writes_follow_assignment_and_self_handling_rules_and_derive_authorship() {
    let Some(w) = world().await else { return };
    let claimed = w
        .ticket(w.requester, "in_progress", Some(w.team), Some(w.op))
        .await;
    let unclaimed = w.ticket(w.requester, "queued", Some(w.team), None).await;
    let general = w.ticket(w.requester, "queued", None, None).await;
    for (name, expected) in [
        ("op", 201),
        ("peer", 403),
        ("lead", 201),
        ("admin", 201),
        ("other", 404),
    ] {
        let cookie = w.cookie(name).await;
        for endpoint in ["replies", "notes"] {
            assert_eq!(
                w.send(claimed, endpoint, &cookie, body("answer"))
                    .await
                    .status,
                expected,
                "{name} {endpoint}"
            );
            assert_eq!(
                w.send(unclaimed, endpoint, &cookie, body("answer"))
                    .await
                    .status,
                if name == "other" { 404 } else { 201 }
            );
            assert_eq!(
                w.send(general, endpoint, &cookie, body("answer"))
                    .await
                    .status,
                403
            );
        }
    }
    for name in ["op", "peer", "lead", "admin"] {
        let cookie = w.cookie(name).await;
        let id: Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE username=$1")
            .bind(name)
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
        let own = w.ticket(id, "queued", Some(w.team), None).await;
        assert_eq!(
            w.send(own, "notes", &cookie, body("note")).await.status,
            403
        );
        let r = w.send(own, "replies", &cookie, body("as requester")).await;
        assert_eq!(r.status, 201, "{}", r.body);
        assert_eq!(r.body["author_kind"], "requester");
        let kind: String =
            sqlx::query_scalar("SELECT author_kind FROM ticket_messages WHERE ticket_id=$1")
                .bind(own)
                .fetch_one(&w.db.pool)
                .await
                .unwrap();
        assert_eq!(kind, "requester");
        for key in [
            "author_account_id",
            "author_kind",
            "kind",
            "role",
            "visibility",
        ] {
            let mut forged = body("forged");
            forged[key] = json!("admin");
            assert_eq!(
                w.send(own, "replies", &cookie, forged).await.code(),
                "unknown_field"
            );
        }
    }
    let requester = w.cookie("requester").await;
    let someone_else = w.ticket(w.peer, "queued", None, None).await;
    assert_eq!(
        w.send(someone_else, "replies", &requester, body("no"))
            .await
            .status,
        404
    );
    assert_eq!(w.list(someone_else, &requester).await.status, 404);
    assert_eq!(
        w.send(claimed, "notes", &requester, body("no"))
            .await
            .status,
        403
    );
}

#[tokio::test]
async fn plain_text_is_escaped_and_messages_and_images_are_append_only() {
    let Some(w) = world().await else { return };
    let cookie = w.cookie("requester").await;
    let ticket = w.ticket(w.requester, "queued", None, None).await;
    let image = w.pending(w.requester).await;
    let xss = "<script>alert('x')</script><img src=x onerror=alert(1)> & \"quoted\" [link](javascript:alert(1))";
    let mut input = body(xss);
    input["screenshot_ids"] = json!([image]);
    let r = w.send(ticket, "replies", &cookie, input).await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.body["body"], xss);
    let html = r.body["body_html"].as_str().unwrap();
    assert!(!html.contains('<'));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&amp;"));
    assert!(html.contains("&quot;quoted&quot;"));
    assert!(!html.contains("<a"));
    let stored: String = sqlx::query_scalar("SELECT body FROM ticket_messages WHERE ticket_id=$1")
        .bind(ticket)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(stored, xss);
    let id = r.body["id"].as_str().unwrap();
    for method in ["PATCH", "DELETE"] {
        assert_eq!(
            call(
                &w.app,
                method,
                &format!("/api/tickets/{ticket}/messages/{id}"),
                Some(json!({"body":"rewrite"})),
                Some(&cookie)
            )
            .await
            .status,
            404
        );
    }
    assert_eq!(
        call(
            &w.app,
            "DELETE",
            &format!("/api/screenshots/{image}"),
            None,
            Some(&cookie)
        )
        .await
        .status,
        409
    );
    for sql in [
        "UPDATE ticket_messages SET body='changed'",
        "DELETE FROM ticket_messages",
        "TRUNCATE ticket_messages CASCADE",
    ] {
        let e = sqlx::query(sql).execute(&w.db.pool).await.unwrap_err();
        assert_eq!(e.as_database_error().unwrap().code().unwrap(), "23001");
    }
    assert_eq!(w.list(ticket, &cookie).await.body["items"][0]["body"], xss);
}

#[tokio::test]
async fn retries_are_atomic_concurrent_sends_have_commit_order_and_pages_never_skip() {
    let Some(w) = world().await else { return };
    let cookie = w.cookie("requester").await;
    let ticket = w
        .ticket(
            w.requester,
            "waiting_for_requester",
            Some(w.team),
            Some(w.op),
        )
        .await;
    let image = w.pending(w.requester).await;
    let mut input = body("retry me");
    input["screenshot_ids"] = json!([image]);
    let (a, b) = tokio::join!(
        w.send(ticket, "replies", &cookie, input.clone()),
        w.send(ticket, "replies", &cookie, input.clone())
    );
    assert_eq!(a.status, 201, "{}", a.body);
    assert_eq!(b.status, 201, "{}", b.body);
    assert_eq!(a.body, b.body);
    input["body"] = json!("different");
    assert_eq!(w.send(ticket, "replies", &cookie, input).await.status, 409);
    let mut jobs = Vec::new();
    for n in 0..12 {
        let app = w.app.clone();
        let cookie = cookie.clone();
        jobs.push(tokio::spawn(async move {
            call(
                &app,
                "POST",
                &format!("/api/tickets/{ticket}/replies"),
                Some(body(&format!("parallel {n}"))),
                Some(&cookie),
            )
            .await
        }));
    }
    for j in jobs {
        let r = j.await.unwrap();
        assert_eq!(r.status, 201, "{}", r.body);
    }
    let mut uri = format!("/api/tickets/{ticket}/messages?limit=2");
    let mut ids = Vec::new();
    let mut last = 0;
    loop {
        let r = call(&w.app, "GET", &uri, None, Some(&cookie)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        for m in r.body["items"].as_array().unwrap() {
            let seq = m["sequence"].as_i64().unwrap();
            assert!(seq > last);
            last = seq;
            ids.push(m["id"].as_str().unwrap().to_owned());
        }
        match r.body["next_cursor"].as_str() {
            Some(c) => uri = format!("/api/tickets/{ticket}/messages?limit=2&cursor={c}"),
            None => break,
        }
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 13);
    let transitions: i64=sqlx::query_scalar("SELECT count(*) FROM ticket_events WHERE ticket_id=$1 AND event_type='requester_responded'").bind(ticket).fetch_one(&w.db.pool).await.unwrap();
    assert_eq!(transitions, 1);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ticket_events WHERE ticket_id=$1 AND event_type='public_reply_added'",
    )
    .bind(ticket)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(events, 13);
    // Retry after a later lifecycle change remains a receipt, never a second write.
    let input = body("last reply");
    let a = w.send(ticket, "replies", &cookie, input.clone()).await;
    let mut tx = w.db.pool.begin().await.unwrap();
    tikkit_api::lifecycle::transition(
        &mut tx,
        ticket,
        tikkit_api::lifecycle::By::Account(w.requester),
        &tikkit_api::lifecycle::Transition::Cancel {
            reason: "done".into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(w.send(ticket, "replies", &cookie, input).await.body, a.body);
}

#[tokio::test]
async fn closed_ai_and_draft_tickets_reject_content_and_validation_rolls_back() {
    let Some(w) = world().await else { return };
    let requester = w.cookie("requester").await;
    let admin = w.cookie("admin").await;
    for status in [
        "draft",
        "ai_processing",
        "awaiting_ai_decision",
        "closed",
        "cancelled",
        "ai_solved",
    ] {
        let ticket = w.ticket(w.requester, status, None, None).await;
        assert_eq!(
            w.send(ticket, "replies", &requester, body("no"))
                .await
                .status,
            409,
            "{status}"
        );
        for endpoint in ["replies", "notes"] {
            let r = w.send(ticket, endpoint, &admin, body("no")).await;
            assert!(r.status == 403 || r.status == 404, "{status}: {}", r.body);
        }
    }
    for status in [
        "queued",
        "in_progress",
        "waiting_for_requester",
        "awaiting_confirmation",
    ] {
        let ticket = w
            .ticket(w.requester, status, Some(w.team), Some(w.op))
            .await;
        assert_eq!(
            w.send(ticket, "replies", &requester, body("yes"))
                .await
                .status,
            201
        );
        let got: String = sqlx::query_scalar("SELECT status::text FROM tickets WHERE id=$1")
            .bind(ticket)
            .fetch_one(&w.db.pool)
            .await
            .unwrap();
        assert_eq!(
            got,
            if status == "waiting_for_requester" {
                "in_progress"
            } else {
                status
            }
        );
    }
    let ticket = w.ticket(w.requester, "queued", None, None).await;
    for (text, code) in [(" \n\t", "required"), (&"é".repeat(10001), "too_long")] {
        let r = w.send(ticket, "replies", &requester, body(text)).await;
        assert_eq!(r.status, 422);
        assert_eq!(r.body["error"]["details"]["fields"]["body"][0], code);
    }
    assert_eq!(
        w.send(ticket, "replies", &requester, body(&"é".repeat(10000)))
            .await
            .status,
        201
    );
    let image = w.pending(w.op).await;
    let mut invalid = body("cannot attach another account's image");
    invalid["screenshot_ids"] = json!([image]);
    assert_eq!(
        w.send(ticket, "replies", &requester, invalid).await.status,
        422
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ticket_messages WHERE ticket_id=$1")
        .bind(ticket)
        .fetch_one(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let r = call(
        &w.app,
        "GET",
        &format!("/api/tickets/{ticket}/messages?cursor=nonsense"),
        None,
        Some(&requester),
    )
    .await;
    assert_eq!(r.status, 422);
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/tickets/{ticket}/replies"))
        .header("cookie", &requester)
        .header("content-type", "application/json")
        .body(Body::from(body("csrf").to_string()))
        .unwrap();
    assert_eq!(w.app.clone().oneshot(request).await.unwrap().status(), 403);
}

#[tokio::test]
async fn transfer_and_membership_removal_revoke_conversations_and_note_images_immediately() {
    let Some(w) = world().await else { return };
    let ticket = w
        .ticket(w.requester, "in_progress", Some(w.team), Some(w.op))
        .await;
    let op = w.cookie("op").await;
    let peer = w.cookie("peer").await;
    let other = w.cookie("other").await;
    let image = w.pending(w.op).await;
    let mut input = body("private diagnostics");
    input["screenshot_ids"] = json!([image]);
    assert_eq!(
        w.send(ticket, "notes", &op, input.clone()).await.status,
        201
    );
    sqlx::query("DELETE FROM team_memberships WHERE account_id=$1")
        .bind(w.peer)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(w.list(ticket, &peer).await.status, 404);
    for suffix in ["", "/preview", "/download"] {
        assert_eq!(
            call(
                &w.app,
                "GET",
                &format!("/api/screenshots/{image}{suffix}"),
                None,
                Some(&peer)
            )
            .await
            .status,
            404
        );
    }
    sqlx::query("UPDATE tickets SET team_id=$2,assignee_account_id=NULL WHERE id=$1")
        .bind(ticket)
        .bind(w.other_team)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(w.list(ticket, &op).await.status, 404);
    assert_eq!(w.send(ticket, "notes", &op, input).await.status, 404); // receipt also rechecks visibility
    assert_eq!(
        w.list(ticket, &other).await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for (cookie, expected) in [(&op, 404), (&other, 200)] {
        for suffix in ["", "/preview", "/download"] {
            assert_eq!(
                call(
                    &w.app,
                    "GET",
                    &format!("/api/screenshots/{image}{suffix}"),
                    None,
                    Some(cookie)
                )
                .await
                .status,
                expected
            );
        }
    }
    // Returning a ticket to shared intake cannot expose historical notes there.
    sqlx::query("UPDATE tickets SET team_id=NULL WHERE id=$1")
        .bind(ticket)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert_eq!(w.list(ticket, &op).await.body["items"], json!([]));
    assert_eq!(
        call(
            &w.app,
            "GET",
            &format!("/api/screenshots/{image}"),
            None,
            Some(&op)
        )
        .await
        .status,
        404
    );
}

#[tokio::test]
async fn private_conversation_responses_cannot_be_cached_after_access_revocation() {
    let Some(w) = world().await else { return };
    let ticket = w.ticket(w.requester, "queued", Some(w.team), None).await;
    let op = w.cookie("op").await;
    let image = w.pending(w.op).await;
    let mut input = body("private diagnostic");
    input["screenshot_ids"] = json!([image]);
    let r = w.send(ticket, "notes", &op, input).await;
    assert_eq!(r.status, 201);
    for uri in [
        format!("/api/tickets/{ticket}/messages"),
        format!("/api/screenshots/{image}"),
        format!("/api/screenshots/{image}/preview"),
        format!("/api/screenshots/{image}/download"),
    ] {
        let request = Request::builder()
            .uri(uri)
            .header("cookie", &op)
            .body(Body::empty())
            .unwrap();
        let response = w.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("private, no-store")
        );
    }
    for endpoint in ["replies", "notes"] {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/tickets/{ticket}/{endpoint}"))
            .header("cookie", &op)
            .header("content-type", "application/json")
            .header("x-requested-with", "tikkit")
            .body(Body::from(body("new message").to_string()))
            .unwrap();
        let response = w.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 201);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("private, no-store")
        );
    }
}

#[tokio::test]
async fn note_image_access_cannot_combine_old_ticket_team_with_new_membership() {
    let Some(w) = world().await else { return };
    let ticket = w
        .ticket(w.requester, "in_progress", Some(w.team), Some(w.op))
        .await;
    let op = w.cookie("op").await;
    let peer = w.cookie("peer").await;
    let image = w.pending(w.op).await;
    let mut input = body("private diagnostic");
    input["screenshot_ids"] = json!([image]);
    assert_eq!(w.send(ticket, "notes", &op, input).await.status, 201);
    sqlx::query("UPDATE team_memberships SET team_id=$2 WHERE account_id=$1")
        .bind(w.peer)
        .bind(w.other_team)
        .execute(&w.db.pool)
        .await
        .unwrap();
    let mut change = w.db.pool.begin().await.unwrap();
    // Pause the image request between reading ticket scope and loading actor team.
    sqlx::query("LOCK TABLE team_memberships IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *change)
        .await
        .unwrap();
    sqlx::query("UPDATE tickets SET team_id=$2,assignee_account_id=NULL WHERE id=$1")
        .bind(ticket)
        .bind(w.other_team)
        .execute(&mut *change)
        .await
        .unwrap();
    sqlx::query("UPDATE team_memberships SET team_id=$2 WHERE account_id=$1")
        .bind(w.peer)
        .bind(w.team)
        .execute(&mut *change)
        .await
        .unwrap();
    let app = w.app.clone();
    let read = tokio::spawn(async move {
        call(
            &app,
            "GET",
            &format!("/api/screenshots/{image}/download"),
            None,
            Some(&peer),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FROM team_memberships m JOIN teams%')")
                .fetch_one(&w.db.pool).await.unwrap();
            if blocked { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("image request blocked while loading its actor");
    // Atomic swap: neither before nor after commit can the caller see this ticket.
    change.commit().await.unwrap();
    assert_eq!(read.await.unwrap().status, 404);
}
