//! PostgreSQL persistence and schema invariants (issue #6).

mod common;

use common::*;
use sqlx::{Connection, PgConnection, postgres::PgPoolOptions};
use tikkit_api::db;
use uuid::Uuid;

#[tokio::test]
async fn migrates_an_empty_database() {
    let test = TestDb::empty().await;
    let before = db::schema_status(&test.pool).await.unwrap();
    assert!(before.applied.is_empty());
    assert!(!before.pending.is_empty());

    let after = db::prepare_schema(&test.pool, true).await.unwrap();
    assert!(after.is_current(), "{after:?}");
    assert_eq!(after.applied, before.pending);

    // Running again is a no-op.
    let again = db::migrate(&test.pool).await.unwrap();
    assert_eq!(again, after);

    for table in [
        "organization_settings",
        "accounts",
        "teams",
        "team_memberships",
        "categories",
        "priorities",
        "tickets",
        "ticket_messages",
        "attachments",
        "ticket_events",
        "audit_events",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&test.pool)
            .await
            .unwrap();
        assert!(exists, "table {table} missing");
    }
    test.cleanup().await;
}

#[tokio::test]
async fn organization_settings_singleton_has_spec_defaults() {
    let test = TestDb::new().await;
    let row: (
        bool,
        bool,
        String,
        i32,
        String,
        Option<time::OffsetDateTime>,
    ) = sqlx::query_as(
        "SELECT allow_requester_skip_ai, reopen_closed_tickets_enabled, cancellation_policy, \
                resolution_confirmation_timeout_hours, default_language, bootstrap_completed_at \
         FROM organization_settings",
    )
    .fetch_one(&test.pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            false,
            false,
            "anytime_while_open".to_owned(),
            168,
            "en".to_owned(),
            None
        )
    );

    let second = sqlx::query("INSERT INTO organization_settings (id) VALUES (true)")
        .execute(&test.pool)
        .await;
    assert_constraint(second, "organization_settings_pkey");
    let deleted = sqlx::query("DELETE FROM organization_settings")
        .execute(&test.pool)
        .await;
    assert_constraint(deleted, "organization_settings_singleton");
    let zero =
        sqlx::query("UPDATE organization_settings SET resolution_confirmation_timeout_hours = 0")
            .execute(&test.pool)
            .await;
    assert_constraint(zero, "organization_settings_timeout_positive");
}

#[tokio::test]
async fn first_setup_completes_exactly_once() {
    let test = TestDb::new().await;
    let admin = complete_first_setup(&test.pool).await;
    assert_eq!(admin.role, "admin");

    // A second bootstrap attempt (e.g. after a restart) finds nothing to update...
    let updated = sqlx::query(
        "UPDATE organization_settings SET bootstrap_completed_at = now() WHERE bootstrap_completed_at IS NULL",
    )
    .execute(&test.pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(updated, 0);
    // ...and the database refuses to undo or rewrite the bootstrap record.
    let reset = sqlx::query(
        "UPDATE organization_settings SET bootstrap_completed_at = NULL, bootstrap_admin_account_id = NULL",
    )
    .execute(&test.pool)
    .await;
    assert_constraint(reset, "organization_settings_bootstrap_once");
}

#[tokio::test]
async fn fixtures_cover_all_five_roles() {
    let test = TestDb::new().await;
    let fixtures = RoleFixtures::create(&test.pool).await;
    for role in Role::ALL {
        let account = fixtures.by_role(role);
        let stored: String = sqlx::query_scalar("SELECT role FROM accounts WHERE id = $1")
            .bind(account.id)
            .fetch_one(&test.pool)
            .await
            .unwrap();
        assert_eq!(stored, role.as_str());
    }
    let lead: (Uuid, bool) = sqlx::query_as(
        "SELECT team_id, is_lead FROM team_memberships WHERE account_id = $1 AND ended_at IS NULL",
    )
    .bind(fixtures.team_lead.id)
    .fetch_one(&test.pool)
    .await
    .unwrap();
    assert_eq!(lead, (fixtures.team.id, true));
    let members: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM team_memberships WHERE team_id = $1 AND ended_at IS NULL",
    )
    .bind(fixtures.team.id)
    .fetch_one(&test.pool)
    .await
    .unwrap();
    assert_eq!(members, 3);
}

