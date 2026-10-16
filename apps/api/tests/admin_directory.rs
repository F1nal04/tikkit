//! Admin directory services (issue #11): roles, last-Admin protection,
//! teams and leadership, audit trail.

mod common;

use common::*;
use sqlx::PgPool;
use tikkit_api::{
    admin::{
        accounts::{self, AccountFilter, RoleChange},
        teams::{self, PreviousLead, TeamStatus},
    },
    domain::{AccountStatus, Role as AuthzRole},
    error::{ApiError, ErrorCode, FieldErrorCode},
    pagination::PageRequest,
};
use uuid::Uuid;

fn role(role: AuthzRole) -> RoleChange {
    RoleChange {
        role,
        specialization: None,
        replacement_lead_id: None,
    }
}

#[track_caller]
fn assert_code<T: std::fmt::Debug>(result: Result<T, ApiError>, code: ErrorCode) {
    match result {
        Ok(value) => panic!("expected {code:?}, got Ok({value:?})"),
        Err(error) => assert_eq!(error.code(), code, "{error:?}"),
    }
}

async fn role_of(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT role FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn current_team(pool: &PgPool, id: Uuid) -> Option<(Uuid, bool)> {
    sqlx::query_as(
        "SELECT team_id, is_lead FROM team_memberships WHERE account_id = $1 AND ended_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn current_leads(pool: &PgPool, team: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT account_id FROM team_memberships WHERE team_id = $1 AND is_lead AND ended_at IS NULL",
    )
    .bind(team)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn active_admins(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin' AND status = 'active'")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn audit_actions(pool: &PgPool, target: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE target_id = $1 ORDER BY occurred_at, id",
    )
    .bind(target.to_string())
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn audit_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(pool)
        .await
        .unwrap()
}

// --- Roles and last-Admin protection ----------------------------------------------

#[tokio::test]
async fn last_active_admin_cannot_be_demoted_or_deactivated() {
    let test = TestDb::new().await;
    let admin = complete_first_setup(&test.pool).await;

    for target_role in [AuthzRole::User, AuthzRole::Operator, AuthzRole::TeamLead] {
        assert_code(
            accounts::change_role(&test.pool, admin.id, admin.id, role(target_role)).await,
            ErrorCode::LastAdmin,
        );
    }
    assert_code(
        accounts::deactivate_account(&test.pool, admin.id, admin.id).await,
        ErrorCode::LastAdmin,
    );
    assert_eq!(role_of(&test.pool, admin.id).await, "admin");
    assert_eq!(active_admins(&test.pool).await, 1);
    assert_eq!(
        audit_count(&test.pool).await,
        0,
        "failed changes are not audited"
    );

    // With a second active Admin the first may step down.
    let second = create_account(&test.pool, Role::User, "second", "second@example.test").await;
    let promoted = accounts::change_role(&test.pool, admin.id, second.id, role(AuthzRole::Admin))
        .await
        .unwrap();
    assert_eq!(promoted.role, AuthzRole::Admin);
    let demoted = accounts::change_role(&test.pool, second.id, admin.id, role(AuthzRole::Operator))
        .await
        .unwrap();
    assert_eq!(demoted.role, AuthzRole::Operator);
    assert_eq!(active_admins(&test.pool).await, 1);
    assert_eq!(
        audit_actions(&test.pool, admin.id).await,
        ["account.role_changed"]
    );

    // A deactivated Admin does not count as the active one.
    let third = create_account(&test.pool, Role::Admin, "third", "third@example.test").await;
    accounts::deactivate_account(&test.pool, second.id, third.id)
        .await
        .unwrap();
    assert_code(
        accounts::deactivate_account(&test.pool, second.id, second.id).await,
        ErrorCode::LastAdmin,
    );
}

#[tokio::test]
async fn concurrent_demotion_of_the_only_two_admins_has_one_winner() {
    let test = TestDb::new().await;
    for round in 0..5 {
        let a = create_account(
            &test.pool,
            Role::Admin,
            &format!("admin.a{round}"),
            "a@example.test",
        )
        .await;
        let b = create_account(
            &test.pool,
            Role::Admin,
            &format!("admin.b{round}"),
            "b@example.test",
        )
        .await;
        if round > 0 {
            // Only `a`/`b` of this round are active Admins.
            sqlx::query(
                "UPDATE accounts SET role = 'user' WHERE role = 'admin' AND id <> $1 AND id <> $2",
            )
            .bind(a.id)
            .bind(b.id)
            .execute(&test.pool)
            .await
            .unwrap();
        }
        assert_eq!(active_admins(&test.pool).await, 2);

        let demote = |actor: Uuid, target: Uuid| {
            let pool = test.pool.clone();
            tokio::spawn(async move {
                accounts::change_role(&pool, actor, target, role(AuthzRole::Operator)).await
            })
        };
        // Each demotes the other, and (second pair) one demotes both.
        let (r1, r2) = tokio::join!(demote(a.id, b.id), demote(b.id, a.id));
        let results = [r1.unwrap(), r2.unwrap()];
        let ok = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 1, "round {round}: {results:?}");
        for error in results.iter().filter_map(|r| r.as_ref().err()) {
            assert!(
                matches!(error.code(), ErrorCode::LastAdmin | ErrorCode::Forbidden),
                "{error:?}"
            );
        }
        assert_eq!(active_admins(&test.pool).await, 1, "round {round}");
    }
}

