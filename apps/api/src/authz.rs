//! Role/permission matrix, team scope, and ticket access policy. See docs/authorization.md.
//!
//! Two layers, both enforced here and never in the frontend:
//! - **Capability**: [`allows`] answers "may this role ever do X" from one const table.
//!   Handlers start with `authz::require_permission(&user, Permission::X)?;`, which
//!   denies with `403 forbidden` (the endpoint is public knowledge, the action is not allowed).
//! - **Resource**: [`ticket_view`] and [`authorize_ticket`] combine the role with
//!   ownership, team scope, assignment, and status. A ticket the actor may not see
//!   is `404 not_found`, identical to a missing one; a visible ticket the actor may
//!   not change is `403 forbidden`.
//!
//! Roles are re-read from `accounts` on every request (session middleware), so
//! role changes apply to live sessions immediately.

use serde::Serialize;
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::{
    api::{ApiError, Role, TicketStatus},
    auth::CurrentUser,
};

/// Role-level capabilities from spec.md. Stable snake_case identifiers.
/// Ticket permissions are additionally scoped per ticket by [`authorize_ticket`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    SubmitTicket,
    ViewGeneralQueue,
    DispatchTicket,
    ClaimTicket,
    AssignTicket,
    ReplyAsStaff,
    AddInternalNote,
    ChangePriority,
    ProposeResolution,
    /// Invitations, roles, deactivation.
    ManageAccounts,
    /// Teams, memberships, leadership.
    ManageTeams,
    /// Categories, priorities, conditional fields.
    ManageCategories,
    /// Organization, lifecycle, SMTP, and AI/BYOK settings.
    ManageSettings,
    /// Admin submission view, including AI attempts and AI-solved records.
    ViewAllSubmissions,
    /// Reports, exports, and audit history.
    ViewReports,
}

const N: bool = false;
const Y: bool = true;

/// The matrix. Columns follow `Role` declaration order:
/// user, operator, specialist, team_lead, admin. Rows follow `Permission` order.
#[rustfmt::skip]
const MATRIX: [(Permission, [bool; 5]); 15] = {
    use Permission::*;
    [
        (SubmitTicket,       [Y, Y, Y, Y, Y]),
        (ViewGeneralQueue,   [N, Y, Y, Y, Y]),
        (DispatchTicket,     [N, Y, Y, Y, Y]),
        (ClaimTicket,        [N, Y, Y, Y, N]), // Admins are never assignees
        (AssignTicket,       [N, N, N, Y, Y]), // Team Lead: own team only
        (ReplyAsStaff,       [N, Y, Y, Y, Y]),
        (AddInternalNote,    [N, Y, Y, Y, Y]),
        (ChangePriority,     [N, Y, Y, Y, Y]),
        (ProposeResolution,  [N, Y, Y, Y, Y]),
        (ManageAccounts,     [N, N, N, N, Y]),
        (ManageTeams,        [N, N, N, N, Y]),
        (ManageCategories,   [N, N, N, N, Y]),
        (ManageSettings,     [N, N, N, N, Y]),
        (ViewAllSubmissions, [N, N, N, N, Y]),
        (ViewReports,        [N, N, N, N, Y]),
    ]
};

pub fn allows(role: Role, permission: Permission) -> bool {
    MATRIX[permission as usize].1[role as usize]
}

/// Capability check for a whole endpoint: `403 forbidden` if the role lacks it.
pub fn require_permission(user: &CurrentUser, permission: Permission) -> Result<(), ApiError> {
    if allows(user.role(), permission) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// An account's current team (non-archived) and whether it leads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeamScope {
    pub team_id: Uuid,
    pub is_lead: bool,
}

/// The acting account with its team scope, as the ticket policy needs it.
#[derive(Clone, Copy, Debug)]
pub struct Actor {
    pub id: Uuid,
    pub role: Role,
    pub team: Option<TeamScope>,
}