#[tokio::test]
async fn records_persist_across_restart() {
    let test = TestDb::new().await;
    let requester = create_account(&test.pool, Role::User, "Alice", "alice@example.test").await;
    let (ticket_id, reference) = create_queued_ticket(&test.pool, &requester).await;

    // Simulate an API restart: close every connection and start a new pool.
    test.pool.close().await;
    let restarted = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(test.connect_options())
        .await
        .unwrap();
    let status = db::prepare_schema(&restarted, false).await.unwrap();
    assert!(status.is_current(), "restart must not need migrations");

    let row: (Uuid, i64, String, String, time::OffsetDateTime) = sqlx::query_as(
        "SELECT t.id, t.reference, t.status, a.username, t.submitted_at \
         FROM tickets t JOIN accounts a ON a.id = t.requester_account_id WHERE t.id = $1",
    )
    .bind(ticket_id)
    .fetch_one(&restarted)
    .await
    .unwrap();
    assert_eq!(row.0, ticket_id);
    assert_eq!(row.1, reference);
    assert_eq!(row.2, "queued");
    assert_eq!(row.3, "Alice");
    assert_eq!(row.4.offset(), time::UtcOffset::UTC, "timestamps are UTC");
    restarted.close().await;
}

#[tokio::test]
async fn ticket_references_are_stable_and_unique() {
    let test = TestDb::new().await;
    let requester = create_account(&test.pool, Role::User, "bob", "bob@example.test").await;
    let (first_id, first) = create_queued_ticket(&test.pool, &requester).await;
    let (_, second) = create_queued_ticket(&test.pool, &requester).await;
    assert!(second > first);

    let rewrite = sqlx::query("UPDATE tickets SET reference = 999 WHERE id = $1")
        .bind(first_id)
        .execute(&test.pool)
        .await;
    assert!(rewrite.is_err(), "identity column cannot be overwritten");
    let requester2 = create_account(&test.pool, Role::User, "carol", "carol@example.test").await;
    let reassign = sqlx::query("UPDATE tickets SET requester_account_id = $2 WHERE id = $1")
        .bind(first_id)
        .bind(requester2.id)
        .execute(&test.pool)
        .await;
    assert_constraint(reassign, "tickets_identity_immutable");
}

#[tokio::test]
async fn foreign_keys_are_enforced() {
    let test = TestDb::new().await;
    let missing = Uuid::now_v7();
    let ticket = sqlx::query("INSERT INTO tickets (requester_account_id) VALUES ($1)")
        .bind(missing)
        .execute(&test.pool)
        .await;
    assert_constraint(ticket, "tickets_requester_account_id_fkey");

    let membership = add_membership(&test.pool, missing, missing, false).await;
    assert!(membership.is_err());

    let event = sqlx::query(
        "INSERT INTO ticket_events (ticket_id, actor_kind, event_type) VALUES ($1, 'system', 'ticket.created')",
    )
    .bind(missing)
    .execute(&test.pool)
    .await;
    assert_constraint(event, "ticket_events_ticket_id_fkey");
}

#[tokio::test]
async fn rolled_back_transaction_leaves_no_rows() {
    let test = TestDb::new().await;
    let mut tx = test.pool.begin().await.unwrap();
    let account = create_account(&mut *tx, Role::Operator, "ghost", "ghost@example.test").await;
    sqlx::query(
        "INSERT INTO audit_events (actor_kind, actor_account_id, action) VALUES ('account', $1, 'account.created')",
    )
    .bind(account.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!((accounts, audits), (0, 0));

    // A failing statement aborts the whole transaction.
    let mut tx = test.pool.begin().await.unwrap();
    create_account(&mut *tx, Role::User, "dup", "a@example.test").await;
    let duplicate = sqlx::query(
        "INSERT INTO accounts (username, username_normalized, email, display_name, password_hash) \
         VALUES ('DUP', 'dup', 'b@example.test', 'Dup', 'x')",
    )
    .execute(&mut *tx)
    .await;
    assert_constraint(duplicate, "accounts_username_normalized_key");
    drop(tx); // implicit rollback
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 0);
}