#[tokio::test]
async fn concurrent_self_and_peer_demotion_keeps_an_admin() {
    let test = TestDb::new().await;
    let a = create_account(&test.pool, Role::Admin, "alpha", "x@example.test").await;
    let b = create_account(&test.pool, Role::Admin, "beta", "x@example.test").await;
    let demote = |target: Uuid| {
        let pool = test.pool.clone();
        let actor = a.id;
        tokio::spawn(async move {
            accounts::change_role(&pool, actor, target, role(AuthzRole::User)).await
        })
    };
    let (r1, r2) = tokio::join!(demote(a.id), demote(b.id));
    let results = [r1.unwrap(), r2.unwrap()];
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert_eq!(active_admins(&test.pool).await, 1);
}

#[tokio::test]
async fn admin_promotion_of_a_team_member_is_rejected_and_state_is_intact() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;

    for member in [&f.operator, &f.specialist] {
        assert_code(
            accounts::change_role(&test.pool, f.admin.id, member.id, role(AuthzRole::Admin)).await,
            ErrorCode::TeamMembershipConflict,
        );
        // Users do no team work either.
        assert_code(
            accounts::change_role(&test.pool, f.admin.id, member.id, role(AuthzRole::User)).await,
            ErrorCode::TeamMembershipConflict,
        );
        assert_eq!(
            current_team(&test.pool, member.id).await,
            Some((f.team.id, false))
        );
    }
    assert_eq!(role_of(&test.pool, f.operator.id).await, "operator");
    assert_eq!(role_of(&test.pool, f.specialist.id).await, "specialist");
    assert_eq!(audit_count(&test.pool).await, 0);

    // After the membership is ended explicitly, the promotion works.
    teams::remove_member(&test.pool, f.admin.id, f.team.id, f.operator.id)
        .await
        .unwrap();
    let promoted = accounts::change_role(
        &test.pool,
        f.admin.id,
        f.operator.id,
        role(AuthzRole::Admin),
    )
    .await
    .unwrap();
    assert_eq!(promoted.role, AuthzRole::Admin);
    assert_eq!(promoted.team, None);
}