impl Actor {
    pub async fn load<'c>(db: impl PgExecutor<'c>, user: &CurrentUser) -> Result<Self, ApiError> {
        Ok(Self {
            id: user.account_id(),
            role: user.role(),
            team: team_of(db, user.account_id()).await?,
        })
    }

    fn leads(&self, team: Option<Uuid>) -> bool {
        team.is_some_and(|t| {
            self.team
                == Some(TeamScope {
                    team_id: t,
                    is_lead: true,
                })
        })
    }
}

/// The one team an account belongs to (schema: at most one), ignoring archived teams.
pub async fn team_of<'c>(
    db: impl PgExecutor<'c>,
    account_id: Uuid,
) -> Result<Option<TeamScope>, sqlx::Error> {
    let row: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT t.id, t.lead_account_id IS NOT DISTINCT FROM m.account_id
         FROM team_memberships m JOIN teams t ON t.id = m.team_id
         WHERE m.account_id = $1 AND t.archived_at IS NULL",
    )
    .bind(account_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(team_id, is_lead)| TeamScope { team_id, is_lead }))
}

pub async fn is_lead_of<'c>(
    db: impl PgExecutor<'c>,
    account_id: Uuid,
    team_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM teams
                        WHERE id = $2 AND lead_account_id = $1 AND archived_at IS NULL)",
    )
    .bind(account_id)
    .bind(team_id)
    .fetch_one(db)
    .await
}

/// Active accounts in a non-archived team, ordered by ID. Use for assignment
/// eligibility and notification recipients.
pub async fn active_team_members<'c>(
    db: impl PgExecutor<'c>,
    team_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT a.id FROM team_memberships m
         JOIN accounts a ON a.id = m.account_id
         JOIN teams t ON t.id = m.team_id
         WHERE m.team_id = $1 AND a.is_active AND t.archived_at IS NULL
         ORDER BY a.id",
    )
    .bind(team_id)
    .fetch_all(db)
    .await
}

/// The `tickets` columns the policy needs; same names as the table.
#[derive(Clone, Copy, Debug)]
pub struct TicketScope {
    pub requester_account_id: Uuid,
    pub team_id: Option<Uuid>,
    pub assignee_account_id: Option<Uuid>,
    pub status: TicketStatus,
}

/// Which representation of a ticket the actor may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketView {
    /// Public ticket data only: no internal notes or staff-only screenshots.
    Requester,
    /// Full staff view.
    Staff,
}

/// `None` means the actor must not learn the ticket exists (`404 not_found`).
///
/// The requester always gets the requester view of their own ticket, even when
/// they are staff on its team: requester identity takes precedence (self-handling).
/// Staff never see drafts; only Admins see tickets still in or finished by AI;
/// the general queue is open to every staff role; a dispatched ticket only to
/// current members of its team and Admins.
pub fn ticket_view(actor: &Actor, ticket: &TicketScope) -> Option<TicketView> {
    use TicketStatus::*;
    if actor.id == ticket.requester_account_id {
        return Some(TicketView::Requester);
    }
    let admin = actor.role == Role::Admin;
    // ponytail: a ticket cancelled during AI shows in the general queue's
    // `cancelled` history; add a "was human-bound" column if that matters.
    let visible = match ticket.status {
        Draft => false,
        AiProcessing | AwaitingAiDecision | AiSolved => admin,
        _ => {
            admin
                || match ticket.team_id {
                    None => allows(actor.role, Permission::ViewGeneralQueue),
                    Some(team) => {
                        actor.role.is_staff() && actor.team.is_some_and(|s| s.team_id == team)
                    }
                }
        }
    };
    visible.then_some(TicketView::Staff)
}