#[tokio::test]
async fn usernames_unique_when_normalized_but_emails_may_be_shared() {
    let test = TestDb::new().await;
    let shared = "it-person@example.test";
    let admin = create_account(&test.pool, Role::Admin, "Pat.Admin", shared).await;
    let lead = create_account(&test.pool, Role::TeamLead, "pat.lead", shared).await;
    let user = create_account(&test.pool, Role::User, "pat", shared).await;
    assert_ne!(admin.id, lead.id);
    assert_ne!(lead.id, user.id);
    // The lead can still lead a team even though an Admin shares the email.
    create_team_with_lead(&test.pool, "Infrastructure", &lead).await;

    let same_email_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE lower(email) = lower($1)")
            .bind(shared)
            .fetch_one(&test.pool)
            .await
            .unwrap();
    assert_eq!(same_email_count, 3);

    let duplicate = sqlx::query(
        "INSERT INTO accounts (username, username_normalized, email, display_name, password_hash) \
         VALUES ('PAT.ADMIN', 'pat.admin', 'other@example.test', 'Other', 'x')",
    )
    .execute(&test.pool)
    .await;
    assert_constraint(duplicate, "accounts_username_normalized_key");

    let invalid = sqlx::query(
        "INSERT INTO accounts (username, username_normalized, email, display_name, password_hash) \
         VALUES ('Has Space', 'has space', 'x@example.test', 'X', 'x')",
    )
    .execute(&test.pool)
    .await;
    assert_constraint(invalid, "accounts_username_normalized_check");
}

#[tokio::test]
async fn concurrent_duplicate_usernames_have_one_winner() {
    let test = TestDb::new().await;
    let attempts = (0..4).map(|i| {
        let pool = test.pool.clone();
        tokio::spawn(async move {
            sqlx::query(
                "INSERT INTO accounts (username, username_normalized, email, display_name, password_hash) \
                 VALUES ('Racer', 'racer', $1, 'Racer', 'x')",
            )
            .bind(format!("racer{i}@example.test"))
            .execute(&pool)
            .await
        })
    });
    let mut successes = 0;
    for attempt in attempts.collect::<Vec<_>>() {
        if attempt.await.unwrap().is_ok() {
            successes += 1;
        }
    }
    assert_eq!(successes, 1);
}

#[tokio::test]
async fn account_ids_are_immutable() {
    let test = TestDb::new().await;
    let account = create_account(&test.pool, Role::User, "fixed", "f@example.test").await;
    let changed = sqlx::query("UPDATE accounts SET id = $2 WHERE id = $1")
        .bind(account.id)
        .bind(Uuid::now_v7())
        .execute(&test.pool)
        .await;
    assert_constraint(changed, "accounts_id_immutable");
}

#[tokio::test]
async fn ticket_and_audit_events_are_append_only() {
    let test = TestDb::new().await;
    let requester = create_account(&test.pool, Role::User, "req", "req@example.test").await;
    let (ticket_id, _) = create_queued_ticket(&test.pool, &requester).await;
    sqlx::query(
        "INSERT INTO ticket_events (ticket_id, actor_account_id, actor_kind, event_type, from_status, to_status) \
         VALUES ($1, $2, 'user', 'ticket.submitted', 'draft', 'queued')",
    )
    .bind(ticket_id)
    .bind(requester.id)
    .execute(&test.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO audit_events (actor_kind, actor_account_id, action, target_type, target_id, request_id) \
         VALUES ('account', $1, 'ticket.submitted', 'ticket', $2, 'req-1')",
    )
    .bind(requester.id)
    .bind(ticket_id.to_string())
    .execute(&test.pool)
    .await
    .unwrap();

    for (table, constraint) in [
        ("ticket_events", "ticket_events_append_only"),
        ("audit_events", "audit_events_append_only"),
    ] {
        let column = if table == "ticket_events" {
            "event_type"
        } else {
            "action"
        };
        let update = sqlx::query(&format!("UPDATE {table} SET {column} = 'tampered'"))
            .execute(&test.pool)
            .await;
        assert_constraint(update, constraint);
        let delete = sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&test.pool)
            .await;
        assert_constraint(delete, constraint);
        let truncate = sqlx::query(&format!("TRUNCATE {table}"))
            .execute(&test.pool)
            .await;
        assert_constraint(truncate, constraint);
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&test.pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "{table} rows must survive");
    }

    // Messages are not rewritten either.
    sqlx::query(
        "INSERT INTO ticket_messages (ticket_id, author_account_id, visibility, body) VALUES ($1, $2, 'public', 'hello')",
    )
    .bind(ticket_id)
    .bind(requester.id)
    .execute(&test.pool)
    .await
    .unwrap();
    let edit = sqlx::query("UPDATE ticket_messages SET body = 'rewritten'")
        .execute(&test.pool)
        .await;
    assert_constraint(edit, "ticket_messages_append_only");
}