#[tokio::test]
async fn role_changes_between_staff_roles_keep_membership_and_set_specialization() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let changed = accounts::change_role(
        &test.pool,
        f.admin.id,
        f.operator.id,
        RoleChange {
            role: AuthzRole::Specialist,
            specialization: Some("  Databases ".into()),
            replacement_lead_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(changed.role, AuthzRole::Specialist);
    assert_eq!(changed.specialization.as_deref(), Some("Databases"));
    assert_eq!(changed.team.as_ref().map(|t| t.id), Some(f.team.id));

    // Specialization is only for Specialists; back to Operator clears it.
    let invalid = accounts::change_role(
        &test.pool,
        f.admin.id,
        f.operator.id,
        RoleChange {
            role: AuthzRole::Operator,
            specialization: Some("Databases".into()),
            replacement_lead_id: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(invalid.code(), ErrorCode::ValidationFailed);
    assert_eq!(invalid.fields()[0].field, "specialization");
    let back = accounts::change_role(
        &test.pool,
        f.admin.id,
        f.operator.id,
        role(AuthzRole::Operator),
    )
    .await
    .unwrap();
    assert_eq!(back.specialization, None);

    // No-op changes write no audit rows.
    let before = audit_count(&test.pool).await;
    accounts::change_role(
        &test.pool,
        f.admin.id,
        f.operator.id,
        role(AuthzRole::Operator),
    )
    .await
    .unwrap();
    assert_eq!(audit_count(&test.pool).await, before);
}

#[tokio::test]
async fn only_active_admins_can_change_the_directory() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    for actor in [&f.user, &f.operator, &f.specialist, &f.team_lead] {
        assert_code(
            accounts::change_role(&test.pool, actor.id, f.user.id, role(AuthzRole::Admin)).await,
            ErrorCode::Forbidden,
        );
        assert_code(
            teams::create_team(&test.pool, actor.id, "Rogue", f.team_lead.id).await,
            ErrorCode::Forbidden,
        );
        assert_code(
            teams::add_member(&test.pool, actor.id, f.team.id, f.user.id).await,
            ErrorCode::Forbidden,
        );
    }
    // A second admin is deactivated and loses all directory permissions.
    let other = create_account(&test.pool, Role::Admin, "other", "other@example.test").await;
    accounts::deactivate_account(&test.pool, f.admin.id, other.id)
        .await
        .unwrap();
    assert_code(
        accounts::change_role(&test.pool, other.id, f.user.id, role(AuthzRole::Operator)).await,
        ErrorCode::Forbidden,
    );
    assert_code(
        teams::rename_team(&test.pool, other.id, f.team.id, "Renamed").await,
        ErrorCode::Forbidden,
    );
    assert_eq!(role_of(&test.pool, f.user.id).await, "user");
}

// --- Teams --------------------------------------------------------------------------

#[tokio::test]
async fn team_creation_is_atomic_and_validated() {
    let test = TestDb::new().await;
    let admin = complete_first_setup(&test.pool).await;
    let lead = create_account(&test.pool, Role::TeamLead, "lead", "lead@example.test").await;
    let operator = create_account(&test.pool, Role::Operator, "oper", "op@example.test").await;

    let team = teams::create_team(&test.pool, admin.id, "  Field Support ", lead.id)
        .await
        .unwrap();
    assert_eq!(team.name, "Field Support");
    assert_eq!(team.status, TeamStatus::Active);
    assert_eq!(team.lead.as_ref().map(|l| l.id), Some(lead.id));
    assert_eq!(team.members.len(), 1);
    assert!(team.members[0].is_lead);
    assert_eq!(audit_actions(&test.pool, team.id).await, ["team.created"]);

    // Invalid lead candidates leave no team behind.
    let lead2 = create_account(&test.pool, Role::TeamLead, "lead2", "lead2@example.test").await;
    for (name, lead_id, code, field) in [
        (
            "Apps",
            operator.id,
            ErrorCode::ValidationFailed,
            Some("leadAccountId"),
        ),
        (
            "Apps",
            admin.id,
            ErrorCode::ValidationFailed,
            Some("leadAccountId"),
        ),
        (
            "Apps",
            Uuid::now_v7(),
            ErrorCode::ValidationFailed,
            Some("leadAccountId"),
        ),
        // Already leads another team.
        ("Apps", lead.id, ErrorCode::TeamMembershipConflict, None),
        // Active names are unique, case-insensitively.
        (
            "field support",
            lead2.id,
            ErrorCode::ValidationFailed,
            Some("name"),
        ),
        ("   ", lead2.id, ErrorCode::ValidationFailed, Some("name")),
    ] {
        let error = teams::create_team(&test.pool, admin.id, name, lead_id)
            .await
            .unwrap_err();
        assert_eq!(error.code(), code, "{name} {error:?}");
        assert_eq!(
            error.fields().first().map(|f| f.field.as_str()),
            field,
            "{error:?}"
        );
    }
    let dup = teams::create_team(&test.pool, admin.id, "field support", lead2.id)
        .await
        .unwrap_err();
    assert_eq!(dup.fields()[0].code, FieldErrorCode::AlreadyTaken);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM teams")
        .fetch_one(&test.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(current_team(&test.pool, lead2.id).await, None);
}

#[tokio::test]
async fn an_account_cannot_join_a_second_team_or_lead_a_different_team() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let lead2 = create_account(&test.pool, Role::TeamLead, "lead2", "lead2@example.test").await;
    let other = teams::create_team(&test.pool, f.admin.id, "Network", lead2.id)
        .await
        .unwrap();

    assert_code(
        teams::add_member(&test.pool, f.admin.id, other.id, f.operator.id).await,
        ErrorCode::TeamMembershipConflict,
    );
    assert_code(
        teams::add_member(&test.pool, f.admin.id, f.team.id, f.operator.id).await,
        ErrorCode::TeamMembershipConflict,
    );
    // Leading a different team than one's own.
    assert_code(
        teams::replace_lead(
            &test.pool,
            f.admin.id,
            other.id,
            f.team_lead.id,
            PreviousLead::KeepAsMember,
        )
        .await,
        ErrorCode::TeamMembershipConflict,
    );
    // Admin and User accounts never join teams.
    assert_code(
        teams::add_member(&test.pool, f.admin.id, other.id, f.admin.id).await,
        ErrorCode::TeamMembershipConflict,
    );
    assert_code(
        teams::add_member(&test.pool, f.admin.id, other.id, f.user.id).await,
        ErrorCode::TeamMembershipConflict,
    );
    assert_eq!(
        current_team(&test.pool, f.operator.id).await,
        Some((f.team.id, false))
    );
    assert_eq!(current_leads(&test.pool, other.id).await, vec![lead2.id]);
    assert_eq!(
        current_leads(&test.pool, f.team.id).await,
        vec![f.team_lead.id]
    );
}

#[tokio::test]
async fn lead_replacement_never_creates_co_leads_or_a_leaderless_team() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let outsider = create_account(&test.pool, Role::TeamLead, "succ", "succ@example.test").await;

    let team = teams::replace_lead(
        &test.pool,
        f.admin.id,
        f.team.id,
        outsider.id,
        PreviousLead::KeepAsMember,
    )
    .await
    .unwrap();
    assert_eq!(team.lead.as_ref().map(|l| l.id), Some(outsider.id));
    assert_eq!(team.members.iter().filter(|m| m.is_lead).count(), 1);
    // The previous lead stays as a plain member with its role unchanged.
    assert_eq!(
        current_team(&test.pool, f.team_lead.id).await,
        Some((f.team.id, false))
    );
    assert_eq!(role_of(&test.pool, f.team_lead.id).await, "team_lead");

    // Promote an existing plain member back, removing the previous lead.
    let team = teams::replace_lead(
        &test.pool,
        f.admin.id,
        f.team.id,
        f.team_lead.id,
        PreviousLead::RemoveFromTeam,
    )
    .await
    .unwrap();
    assert_eq!(
        current_leads(&test.pool, f.team.id).await,
        vec![f.team_lead.id]
    );
    assert_eq!(current_team(&test.pool, outsider.id).await, None);
    assert_eq!(team.members.len(), 3);

    // Non-Team-Lead candidates are rejected; the current lead is a no-op.
    assert_code(
        teams::replace_lead(
            &test.pool,
            f.admin.id,
            f.team.id,
            f.operator.id,
            PreviousLead::KeepAsMember,
        )
        .await,
        ErrorCode::ValidationFailed,
    );
    teams::replace_lead(
        &test.pool,
        f.admin.id,
        f.team.id,
        f.team_lead.id,
        PreviousLead::KeepAsMember,
    )
    .await
    .unwrap();
    assert_eq!(
        audit_actions(&test.pool, f.team.id).await,
        ["team.lead_replaced", "team.lead_replaced"]
    );
}

#[tokio::test]
async fn concurrent_lead_replacements_end_with_a_single_consistent_lead() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let mut candidates = Vec::new();
    for i in 0..4 {
        candidates.push(
            create_account(
                &test.pool,
                Role::TeamLead,
                &format!("cand{i}"),
                "cand@example.test",
            )
            .await,
        );
    }
    let handles: Vec<_> = candidates
        .iter()
        .map(|c| {
            let pool = test.pool.clone();
            let (admin, team, id) = (f.admin.id, f.team.id, c.id);
            tokio::spawn(async move {
                teams::replace_lead(&pool, admin, team, id, PreviousLead::KeepAsMember).await
            })
        })
        .collect();
    for handle in handles {
        let result = handle.await.unwrap();
        if let Err(error) = &result {
            assert_eq!(error.code(), ErrorCode::Conflict, "{error:?}");
        }
    }
    let leads = current_leads(&test.pool, f.team.id).await;
    assert_eq!(leads.len(), 1);
    assert!(candidates.iter().any(|c| c.id == leads[0]));
    // Everyone who led at some point is still a member exactly once.
    for account in candidates.iter().map(|c| c.id).chain([f.team_lead.id]) {
        let team = current_team(&test.pool, account).await;
        assert!(
            team.is_none() || team.map(|t| t.0) == Some(f.team.id),
            "{team:?}"
        );
    }
    let team = teams::get_team(&test.pool, f.team.id).await.unwrap();
    assert_eq!(team.members.iter().filter(|m| m.is_lead).count(), 1);
}