/// Staff action on one ticket. `404` if invisible, `403` if visible but denied.
///
/// Rules, in order: the role must hold `action`; the requester can never act as
/// staff on their own ticket (compared by account ID, any role); the ticket must
/// be open for human work; claiming needs an unclaimed team ticket; assigning
/// needs the team's Lead or an Admin; every other write on a claimed ticket needs
/// its assignee, the team's Lead, or an Admin.
pub fn authorize_ticket(
    actor: &Actor,
    ticket: &TicketScope,
    action: Permission,
) -> Result<(), ApiError> {
    use Permission::*;
    use TicketStatus::*;
    match ticket_view(actor, ticket) {
        None => return Err(ApiError::NotFound),
        Some(TicketView::Requester) => return Err(ApiError::Forbidden),
        Some(TicketView::Staff) => {}
    }
    let open = matches!(
        ticket.status,
        Queued | InProgress | WaitingForRequester | AwaitingConfirmation
    );
    let admin = actor.role == Role::Admin;
    let in_charge = || match ticket.assignee_account_id {
        None => true,
        Some(assignee) => assignee == actor.id || admin || actor.leads(ticket.team_id),
    };
    let ok = open
        && allows(actor.role, action)
        && match action {
            ClaimTicket => ticket.team_id.is_some() && ticket.assignee_account_id.is_none(),
            AssignTicket => ticket.team_id.is_some() && (admin || actor.leads(ticket.team_id)),
            DispatchTicket | ReplyAsStaff | AddInternalNote | ChangePriority
            | ProposeResolution => in_charge(),
            _ => false, // not a ticket action
        };
    if ok { Ok(()) } else { Err(ApiError::Forbidden) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Permission::*;
    use Role::*;

    const ROLES: [Role; 5] = [User, Operator, Specialist, TeamLead, Admin];

    fn permissions() -> impl Iterator<Item = Permission> {
        MATRIX.iter().map(|(p, _)| *p)
    }

    fn ident(value: impl Serialize) -> String {
        serde_json::to_value(value)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn matrix_rows_are_indexed_by_permission() {
        for (i, (p, _)) in MATRIX.iter().enumerate() {
            assert_eq!(*p as usize, i, "{p:?}");
        }
        for (i, r) in ROLES.iter().enumerate() {
            assert_eq!(*r as usize, i, "{r:?}");
        }
    }

    /// Expectations stated independently of the table, straight from spec.md.
    #[test]
    fn matrix_matches_spec() {
        let operator = [
            SubmitTicket,
            ViewGeneralQueue,
            DispatchTicket,
            ClaimTicket,
            ReplyAsStaff,
            AddInternalNote,
            ChangePriority,
            ProposeResolution,
        ];
        for p in permissions() {
            // User only submits and follows own tickets (ownership, not role).
            assert_eq!(allows(User, p), p == SubmitTicket, "user {p:?}");
            assert_eq!(allows(Operator, p), operator.contains(&p), "operator {p:?}");
            // Specialist is a title only.
            assert_eq!(
                allows(Specialist, p),
                allows(Operator, p),
                "specialist {p:?}"
            );
            // Team Lead adds assignment within the led team.
            assert_eq!(
                allows(TeamLead, p),
                allows(Operator, p) || p == AssignTicket,
                "team_lead {p:?}"
            );
            // Admin does everything except become an assignee.
            assert_eq!(allows(Admin, p), p != ClaimTicket, "admin {p:?}");
        }
    }

    fn render_matrix() -> String {
        let mut out = String::from("| Permission |");
        for r in ROLES {
            out += &format!(" `{}` |", ident(r));
        }
        out += "\n| --- |";
        out += &" :-: |".repeat(ROLES.len());
        for p in permissions() {
            out += &format!("\n| `{}` |", ident(p));
            for r in ROLES {
                out += if allows(r, p) { " yes |" } else { " - |" };
            }
        }
        out
    }

    #[test]
    fn docs_show_the_current_matrix() {
        let docs = include_str!("../../../docs/authorization.md");
        let table = render_matrix();
        assert!(
            docs.contains(&table),
            "update docs/authorization.md:\n{table}"
        );
    }

    // --- ticket policy ---

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }
    const TEAM: u128 = 100;
    const OTHER_TEAM: u128 = 200;

    fn actor(n: u128, role: Role, team: Option<(u128, bool)>) -> Actor {
        Actor {
            id: uid(n),
            role,
            team: team.map(|(t, is_lead)| TeamScope {
                team_id: uid(t),
                is_lead,
            }),
        }
    }

    fn ticket(
        requester: u128,
        team: Option<u128>,
        assignee: Option<u128>,
        status: TicketStatus,
    ) -> TicketScope {
        TicketScope {
            requester_account_id: uid(requester),
            team_id: team.map(uid),
            assignee_account_id: assignee.map(uid),
            status,
        }
    }

    fn code(r: Result<(), ApiError>) -> &'static str {
        match r {
            Ok(()) => "ok",
            Err(ApiError::NotFound) => "not_found",
            Err(ApiError::Forbidden) => "forbidden",
            Err(e) => panic!("{e:?}"),
        }
    }

    const WRITES: [Permission; 5] = [
        DispatchTicket,
        ReplyAsStaff,
        AddInternalNote,
        ChangePriority,
        ProposeResolution,
    ];

    #[test]
    fn requester_sees_own_ticket_in_every_status_but_never_acts_as_staff() {
        use TicketStatus::*;
        let statuses = [
            Draft,
            AiProcessing,
            AwaitingAiDecision,
            Queued,
            InProgress,
            WaitingForRequester,
            AwaitingConfirmation,
            Closed,
            Cancelled,
            AiSolved,
        ];
        for role in ROLES {
            // A staff requester who is even the lead of the ticket's team.
            let team = (role != Admin && role != User).then_some((TEAM, true));
            let me = actor(1, role, team);
            for status in statuses {
                let t = ticket(1, Some(TEAM), None, status);
                assert_eq!(
                    ticket_view(&me, &t),
                    Some(TicketView::Requester),
                    "{role:?} {status:?}"
                );
                for p in permissions() {
                    assert_eq!(
                        code(authorize_ticket(&me, &t, p)),
                        "forbidden",
                        "{role:?} {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn self_handling_compares_account_ids_only() {
        // Same person, separate Admin account: handles the ticket normally.
        let admin = actor(2, Admin, None);
        let t = ticket(1, Some(TEAM), Some(3), TicketStatus::InProgress);
        for p in WRITES.into_iter().chain([AssignTicket]) {
            assert_eq!(code(authorize_ticket(&admin, &t, p)), "ok", "{p:?}");
        }
    }

    #[test]
    fn general_queue_is_open_to_all_staff_and_hidden_from_users() {
        let t = ticket(1, None, None, TicketStatus::Queued);
        for role in ROLES {
            let a = actor(2, role, None);
            let staff = role != User;
            assert_eq!(ticket_view(&a, &t).is_some(), staff, "{role:?}");
            let expected = if staff { "ok" } else { "not_found" };
            assert_eq!(
                code(authorize_ticket(&a, &t, DispatchTicket)),
                expected,
                "{role:?}"
            );
            assert_eq!(
                code(authorize_ticket(&a, &t, ReplyAsStaff)),
                expected,
                "{role:?}"
            );
            // Claim/assign need a team first.
            if staff {
                assert_eq!(code(authorize_ticket(&a, &t, ClaimTicket)), "forbidden");
                assert_eq!(code(authorize_ticket(&a, &t, AssignTicket)), "forbidden");
            }
        }
    }

    #[test]
    fn dispatched_ticket_is_visible_only_to_its_team_and_admins() {
        let t = ticket(1, Some(TEAM), None, TicketStatus::Queued);
        let cases = [
            (actor(2, Operator, Some((TEAM, false))), true),
            (actor(3, Specialist, Some((TEAM, false))), true),
            (actor(4, TeamLead, Some((TEAM, true))), true),
            (actor(5, Admin, None), true),
            // The dispatcher in another team, or no team, loses access.
            (actor(6, Operator, Some((OTHER_TEAM, false))), false),
            (actor(7, Operator, None), false),
            (actor(8, TeamLead, Some((OTHER_TEAM, true))), false),
            // A User-role member is not staff.
            (actor(9, User, Some((TEAM, false))), false),
        ];
        for (a, visible) in cases {
            assert_eq!(ticket_view(&a, &t).is_some(), visible, "{a:?}");
            let expected = if visible { "ok" } else { "not_found" };
            assert_eq!(
                code(authorize_ticket(&a, &t, ReplyAsStaff)),
                expected,
                "{a:?}"
            );
        }
    }

    #[test]
    fn claimed_ticket_writes_need_assignee_lead_or_admin() {
        let t = ticket(1, Some(TEAM), Some(2), TicketStatus::InProgress);
        let cases = [
            (actor(2, Operator, Some((TEAM, false))), "ok"), // assignee
            (actor(3, Specialist, Some((TEAM, false))), "forbidden"), // read-only peer
            (actor(4, TeamLead, Some((TEAM, true))), "ok"),
            (actor(5, Admin, None), "ok"),
            (actor(6, TeamLead, Some((OTHER_TEAM, true))), "not_found"),
        ];
        for (a, expected) in cases {
            assert!(ticket_view(&a, &t).is_some() == (expected != "not_found"));
            for p in WRITES {
                assert_eq!(code(authorize_ticket(&a, &t, p)), expected, "{a:?} {p:?}");
            }
        }
        // Nobody takes over by claiming.
        for n in [2, 3, 4] {
            let a = actor(n, Operator, Some((TEAM, n == 4)));
            assert_eq!(code(authorize_ticket(&a, &t, ClaimTicket)), "forbidden");
        }
    }

    #[test]
    fn unclaimed_team_ticket_claim_and_assign() {
        let t = ticket(1, Some(TEAM), None, TicketStatus::Queued);
        let member = actor(2, Operator, Some((TEAM, false)));
        let lead = actor(3, TeamLead, Some((TEAM, true)));
        let admin = actor(4, Admin, None);
        assert_eq!(code(authorize_ticket(&member, &t, ClaimTicket)), "ok");
        assert_eq!(code(authorize_ticket(&lead, &t, ClaimTicket)), "ok");
        assert_eq!(code(authorize_ticket(&admin, &t, ClaimTicket)), "forbidden");
        assert_eq!(
            code(authorize_ticket(&member, &t, AssignTicket)),
            "forbidden"
        );
        assert_eq!(code(authorize_ticket(&lead, &t, AssignTicket)), "ok");
        assert_eq!(code(authorize_ticket(&admin, &t, AssignTicket)), "ok");
        // Every member may work an unclaimed team ticket.
        for p in WRITES {
            assert_eq!(code(authorize_ticket(&member, &t, p)), "ok", "{p:?}");
        }
    }

    #[test]
    fn ai_and_draft_states_hide_from_operators_and_final_states_are_read_only() {
        use TicketStatus::*;
        let op = actor(2, Operator, None);
        let admin = actor(3, Admin, None);
        for status in [Draft, AiProcessing, AwaitingAiDecision, AiSolved] {
            let t = ticket(1, None, None, status);
            assert_eq!(ticket_view(&op, &t), None, "{status:?}");
            assert_eq!(code(authorize_ticket(&op, &t, ReplyAsStaff)), "not_found");
            assert_eq!(
                ticket_view(&admin, &t).is_some(),
                status != Draft,
                "{status:?}"
            );
        }
        for status in [Closed, Cancelled, AiSolved] {
            let t = ticket(1, None, None, status);
            for p in permissions() {
                assert_eq!(
                    code(authorize_ticket(&admin, &t, p)),
                    "forbidden",
                    "{status:?} {p:?}"
                );
            }
        }
    }

    #[test]
    fn non_ticket_permissions_never_authorize_ticket_actions() {
        let t = ticket(1, None, None, TicketStatus::Queued);
        let admin = actor(2, Admin, None);
        for p in [SubmitTicket, ViewGeneralQueue, ManageAccounts, ViewReports] {
            assert_eq!(code(authorize_ticket(&admin, &t, p)), "forbidden", "{p:?}");
        }
    }
}