#[tokio::test]
async fn submitted_intake_is_frozen_and_ai_solved_is_final() {
    let test = TestDb::new().await;
    let requester = create_account(&test.pool, Role::User, "req", "req@example.test").await;
    let (ticket_id, _) = create_queued_ticket(&test.pool, &requester).await;

    let edit = sqlx::query("UPDATE tickets SET description = 'changed' WHERE id = $1")
        .bind(ticket_id)
        .execute(&test.pool)
        .await;
    assert_constraint(edit, "tickets_intake_frozen");

    // Drafts stay editable.
    let draft: Uuid = sqlx::query_scalar(
        "INSERT INTO tickets (requester_account_id, subject) VALUES ($1, 'draft') RETURNING id",
    )
    .bind(requester.id)
    .fetch_one(&test.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tickets SET subject = 'better subject' WHERE id = $1")
        .bind(draft)
        .execute(&test.pool)
        .await
        .unwrap();
    // A draft cannot be submitted without complete intake.
    let incomplete =
        sqlx::query("UPDATE tickets SET status = 'queued', submitted_at = now() WHERE id = $1")
            .bind(draft)
            .execute(&test.pool)
            .await;
    assert_constraint(incomplete, "tickets_submitted_intake_complete");

    // AI-solved: closed with resolution ai_confirmed, never routed, then final.
    sqlx::query("UPDATE tickets SET status = 'awaiting_ai_decision' WHERE id = $1")
        .bind(ticket_id)
        .execute(&test.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tickets SET status = 'closed', resolution_kind = 'ai_confirmed', closed_at = now() WHERE id = $1",
    )
    .bind(ticket_id)
    .execute(&test.pool)
    .await
    .unwrap();
    let reopen = sqlx::query(
        "UPDATE tickets SET status = 'queued', resolution_kind = NULL, closed_at = NULL WHERE id = $1",
    )
    .bind(ticket_id)
    .execute(&test.pool)
    .await;
    assert_constraint(reopen, "tickets_ai_solved_final");
    let reply = sqlx::query(
        "INSERT INTO ticket_messages (ticket_id, author_account_id, visibility, body) VALUES ($1, $2, 'public', 'hi')",
    )
    .bind(ticket_id)
    .bind(requester.id)
    .execute(&test.pool)
    .await;
    assert_constraint(reply, "ticket_messages_ticket_open");

    // Closed requires a resolution kind; an unknown status is rejected.
    let (other, _) = create_queued_ticket(&test.pool, &requester).await;
    let no_kind =
        sqlx::query("UPDATE tickets SET status = 'closed', closed_at = now() WHERE id = $1")
            .bind(other)
            .execute(&test.pool)
            .await;
    assert_constraint(no_kind, "tickets_closed_has_resolution");
    let bogus = sqlx::query("UPDATE tickets SET status = 'resolved' WHERE id = $1")
        .bind(other)
        .execute(&test.pool)
        .await;
    assert_constraint(bogus, "tickets_status_check");
}

#[tokio::test]
async fn assignees_must_be_current_team_members_and_not_the_requester() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let (ticket_id, _) = create_queued_ticket(&test.pool, &f.user).await;

    // Assignment without a team is rejected.
    let no_team = sqlx::query("UPDATE tickets SET assignee_account_id = $2 WHERE id = $1")
        .bind(ticket_id)
        .bind(f.operator.id)
        .execute(&test.pool)
        .await;
    assert_constraint(no_team, "tickets_assignee_requires_team");
    // Admins are never team members, so never assignees.
    let admin =
        sqlx::query("UPDATE tickets SET team_id = $2, assignee_account_id = $3 WHERE id = $1")
            .bind(ticket_id)
            .bind(f.team.id)
            .bind(f.admin.id)
            .execute(&test.pool)
            .await;
    assert_constraint(admin, "tickets_assignee_team_member");
    sqlx::query("UPDATE tickets SET team_id = $2, assignee_account_id = $3, status = 'in_progress' WHERE id = $1")
        .bind(ticket_id)
        .bind(f.team.id)
        .bind(f.operator.id)
        .execute(&test.pool)
        .await
        .unwrap();

    // The requester can never be assigned their own ticket.
    let (own, _) = create_queued_ticket(&test.pool, &f.operator).await;
    let selfish =
        sqlx::query("UPDATE tickets SET team_id = $2, assignee_account_id = $3 WHERE id = $1")
            .bind(own)
            .bind(f.team.id)
            .bind(f.operator.id)
            .execute(&test.pool)
            .await;
    assert_constraint(selfish, "tickets_assignee_not_requester");
}