#[tokio::test]
async fn the_sole_lead_cannot_be_removed_demoted_or_deactivated() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    assert_code(
        teams::remove_member(&test.pool, f.admin.id, f.team.id, f.team_lead.id).await,
        ErrorCode::TeamLeadRequired,
    );
    for target in [AuthzRole::Operator, AuthzRole::User, AuthzRole::Admin] {
        assert_code(
            accounts::change_role(&test.pool, f.admin.id, f.team_lead.id, role(target)).await,
            ErrorCode::TeamLeadRequired,
        );
    }
    assert_code(
        accounts::deactivate_account(&test.pool, f.admin.id, f.team_lead.id).await,
        ErrorCode::TeamLeadRequired,
    );
    assert_eq!(
        current_leads(&test.pool, f.team.id).await,
        vec![f.team_lead.id]
    );
    assert_eq!(role_of(&test.pool, f.team_lead.id).await, "team_lead");
    assert_eq!(audit_count(&test.pool).await, 0);

    // A replacement that is not a Team Lead fails the whole change.
    let bad = accounts::change_role(
        &test.pool,
        f.admin.id,
        f.team_lead.id,
        RoleChange {
            role: AuthzRole::Operator,
            specialization: None,
            replacement_lead_id: Some(f.operator.id),
        },
    )
    .await;
    assert_code(bad, ErrorCode::ValidationFailed);
    assert_eq!(role_of(&test.pool, f.team_lead.id).await, "team_lead");

    // Demotion together with an explicit replacement is atomic.
    let successor = create_account(&test.pool, Role::TeamLead, "succ", "s@example.test").await;
    let demoted = accounts::change_role(
        &test.pool,
        f.admin.id,
        f.team_lead.id,
        RoleChange {
            role: AuthzRole::Operator,
            specialization: None,
            replacement_lead_id: Some(successor.id),
        },
    )
    .await
    .unwrap();
    assert_eq!(demoted.role, AuthzRole::Operator);
    let team = demoted.team.unwrap();
    assert_eq!((team.id, team.is_lead), (f.team.id, false));
    assert_eq!(
        current_leads(&test.pool, f.team.id).await,
        vec![successor.id]
    );

    // Promoting a lead to Admin with a replacement removes it from the team.
    let next = create_account(&test.pool, Role::TeamLead, "next", "n@example.test").await;
    let admin = accounts::change_role(
        &test.pool,
        f.admin.id,
        successor.id,
        RoleChange {
            role: AuthzRole::Admin,
            specialization: None,
            replacement_lead_id: Some(next.id),
        },
    )
    .await
    .unwrap();
    assert_eq!(admin.role, AuthzRole::Admin);
    assert_eq!(admin.team, None);
    assert_eq!(current_leads(&test.pool, f.team.id).await, vec![next.id]);
    // The former lead can now be deactivated.
    let gone = accounts::deactivate_account(&test.pool, f.admin.id, f.operator.id)
        .await
        .unwrap();
    assert_eq!(gone.status, AccountStatus::Deactivated);
    let back = accounts::reactivate_account(&test.pool, f.admin.id, f.operator.id)
        .await
        .unwrap();
    assert_eq!(back.status, AccountStatus::Active);
}

#[tokio::test]
async fn removing_a_member_clears_its_open_assignments_in_that_team() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let (ticket, _) = create_queued_ticket(&test.pool, &f.user).await;
    sqlx::query(
        "UPDATE tickets SET team_id = $2, assignee_account_id = $3, status = 'in_progress' WHERE id = $1",
    )
    .bind(ticket)
    .bind(f.team.id)
    .bind(f.operator.id)
    .execute(&test.pool)
    .await
    .unwrap();

    teams::remove_member(&test.pool, f.admin.id, f.team.id, f.operator.id)
        .await
        .unwrap();
    let assignee: Option<Uuid> =
        sqlx::query_scalar("SELECT assignee_account_id FROM tickets WHERE id = $1")
            .bind(ticket)
            .fetch_one(&test.pool)
            .await
            .unwrap();
    assert_eq!(assignee, None);
    let events: Vec<String> =
        sqlx::query_scalar("SELECT event_type FROM ticket_events WHERE ticket_id = $1")
            .bind(ticket)
            .fetch_all(&test.pool)
            .await
            .unwrap();
    assert_eq!(events, ["ticket.unassigned"]);
    assert_code(
        teams::remove_member(&test.pool, f.admin.id, f.team.id, f.operator.id).await,
        ErrorCode::NotFound,
    );
}