#[tokio::test]
async fn attachments_accept_only_screenshot_metadata() {
    let test = TestDb::new().await;
    let requester = create_account(&test.pool, Role::User, "req", "req@example.test").await;
    let (ticket_id, _) = create_queued_ticket(&test.pool, &requester).await;
    let insert = |content_type: &'static str, key: &'static str| {
        sqlx::query(
            "INSERT INTO attachments (ticket_id, uploader_account_id, storage_key, content_type, byte_size, \
                                      width, height, sha256, original_filename) \
             VALUES ($1, $2, $3, $4, 1024, 800, 600, repeat('a', 64), 'screen.png')",
        )
        .bind(ticket_id)
        .bind(requester.id)
        .bind(key)
        .bind(content_type)
        .execute(&test.pool)
    };
    insert("image/png", "ab/one").await.unwrap();
    insert("image/webp", "ab/two").await.unwrap();
    assert_constraint(
        insert("image/svg+xml", "ab/three").await,
        "attachments_content_type_check",
    );
    assert_constraint(
        insert("image/jpeg", "ab/one").await,
        "attachments_storage_key_key",
    );

    // A message attachment must reference a message on the same ticket.
    let (other_ticket, _) = create_queued_ticket(&test.pool, &requester).await;
    let message: Uuid = sqlx::query_scalar(
        "INSERT INTO ticket_messages (ticket_id, author_account_id, visibility, body) \
         VALUES ($1, $2, 'public', 'see screenshot') RETURNING id",
    )
    .bind(other_ticket)
    .bind(requester.id)
    .fetch_one(&test.pool)
    .await
    .unwrap();
    let cross = sqlx::query(
        "INSERT INTO attachments (ticket_id, message_id, uploader_account_id, storage_key, content_type, \
                                  byte_size, width, height, sha256) \
         VALUES ($1, $2, $3, 'cd/x', 'image/png', 1, 1, 1, repeat('b', 64))",
    )
    .bind(ticket_id)
    .bind(message)
    .bind(requester.id)
    .execute(&test.pool)
    .await;
    assert_constraint(cross, "attachments_message_same_ticket");
}

#[tokio::test]
async fn test_databases_are_dropped_on_cleanup() {
    let test = TestDb::new().await;
    let name = test.name().to_owned();
    let server = test.server_connect_options();
    drop(test); // Drop guard (also runs when a test panics)

    let mut conn = PgConnection::connect_with(&server).await.unwrap();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&name)
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert!(!exists, "{name} should have been dropped");
}