#[tokio::test]
async fn archiving_ends_memberships_and_requires_no_open_tickets() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let (ticket, _) = create_queued_ticket(&test.pool, &f.user).await;
    sqlx::query("UPDATE tickets SET team_id = $2 WHERE id = $1")
        .bind(ticket)
        .bind(f.team.id)
        .execute(&test.pool)
        .await
        .unwrap();
    assert_code(
        teams::archive_team(&test.pool, f.admin.id, f.team.id).await,
        ErrorCode::TeamHasOpenTickets,
    );
    sqlx::query("UPDATE tickets SET team_id = NULL WHERE id = $1")
        .bind(ticket)
        .execute(&test.pool)
        .await
        .unwrap();

    let archived = teams::archive_team(&test.pool, f.admin.id, f.team.id)
        .await
        .unwrap();
    assert_eq!(archived.status, TeamStatus::Archived);
    assert!(archived.archived_at.is_some());
    assert!(archived.lead.is_none() && archived.members.is_empty());
    for member in [&f.team_lead, &f.operator, &f.specialist] {
        assert_eq!(current_team(&test.pool, member.id).await, None);
    }
    // Archived teams are frozen; the former lead is free to change role.
    assert_code(
        teams::add_member(&test.pool, f.admin.id, f.team.id, f.operator.id).await,
        ErrorCode::Conflict,
    );
    assert_code(
        teams::archive_team(&test.pool, f.admin.id, f.team.id).await,
        ErrorCode::Conflict,
    );
    accounts::change_role(
        &test.pool,
        f.admin.id,
        f.team_lead.id,
        role(AuthzRole::Operator),
    )
    .await
    .unwrap();
    assert!(
        teams::list_teams(&test.pool, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(teams::list_teams(&test.pool, true).await.unwrap().len(), 1);
    // The archived name can be reused.
    let lead = create_account(&test.pool, Role::TeamLead, "new.lead", "nl@example.test").await;
    teams::create_team(&test.pool, f.admin.id, "Service Desk", lead.id)
        .await
        .unwrap();
    assert_eq!(
        audit_actions(&test.pool, f.team.id).await,
        ["team.archived"]
    );
}

#[tokio::test]
async fn rename_is_validated_and_audited() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let renamed = teams::rename_team(&test.pool, f.admin.id, f.team.id, "Help Desk")
        .await
        .unwrap();
    assert_eq!(renamed.name, "Help Desk");
    let lead2 = create_account(&test.pool, Role::TeamLead, "lead2", "l2@example.test").await;
    let other = teams::create_team(&test.pool, f.admin.id, "Network", lead2.id)
        .await
        .unwrap();
    let dup = teams::rename_team(&test.pool, f.admin.id, other.id, "HELP DESK")
        .await
        .unwrap_err();
    assert_eq!(dup.code(), ErrorCode::ValidationFailed);
    assert_eq!(dup.fields()[0].code, FieldErrorCode::AlreadyTaken);
    assert_code(
        teams::rename_team(&test.pool, f.admin.id, Uuid::now_v7(), "X").await,
        ErrorCode::NotFound,
    );
    let metadata: serde_json::Value = sqlx::query_scalar(
        "SELECT metadata FROM audit_events WHERE action = 'team.renamed' AND target_id = $1",
    )
    .bind(f.team.id.to_string())
    .fetch_one(&test.pool)
    .await
    .unwrap();
    assert_eq!(
        metadata,
        serde_json::json!({"previousName": "Service Desk", "name": "Help Desk"})
    );
}

// --- Shared email ------------------------------------------------------------------

#[tokio::test]
async fn accounts_sharing_an_email_are_constrained_independently() {
    let test = TestDb::new().await;
    let admin = complete_first_setup(&test.pool).await;
    let shared = "it@example.test";
    assert_eq!(admin.email, shared);
    let lead = create_account(&test.pool, Role::TeamLead, "it.lead", shared).await;
    let team = teams::create_team(&test.pool, admin.id, "Ops", lead.id)
        .await
        .unwrap();

    // The Admin account cannot join, even though its email matches a member.
    assert_code(
        teams::add_member(&test.pool, admin.id, team.id, admin.id).await,
        ErrorCode::TeamMembershipConflict,
    );
    // The Team Lead account cannot become Admin while it leads/belongs.
    assert_code(
        accounts::change_role(&test.pool, admin.id, lead.id, role(AuthzRole::Admin)).await,
        ErrorCode::TeamLeadRequired,
    );
    let admin_view = accounts::get_account(&test.pool, admin.id).await.unwrap();
    let lead_view = accounts::get_account(&test.pool, lead.id).await.unwrap();
    assert_eq!(admin_view.role, AuthzRole::Admin);
    assert_eq!(admin_view.team, None);
    assert_eq!(lead_view.role, AuthzRole::TeamLead);
    assert_eq!(
        lead_view.team.map(|t| (t.id, t.is_lead)),
        Some((team.id, true))
    );
    assert_ne!(admin_view.id, lead_view.id);
    assert_ne!(admin_view.username, lead_view.username);
}

// --- Listing --------------------------------------------------------------------------

#[tokio::test]
async fn list_accounts_filters_and_paginates() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let all = accounts::list_accounts(
        &test.pool,
        &AccountFilter::default(),
        &PageRequest {
            limit: 2,
            after: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(all.items.len(), 2);
    assert_eq!(all.items[0].username, "admin");
    let cursor = all.next_cursor.clone().unwrap();
    let after = tikkit_api::pagination::PageParams {
        limit: Some("10".into()),
        cursor: Some(cursor),
    }
    .validate::<accounts::AccountCursor>()
    .unwrap();
    let rest = accounts::list_accounts(&test.pool, &AccountFilter::default(), &after)
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 3);
    assert!(rest.next_cursor.is_none());

    let staff = accounts::list_accounts(
        &test.pool,
        &AccountFilter {
            role: Some(AuthzRole::Specialist),
            ..AccountFilter::default()
        },
        &PageRequest {
            limit: 25,
            after: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(staff.items.len(), 1);
    assert_eq!(staff.items[0].id, f.specialist.id);
    assert_eq!(
        staff.items[0].team.as_ref().map(|t| t.name.as_str()),
        Some("Service Desk")
    );

    // `q` matches username/display name/email; LIKE wildcards are literal.
    let search = |q: &str| {
        let pool = test.pool.clone();
        let q = q.to_owned();
        async move {
            accounts::list_accounts(
                &pool,
                &AccountFilter {
                    q: Some(q),
                    ..AccountFilter::default()
                },
                &PageRequest {
                    limit: 25,
                    after: None,
                },
            )
            .await
            .unwrap()
            .items
            .len()
        }
    };
    assert_eq!(search("LEAD@").await, 1);
    assert_eq!(search("%").await, 0);
    assert_eq!(search("_").await, 0);
    assert_eq!(search("test ").await, 5);

    let deactivated = accounts::list_accounts(
        &test.pool,
        &AccountFilter {
            status: Some(AccountStatus::Deactivated),
            ..AccountFilter::default()
        },
        &PageRequest {
            limit: 25,
            after: None,
        },
    )
    .await
    .unwrap();
    assert!(deactivated.items.is_empty());
    assert_code(
        accounts::get_account(&test.pool, Uuid::now_v7()).await,
        ErrorCode::NotFound,
    );
}

// --- Audit -------------------------------------------------------------------------------

#[tokio::test]
async fn directory_changes_are_audited_and_audit_rows_are_immutable() {
    let test = TestDb::new().await;
    let f = RoleFixtures::create(&test.pool).await;
    let newcomer = create_account(&test.pool, Role::Operator, "newcomer", "n@example.test").await;
    teams::add_member(&test.pool, f.admin.id, f.team.id, newcomer.id)
        .await
        .unwrap();
    teams::remove_member(&test.pool, f.admin.id, f.team.id, newcomer.id)
        .await
        .unwrap();
    accounts::change_role(&test.pool, f.admin.id, newcomer.id, role(AuthzRole::User))
        .await
        .unwrap();

    let rows: Vec<(
        Option<Uuid>,
        String,
        String,
        String,
        String,
        serde_json::Value,
    )> = sqlx::query_as(
        "SELECT actor_account_id, actor_kind, action, target_type, target_id, metadata \
             FROM audit_events ORDER BY occurred_at, id",
    )
    .fetch_all(&test.pool)
    .await
    .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    assert_eq!(
        actions,
        [
            "team.member_added",
            "team.member_removed",
            "account.role_changed"
        ]
    );
    for (actor, kind, _, _, _, _) in &rows {
        assert_eq!(*actor, Some(f.admin.id));
        assert_eq!(kind, "account");
    }
    assert_eq!(
        (rows[0].3.as_str(), rows[0].4.clone()),
        ("team", f.team.id.to_string())
    );
    assert_eq!(rows[0].5["accountId"], newcomer.id.to_string());
    assert_eq!(rows[2].3, "account");
    assert_eq!(rows[2].5["fromRole"], "operator");
    assert_eq!(rows[2].5["toRole"], "user");

    let update = sqlx::query("UPDATE audit_events SET action = 'team.created'")
        .execute(&test.pool)
        .await;
    assert_constraint(update, "audit_events_append_only");
    let delete = sqlx::query("DELETE FROM audit_events")
        .execute(&test.pool)
        .await;
    assert_constraint(delete, "audit_events_append_only");
}
