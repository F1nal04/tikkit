//! Exhaustive permission matrix tests.
//!
//! Every principal kind × ownership × queue location × ticket state × policy
//! × action is evaluated. Invariants from issue #11 are asserted over the
//! whole matrix (one test per acceptance bullet), and a golden table pins the
//! concrete outcomes for active human work.

use super::*;
use crate::domain::AccountStatus;
use crate::error::ErrorCode;

const ME: Uuid = Uuid::from_u128(1);
const OTHER_REQUESTER: Uuid = Uuid::from_u128(2);
/// Plain Operator member of TEAM_A.
const PEER: Uuid = Uuid::from_u128(3);
/// Plain Operator member of TEAM_B.
const OUTSIDER: Uuid = Uuid::from_u128(4);
/// The principals' team (Operator/Specialist/Team Lead fixtures belong to it).
const TEAM_A: Uuid = Uuid::from_u128(100);
const TEAM_B: Uuid = Uuid::from_u128(200);
/// Active dispatch destination, distinct from both.
const TEAM_C: Uuid = Uuid::from_u128(300);

/// Who is acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    User,
    Operator,
    Specialist,
    /// Team Lead role leading TEAM_A.
    TeamLead,
    /// Team Lead role, plain member of TEAM_A (e.g. a replaced lead).
    TeamLeadNotLeading,
    Admin,
}

const KINDS: [Kind; 6] = [
    Kind::User,
    Kind::Operator,
    Kind::Specialist,
    Kind::TeamLead,
    Kind::TeamLeadNotLeading,
    Kind::Admin,
];

fn principal(kind: Kind, active: bool) -> Principal {
    let member = |is_lead| {
        Some(TeamMembershipRef {
            team_id: TEAM_A,
            is_lead,
        })
    };
    let (role, team) = match kind {
        Kind::User => (Role::User, None),
        Kind::Operator => (Role::Operator, member(false)),
        Kind::Specialist => (Role::Specialist, member(false)),
        Kind::TeamLead => (Role::TeamLead, member(true)),
        Kind::TeamLeadNotLeading => (Role::TeamLead, member(false)),
        Kind::Admin => (Role::Admin, None),
    };
    Principal {
        account_id: ME,
        role,
        team,
        active,
    }
}

fn member(id: Uuid, team: Uuid) -> Principal {
    Principal {
        account_id: id,
        role: Role::Operator,
        team: Some(TeamMembershipRef {
            team_id: team,
            is_lead: false,
        }),
        active: true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Location {
    GeneralQueue,
    OwnTeamUnclaimed,
    OwnTeamClaimedBySelf,
    OwnTeamClaimedByOther,
    OtherTeamUnclaimed,
    OtherTeamClaimed,
}

const LOCATIONS: [Location; 6] = [
    Location::GeneralQueue,
    Location::OwnTeamUnclaimed,
    Location::OwnTeamClaimedBySelf,
    Location::OwnTeamClaimedByOther,
    Location::OtherTeamUnclaimed,
    Location::OtherTeamClaimed,
];

impl Location {
    fn routing(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::GeneralQueue => (None, None),
            Self::OwnTeamUnclaimed => (Some(TEAM_A), None),
            Self::OwnTeamClaimedBySelf => (Some(TEAM_A), Some(ME)),
            Self::OwnTeamClaimedByOther => (Some(TEAM_A), Some(PEER)),
            Self::OtherTeamUnclaimed => (Some(TEAM_B), None),
            Self::OtherTeamClaimed => (Some(TEAM_B), Some(OUTSIDER)),
        }
    }

    fn is_other_team(self) -> bool {
        matches!(self, Self::OtherTeamUnclaimed | Self::OtherTeamClaimed)
    }
}

/// A ticket state: status + resolution + whether it was handed off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    status: TicketStatus,
    resolution: Option<ResolutionKind>,
    handed_off: bool,
}

fn states() -> Vec<State> {
    use ResolutionKind::*;
    use TicketStatus::*;
    let s = |status, resolution, handed_off| State {
        status,
        resolution,
        handed_off,
    };
    vec![
        s(Draft, None, false),
        s(AiProcessing, None, false),
        s(AwaitingAiDecision, None, false),
        s(Queued, None, true),
        s(InProgress, None, true),
        s(WaitingForRequester, None, true),
        s(AwaitingConfirmation, None, true),
        s(Closed, Some(RequesterConfirmed), true),
        s(Closed, Some(AutoClosedTimeout), true),
        s(Closed, Some(AiConfirmed), false),
        s(Cancelled, None, true),
        s(Cancelled, None, false),
    ]
}

impl State {
    fn ai_solved(self) -> bool {
        self.resolution == Some(ResolutionKind::AiConfirmed)
    }
    /// States that can only exist with no team and no assignee.
    fn unrouted_only(self) -> bool {
        self.status.is_pre_handoff() || self.ai_solved() || !self.handed_off
    }
}

#[derive(Clone, Copy, Debug)]
struct Scenario {
    kind: Kind,
    active: bool,
    own: bool,
    location: Location,
    state: State,
    work_started: bool,
}

impl Scenario {
    fn principal(&self) -> Principal {
        principal(self.kind, self.active)
    }

    fn ticket(&self) -> TicketAccess {
        let (team_id, assignee_id) = self.location.routing();
        TicketAccess {
            requester_id: if self.own { ME } else { OTHER_REQUESTER },
            status: self.state.status,
            resolution_kind: self.state.resolution,
            team_id,
            assignee_id,
            has_work_started: self.work_started,
            handed_off: self.state.handed_off,
        }
    }

    /// The eligible assignment target for this ticket's team.
    fn assign_target(&self) -> Principal {
        match self.location.routing().0 {
            Some(TEAM_B) => member(OUTSIDER, TEAM_B),
            _ => member(PEER, TEAM_A),
        }
    }
}

/// Every valid scenario (combinations the schema forbids are skipped).
fn scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    for kind in KINDS {
        for active in [true, false] {
            for own in [false, true] {
                for location in LOCATIONS {
                    for state in states() {
                        for work_started in [false, true] {
                            if state.unrouted_only() && location != Location::GeneralQueue {
                                continue;
                            }
                            if work_started && !state.handed_off {
                                continue;
                            }
                            // The requester can never be the assignee, and only
                            // eligible members can be assigned at all.
                            if location == Location::OwnTeamClaimedBySelf
                                && (own || !principal(kind, true).role.is_team_eligible())
                            {
                                continue;
                            }
                            out.push(Scenario {
                                kind,
                                active,
                                own,
                                location,
                                state,
                                work_started,
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

fn policies() -> Vec<OrgPolicy> {
    let mut out = Vec::new();
    for reopen in [false, true] {
        for cancellation_policy in [
            CancellationPolicy::AnytimeWhileOpen,
            CancellationPolicy::BeforeWorkStarts,
            CancellationPolicy::Disabled,
        ] {
            for skip in [false, true] {
                out.push(OrgPolicy {
                    reopen_closed_tickets_enabled: reopen,
                    cancellation_policy,
                    allow_requester_skip_ai: skip,
                });
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    ViewAsRequester,
    ViewAsStaff,
    ViewInternal,
    Claim,
    Assign,
    Unassign,
    Dispatch,
    ReturnToQueue,
    StaffWrite,
    ChangePriority,
    ProposeResolution,
    RequesterReply,
    RequesterScreenshots,
    ConfirmOrReject,
    DecideAi,
    SkipAi,
    Cancel,
    Reopen,
}

const ACTIONS: [Action; 18] = [
    Action::ViewAsRequester,
    Action::ViewAsStaff,
    Action::ViewInternal,
    Action::Claim,
    Action::Assign,
    Action::Unassign,
    Action::Dispatch,
    Action::ReturnToQueue,
    Action::StaffWrite,
    Action::ChangePriority,
    Action::ProposeResolution,
    Action::RequesterReply,
    Action::RequesterScreenshots,
    Action::ConfirmOrReject,
    Action::DecideAi,
    Action::SkipAi,
    Action::Cancel,
    Action::Reopen,
];

/// Staff handling actions (subject to self-handling).
const STAFF_ACTIONS: [Action; 10] = [
    Action::ViewAsStaff,
    Action::ViewInternal,
    Action::Claim,
    Action::Assign,
    Action::Unassign,
    Action::Dispatch,
    Action::ReturnToQueue,
    Action::StaffWrite,
    Action::ChangePriority,
    Action::ProposeResolution,
];

/// Actions that change a ticket.
const MUTATIONS: [Action; 15] = [
    Action::Claim,
    Action::Assign,
    Action::Unassign,
    Action::Dispatch,
    Action::ReturnToQueue,
    Action::StaffWrite,
    Action::ChangePriority,
    Action::ProposeResolution,
    Action::RequesterReply,
    Action::RequesterScreenshots,
    Action::ConfirmOrReject,
    Action::DecideAi,
    Action::SkipAi,
    Action::Cancel,
    Action::Reopen,
];

fn eval(action: Action, scenario: &Scenario, policy: &OrgPolicy) -> Authz {
    let p = scenario.principal();
    let t = scenario.ticket();
    let target_team = TeamTarget {
        team_id: TEAM_C,
        active: true,
    };
    match action {
        Action::ViewAsRequester => can_view_ticket_as_requester(&p, &t),
        Action::ViewAsStaff => can_view_ticket_as_staff(&p, &t),
        Action::ViewInternal => can_view_internal_content(&p, &t),
        Action::Claim => can_claim(&p, &t),
        Action::Assign => can_assign(&p, &t, &scenario.assign_target()),
        Action::Unassign => can_unassign(&p, &t),
        Action::Dispatch => can_dispatch(&p, &t, target_team),
        Action::ReturnToQueue => can_return_to_general_queue(&p, &t),
        Action::StaffWrite => can_staff_write(&p, &t),
        Action::ChangePriority => can_change_priority(&p, &t),
        Action::ProposeResolution => can_propose_resolution(&p, &t),
        Action::RequesterReply => can_requester_reply(&p, &t),
        Action::RequesterScreenshots => can_requester_add_screenshots(&p, &t),
        Action::ConfirmOrReject => can_confirm_or_reject_resolution(&p, &t),
        Action::DecideAi => can_decide_ai_suggestion(&p, &t),
        Action::SkipAi => can_requester_skip_ai(&p, &t, policy),
        Action::Cancel => can_cancel(&p, &t, policy),
        Action::Reopen => can_reopen(&p, &t, policy),
    }
}

/// Runs `f` for every scenario × policy × action.
fn for_all(mut f: impl FnMut(&Scenario, &OrgPolicy, Action, Authz)) {
    let policies = policies();
    for scenario in scenarios() {
        for policy in &policies {
            for action in ACTIONS {
                f(&scenario, policy, action, eval(action, &scenario, policy));
            }
        }
    }
}

#[test]
fn matrix_is_large_enough_to_be_meaningful() {
    let n = scenarios().len();
    assert!(n > 500, "only {n} scenarios");
}

// --- Role definitions --------------------------------------------------------

#[test]
fn operator_and_specialist_have_identical_permissions() {
    let policies = policies();
    for scenario in scenarios().into_iter().filter(|s| s.kind == Kind::Operator) {
        let specialist = Scenario {
            kind: Kind::Specialist,
            ..scenario
        };
        for policy in &policies {
            for action in ACTIONS {
                assert_eq!(
                    eval(action, &scenario, policy),
                    eval(action, &specialist, policy),
                    "{action:?} {scenario:?}"
                );
            }
        }
    }
    // Scope queries agree too.
    let op = staff_ticket_scope(&principal(Kind::Operator, true));
    let sp = staff_ticket_scope(&principal(Kind::Specialist, true));
    assert_eq!(op, sp);
}

#[test]
fn team_lead_authority_comes_from_leadership_not_role_name() {
    // A team_lead-role account that does not lead behaves exactly like an
    // Operator member of the same team.
    let policies = policies();
    for scenario in scenarios().into_iter().filter(|s| s.kind == Kind::Operator) {
        let not_leading = Scenario {
            kind: Kind::TeamLeadNotLeading,
            ..scenario
        };
        for policy in &policies {
            for action in ACTIONS {
                assert_eq!(
                    eval(action, &scenario, policy),
                    eval(action, &not_leading, policy),
                    "{action:?} {scenario:?}"
                );
            }
        }
    }
}

#[test]
fn role_identifiers_round_trip() {
    for role in Role::ALL {
        assert_eq!(Role::parse(role.as_str()), Some(role));
        assert_eq!(
            serde_json::to_value(role).unwrap(),
            serde_json::json!(role.as_str())
        );
    }
    assert_eq!(Role::parse("Admin"), None);
    assert_eq!(Role::parse("superuser"), None);
    for status in TicketStatus::ALL {
        assert_eq!(TicketStatus::parse(status.as_str()), Some(status));
    }
    for kind in [
        ResolutionKind::AiConfirmed,
        ResolutionKind::RequesterConfirmed,
        ResolutionKind::AutoClosedTimeout,
    ] {
        assert_eq!(ResolutionKind::parse(kind.as_str()), Some(kind));
    }
    for policy in [
        CancellationPolicy::AnytimeWhileOpen,
        CancellationPolicy::BeforeWorkStarts,
        CancellationPolicy::Disabled,
    ] {
        assert_eq!(CancellationPolicy::parse(policy.as_str()), Some(policy));
    }
    for status in [AccountStatus::Active, AccountStatus::Deactivated] {
        assert_eq!(AccountStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(
        OrgPolicy::default().cancellation_policy.as_str(),
        "anytime_while_open"
    );
    assert!(!OrgPolicy::default().reopen_closed_tickets_enabled);
    assert!(!OrgPolicy::default().allow_requester_skip_ai);
}

// --- Requesters ----------------------------------------------------------------

#[test]
fn requesters_access_only_their_own_records() {
    for_all(|s, _, action, result| {
        if !s.own && action == Action::ViewAsRequester {
            assert_eq!(result, Err(Denial::NotVisible), "{s:?}");
        }
        if s.own && s.active && action == Action::ViewAsRequester {
            assert_eq!(
                result,
                Ok(()),
                "own records incl. AI-solved and drafts: {s:?}"
            );
        }
    });
}

#[test]
fn requesters_cannot_see_internal_content() {
    for_all(|s, _, action, result| {
        if action == Action::ViewInternal && (s.kind == Kind::User || s.own) {
            assert!(result.is_err(), "{s:?}");
        }
    });
}

#[test]
fn users_have_no_staff_access() {
    for_all(|s, _, action, result| {
        if s.kind == Kind::User && STAFF_ACTIONS.contains(&action) {
            assert_eq!(result, Err(Denial::NotVisible), "{action:?} {s:?}");
        }
    });
    assert_eq!(
        can_view_general_queue(&principal(Kind::User, true)),
        Err(Denial::Forbidden)
    );
    assert_eq!(
        staff_ticket_scope(&principal(Kind::User, true)),
        StaffTicketScope::Nothing
    );
}

#[test]
fn requester_content_updates_need_an_open_handed_off_ticket() {
    for_all(|s, _, action, result| {
        if matches!(
            action,
            Action::RequesterReply | Action::RequesterScreenshots
        ) {
            let expected_ok = s.own && s.active && s.state.status.is_active_human_work();
            assert_eq!(result.is_ok(), expected_ok, "{action:?} {s:?}");
        }
    });
}

// --- AI-solved finality ----------------------------------------------------------

#[test]
fn ai_solved_records_cannot_be_reopened_or_escalated_by_any_role() {
    for_all(|s, _, action, result| {
        if s.state.ai_solved() && MUTATIONS.contains(&action) {
            assert!(result.is_err(), "{action:?} {s:?}");
        }
    });
}

#[test]
fn requester_visibility_of_ai_solved_is_read_only() {
    for_all(|s, _, action, result| {
        if s.state.ai_solved() && s.own && s.active {
            let expected = action == Action::ViewAsRequester;
            assert_eq!(result.is_ok(), expected, "{action:?} {s:?}");
        }
    });
}

#[test]
fn operators_cannot_read_ai_solved_records() {
    for_all(|s, _, action, result| {
        if s.state.ai_solved() && s.kind != Kind::Admin && STAFF_ACTIONS.contains(&action) {
            assert!(result.is_err(), "{action:?} {s:?}");
            if !s.own && s.active {
                assert_eq!(result, Err(Denial::NotVisible), "{action:?} {s:?}");
            }
        }
    });
    for kind in [Kind::Operator, Kind::Specialist, Kind::TeamLead] {
        let scope = staff_ticket_scope(&principal(kind, true));
        let ai_solved = Scenario {
            kind,
            active: true,
            own: false,
            location: Location::GeneralQueue,
            state: State {
                status: TicketStatus::Closed,
                resolution: Some(ResolutionKind::AiConfirmed),
                handed_off: false,
            },
            work_started: false,
        };
        assert!(!scope.includes(&ai_solved.ticket()));
    }
}

// --- Queue and team isolation ------------------------------------------------------

#[test]
fn every_active_operator_can_access_the_general_queue_and_dispatch() {
    for kind in [
        Kind::Operator,
        Kind::Specialist,
        Kind::TeamLead,
        Kind::TeamLeadNotLeading,
        Kind::Admin,
    ] {
        assert_eq!(can_view_general_queue(&principal(kind, true)), Ok(()));
        assert_eq!(
            can_view_general_queue(&principal(kind, false)),
            Err(Denial::Forbidden)
        );
    }
    for_all(|s, _, action, result| {
        let queued_elsewhere = s.location == Location::GeneralQueue
            && s.state.status.is_active_human_work()
            && !s.own
            && s.active
            && s.kind != Kind::User;
        if queued_elsewhere && matches!(action, Action::ViewAsStaff | Action::Dispatch) {
            assert_eq!(result, Ok(()), "{action:?} {s:?}");
        }
    });
}

#[test]
fn dispatch_requires_an_active_different_team() {
    let s = Scenario {
        kind: Kind::Operator,
        active: true,
        own: false,
        location: Location::GeneralQueue,
        state: states()[3],
        work_started: false,
    };
    let (p, t) = (s.principal(), s.ticket());
    let archived = TeamTarget {
        team_id: TEAM_C,
        active: false,
    };
    assert_eq!(can_dispatch(&p, &t, archived), Err(Denial::InvalidTarget));
    let lead = principal(Kind::TeamLead, true);
    let in_team = TicketAccess {
        team_id: Some(TEAM_A),
        ..t
    };
    let same = TeamTarget {
        team_id: TEAM_A,
        active: true,
    };
    assert_eq!(
        can_dispatch(&lead, &in_team, same),
        Err(Denial::InvalidTarget)
    );
}

#[test]
fn after_dispatch_staff_access_is_limited_to_destination_team_and_admins() {
    for_all(|s, _, action, result| {
        if s.location.is_other_team()
            && s.kind != Kind::Admin
            && s.active
            && !s.own
            && STAFF_ACTIONS.contains(&action)
        {
            assert_eq!(result, Err(Denial::NotVisible), "{action:?} {s:?}");
        }
        if s.location.is_other_team()
            && s.kind == Kind::Admin
            && s.active
            && !s.own
            && action == Action::ViewAsStaff
        {
            assert_eq!(result, Ok(()), "{s:?}");
        }
    });
}

#[test]
fn assignment_never_bypasses_team_membership() {
    // An assignee whose membership moved to another team loses access.
    let moved = Principal {
        team: Some(TeamMembershipRef {
            team_id: TEAM_B,
            is_lead: false,
        }),
        ..principal(Kind::Operator, true)
    };
    let ticket = TicketAccess {
        requester_id: OTHER_REQUESTER,
        status: TicketStatus::InProgress,
        resolution_kind: None,
        team_id: Some(TEAM_A),
        assignee_id: Some(ME),
        has_work_started: true,
        handed_off: true,
    };
    assert_eq!(
        can_view_ticket_as_staff(&moved, &ticket),
        Err(Denial::NotVisible)
    );
    assert_eq!(can_staff_write(&moved, &ticket), Err(Denial::NotVisible));
    // The requester still sees their own public ticket.
    let requester = Principal {
        account_id: OTHER_REQUESTER,
        ..principal(Kind::User, true)
    };
    assert_eq!(can_view_ticket_as_requester(&requester, &ticket), Ok(()));
}

#[test]
fn staff_scope_matches_per_ticket_visibility() {
    for s in scenarios() {
        let scope = staff_ticket_scope(&s.principal());
        let visible = can_view_ticket_as_staff(&s.principal(), &s.ticket()).is_ok();
        assert_eq!(scope.includes(&s.ticket()), visible, "{s:?}");
    }
}

#[test]
fn drafts_are_never_visible_to_staff() {
    for_all(|s, _, action, result| {
        if s.state.status == TicketStatus::Draft && STAFF_ACTIONS.contains(&action) {
            assert!(result.is_err(), "{action:?} {s:?}");
        }
    });
}

#[test]
fn pre_handoff_tickets_are_admin_only_and_read_only_for_staff() {
    for_all(|s, _, action, result| {
        if !s.state.handed_off && s.state.status != TicketStatus::Draft && !s.own && s.active {
            match (s.kind, action) {
                (Kind::Admin, Action::ViewAsStaff | Action::ViewInternal) => {
                    assert_eq!(result, Ok(()), "{s:?}")
                }
                (_, a) if STAFF_ACTIONS.contains(&a) => assert!(result.is_err(), "{a:?} {s:?}"),
                _ => {}
            }
        }
    });
}

// --- Admin ------------------------------------------------------------------------

#[test]
fn administrators_can_inspect_all_persisted_submissions() {
    for s in scenarios() {
        if s.kind == Kind::Admin && s.active && !s.own && s.state.status != TicketStatus::Draft {
            assert_eq!(
                can_view_ticket_as_staff(&s.principal(), &s.ticket()),
                Ok(()),
                "{s:?}"
            );
        }
    }
    let admin = principal(Kind::Admin, true);
    assert_eq!(can_manage_org(&admin), Ok(()));
    assert_eq!(can_view_admin_submissions(&admin), Ok(()));
}

#[test]
fn only_active_admins_manage_the_organization() {
    for kind in KINDS {
        for active in [true, false] {
            let p = principal(kind, active);
            let expected = if kind == Kind::Admin && active {
                Ok(())
            } else {
                Err(Denial::Forbidden)
            };
            assert_eq!(can_manage_org(&p), expected, "{kind:?} {active}");
            assert_eq!(
                can_view_admin_submissions(&p),
                expected,
                "{kind:?} {active}"
            );
        }
    }
}

#[test]
fn admins_never_claim_and_are_never_assignees() {
    for_all(|s, _, action, result| {
        if s.kind == Kind::Admin && action == Action::Claim {
            assert!(result.is_err(), "{s:?}");
        }
    });
    let admin_target = Principal {
        account_id: PEER,
        ..principal(Kind::Admin, true)
    };
    let lead = principal(Kind::TeamLead, true);
    let ticket = TicketAccess {
        requester_id: OTHER_REQUESTER,
        status: TicketStatus::Queued,
        resolution_kind: None,
        team_id: Some(TEAM_A),
        assignee_id: None,
        has_work_started: false,
        handed_off: true,
    };
    assert_eq!(
        can_assign(&lead, &ticket, &admin_target),
        Err(Denial::InvalidTarget)
    );
    // Even a (corrupt) admin principal carrying a membership is ineligible.
    let admin_with_team = Principal {
        team: Some(TeamMembershipRef {
            team_id: TEAM_A,
            is_lead: false,
        }),
        ..admin_target
    };
    assert_eq!(
        can_assign(&lead, &ticket, &admin_with_team),
        Err(Denial::InvalidTarget)
    );
}

// --- Team Lead ------------------------------------------------------------------

#[test]
fn team_lead_manages_all_tickets_of_the_led_team_only() {
    for_all(|s, _, action, result| {
        if s.kind != Kind::TeamLead || !s.active || s.own || !s.state.status.is_active_human_work()
        {
            return;
        }
        let in_led_team = matches!(
            s.location,
            Location::OwnTeamUnclaimed
                | Location::OwnTeamClaimedBySelf
                | Location::OwnTeamClaimedByOther
        );
        if in_led_team
            && matches!(
                action,
                Action::StaffWrite
                    | Action::ChangePriority
                    | Action::ProposeResolution
                    | Action::Assign
                    | Action::Dispatch
                    | Action::ReturnToQueue
            )
        {
            assert_eq!(result, Ok(()), "{action:?} {s:?}");
        }
        if in_led_team && action == Action::Unassign {
            let expected = s.location != Location::OwnTeamUnclaimed;
            assert_eq!(result.is_ok(), expected, "{s:?}");
        }
        if s.location.is_other_team() && STAFF_ACTIONS.contains(&action) {
            assert_eq!(result, Err(Denial::NotVisible), "{action:?} {s:?}");
        }
    });
}

#[test]
fn basic_members_self_claim_but_cannot_assign_others() {
    for_all(|s, _, action, result| {
        if !matches!(s.kind, Kind::Operator | Kind::Specialist) || !s.active || s.own {
            return;
        }
        let open = s.state.status.is_active_human_work();
        if action == Action::Claim {
            assert_eq!(
                result.is_ok(),
                open && s.location == Location::OwnTeamUnclaimed,
                "{s:?}"
            );
        }
        if matches!(action, Action::Assign | Action::Unassign) {
            assert!(result.is_err(), "{action:?} {s:?}");
        }
    });
}

#[test]
fn claimed_ticket_writes_are_limited_to_assignee_lead_and_admin() {
    let writes = [
        Action::StaffWrite,
        Action::ChangePriority,
        Action::ProposeResolution,
        Action::Dispatch,
        Action::ReturnToQueue,
        Action::Assign,
        Action::Unassign,
    ];
    for_all(|s, _, action, result| {
        if s.own || !s.active || !s.state.status.is_active_human_work() {
            return;
        }
        if s.location == Location::OwnTeamClaimedByOther && writes.contains(&action) {
            let expected = matches!(s.kind, Kind::TeamLead | Kind::Admin);
            assert_eq!(result.is_ok(), expected, "{action:?} {s:?}");
            if matches!(
                s.kind,
                Kind::Operator | Kind::Specialist | Kind::TeamLeadNotLeading
            ) {
                assert_eq!(result, Err(Denial::Forbidden), "{action:?} {s:?}");
                // Other members keep read access.
                assert_eq!(eval(Action::ViewAsStaff, s, &OrgPolicy::default()), Ok(()));
            }
        }
        if s.location == Location::OwnTeamClaimedBySelf
            && matches!(
                action,
                Action::StaffWrite | Action::ChangePriority | Action::ProposeResolution
            )
        {
            assert_eq!(result, Ok(()), "assignee writes: {action:?} {s:?}");
        }
    });
}

#[test]
fn general_queue_and_unclaimed_team_writes_follow_documented_interpretation() {
    for_all(|s, _, action, result| {
        if s.own || !s.active || !s.state.status.is_active_human_work() {
            return;
        }
        if action != Action::StaffWrite {
            return;
        }
        match s.location {
            Location::GeneralQueue => {
                assert_eq!(result.is_ok(), s.kind == Kind::Admin, "{s:?}");
            }
            Location::OwnTeamUnclaimed => {
                assert_eq!(
                    result.is_ok(),
                    matches!(s.kind, Kind::Admin | Kind::TeamLead),
                    "{s:?}"
                );
            }
            _ => {}
        }
    });
}

#[test]
fn assignment_targets_must_be_eligible_members() {
    let lead = principal(Kind::TeamLead, true);
    let ticket = TicketAccess {
        requester_id: OTHER_REQUESTER,
        status: TicketStatus::InProgress,
        resolution_kind: None,
        team_id: Some(TEAM_A),
        assignee_id: Some(PEER),
        has_work_started: true,
        handed_off: true,
    };
    // Reassign to self (the lead is an eligible member) and to a peer.
    assert_eq!(can_assign(&lead, &ticket, &lead), Ok(()));
    assert_eq!(can_assign(&lead, &ticket, &member(PEER, TEAM_A)), Ok(()));
    // Not a member of this team.
    assert_eq!(
        can_assign(&lead, &ticket, &member(OUTSIDER, TEAM_B)),
        Err(Denial::InvalidTarget)
    );
    // Deactivated member.
    let inactive = Principal {
        active: false,
        ..member(PEER, TEAM_A)
    };
    assert_eq!(
        can_assign(&lead, &ticket, &inactive),
        Err(Denial::InvalidTarget)
    );
    // User role, even with a (corrupt) membership.
    let user = Principal {
        role: Role::User,
        ..member(PEER, TEAM_A)
    };
    assert_eq!(
        can_assign(&lead, &ticket, &user),
        Err(Denial::InvalidTarget)
    );
    // The requester can never be assigned their own ticket.
    let requester_member = member(OTHER_REQUESTER, TEAM_A);
    assert_eq!(
        can_assign(&lead, &ticket, &requester_member),
        Err(Denial::InvalidTarget)
    );
}

// --- Self-handling ----------------------------------------------------------------

#[test]
fn staff_must_not_handle_their_own_submitted_tickets() {
    for_all(|s, _, action, result| {
        if s.own && s.active && s.kind != Kind::User && STAFF_ACTIONS.contains(&action) {
            assert_eq!(result, Err(Denial::Forbidden), "{action:?} {s:?}");
        }
    });
}

#[test]
fn self_handling_preserves_requester_actions() {
    let policy = OrgPolicy {
        reopen_closed_tickets_enabled: true,
        ..OrgPolicy::default()
    };
    for kind in KINDS {
        let p = principal(kind, true);
        let base = TicketAccess {
            requester_id: ME,
            status: TicketStatus::InProgress,
            resolution_kind: None,
            team_id: Some(TEAM_A),
            assignee_id: Some(PEER),
            has_work_started: true,
            handed_off: true,
        };
        assert_eq!(can_requester_reply(&p, &base), Ok(()), "{kind:?}");
        assert_eq!(can_requester_add_screenshots(&p, &base), Ok(()), "{kind:?}");
        assert_eq!(can_cancel(&p, &base, &policy), Ok(()), "{kind:?}");
        let awaiting = TicketAccess {
            status: TicketStatus::AwaitingConfirmation,
            ..base
        };
        assert_eq!(
            can_confirm_or_reject_resolution(&p, &awaiting),
            Ok(()),
            "{kind:?}"
        );
        let closed = TicketAccess {
            status: TicketStatus::Closed,
            resolution_kind: Some(ResolutionKind::RequesterConfirmed),
            ..base
        };
        assert_eq!(can_reopen(&p, &closed, &policy), Ok(()), "{kind:?}");
    }
}

#[test]
fn self_handling_compares_account_ids_only() {
    // A separate Admin account of the same person (same email, different ID)
    // may handle the ticket submitted from the operational account.
    let operational = principal(Kind::Operator, true);
    let separate_admin = Principal {
        account_id: Uuid::from_u128(99),
        ..principal(Kind::Admin, true)
    };
    let ticket = TicketAccess {
        requester_id: operational.account_id,
        status: TicketStatus::Queued,
        resolution_kind: None,
        team_id: None,
        assignee_id: None,
        has_work_started: false,
        handed_off: true,
    };
    assert_eq!(
        can_view_ticket_as_staff(&operational, &ticket),
        Err(Denial::Forbidden)
    );
    assert_eq!(can_staff_write(&separate_admin, &ticket), Ok(()));
    assert_eq!(
        can_dispatch(
            &separate_admin,
            &ticket,
            TeamTarget {
                team_id: TEAM_A,
                active: true
            }
        ),
        Ok(())
    );
}

#[test]
fn every_role_submits_only_for_its_own_account() {
    for kind in KINDS {
        let p = principal(kind, true);
        assert_eq!(can_submit_ticket_for(&p, ME), Ok(()));
        assert_eq!(
            can_submit_ticket_for(&p, OTHER_REQUESTER),
            Err(Denial::Forbidden),
            "{kind:?}"
        );
        assert_eq!(
            can_submit_ticket_for(&principal(kind, false), ME),
            Err(Denial::Forbidden)
        );
    }
}

// --- Resolution, cancellation, reopening ----------------------------------------

#[test]
fn only_the_owning_requester_can_confirm_or_reject_resolution() {
    for_all(|s, _, action, result| {
        if action == Action::ConfirmOrReject {
            let expected =
                s.own && s.active && s.state.status == TicketStatus::AwaitingConfirmation;
            assert_eq!(result.is_ok(), expected, "{s:?}");
        }
        if action == Action::DecideAi {
            let expected = s.own && s.active && s.state.status == TicketStatus::AwaitingAiDecision;
            assert_eq!(result.is_ok(), expected, "{s:?}");
        }
    });
}

#[test]
fn non_owner_requester_actions_do_not_reveal_existence() {
    for_all(|s, _, action, result| {
        let requester_action = matches!(
            action,
            Action::RequesterReply
                | Action::RequesterScreenshots
                | Action::ConfirmOrReject
                | Action::DecideAi
                | Action::SkipAi
                | Action::Cancel
        );
        if requester_action && !s.own && s.active {
            let staff_visible = eval(Action::ViewAsStaff, s, &OrgPolicy::default()).is_ok();
            let expected = if staff_visible {
                Denial::Forbidden
            } else {
                Denial::NotVisible
            };
            assert_eq!(result, Err(expected), "{action:?} {s:?}");
        }
    });
}

#[test]
fn requester_cancellation_follows_policy_and_state() {
    for_all(|s, policy, action, result| {
        if action != Action::Cancel {
            return;
        }
        let open = s.state.status != TicketStatus::Draft && !s.state.status.is_terminal();
        let policy_ok = match policy.cancellation_policy {
            CancellationPolicy::AnytimeWhileOpen => true,
            CancellationPolicy::BeforeWorkStarts => !s.work_started,
            CancellationPolicy::Disabled => false,
        };
        assert_eq!(
            result.is_ok(),
            s.own && s.active && open && policy_ok,
            "{s:?} {policy:?}"
        );
    });
}

#[test]
fn cancellation_reason_is_required_and_bounded() {
    assert_eq!(validate_cancellation_reason("  done  ").unwrap(), "done");
    let blank = validate_cancellation_reason(" \n\t").unwrap_err();
    assert_eq!(blank.code(), ErrorCode::ValidationFailed);
    assert_eq!(blank.fields()[0].field, "reason");
    let long = "x".repeat(CANCELLATION_REASON_MAX + 1);
    assert_eq!(
        validate_cancellation_reason(&long).unwrap_err().code(),
        ErrorCode::ValidationFailed
    );
    assert!(validate_cancellation_reason(&"ä".repeat(CANCELLATION_REASON_MAX)).is_ok());
}

#[test]
fn reopening_requires_the_admin_toggle_and_never_applies_to_ai_solved() {
    for_all(|s, policy, action, result| {
        if action != Action::Reopen {
            return;
        }
        if !policy.reopen_closed_tickets_enabled {
            assert!(result.is_err(), "toggle off: {s:?}");
        }
        if s.state.ai_solved() || s.state.status != TicketStatus::Closed {
            assert!(result.is_err(), "{s:?}");
        }
    });
}

#[test]
fn reopening_with_toggle_requires_normal_permission() {
    let on = OrgPolicy {
        reopen_closed_tickets_enabled: true,
        ..OrgPolicy::default()
    };
    for s in scenarios() {
        if !(s.state.status == TicketStatus::Closed && !s.state.ai_solved()) {
            continue;
        }
        let result = can_reopen(&s.principal(), &s.ticket(), &on);
        let expected = s.active
            && (s.own
                || s.kind == Kind::Admin
                || (s.kind == Kind::TeamLead
                    && matches!(
                        s.location,
                        Location::OwnTeamUnclaimed
                            | Location::OwnTeamClaimedBySelf
                            | Location::OwnTeamClaimedByOther
                    ))
                || s.location == Location::OwnTeamClaimedBySelf);
        assert_eq!(result.is_ok(), expected, "{s:?}");
    }
}

#[test]
fn requester_skip_ai_needs_the_org_toggle() {
    for_all(|s, policy, action, result| {
        if action == Action::SkipAi {
            let expected = s.own
                && s.active
                && s.state.status == TicketStatus::Draft
                && policy.allow_requester_skip_ai;
            assert_eq!(result.is_ok(), expected, "{s:?} {policy:?}");
        }
    });
}

// --- Deactivation -----------------------------------------------------------------

#[test]
fn deactivated_principals_get_nothing() {
    for_all(|s, _, action, result| {
        if !s.active {
            assert!(result.is_err(), "{action:?} {s:?}");
        }
    });
    for kind in KINDS {
        let p = principal(kind, false);
        assert_eq!(staff_ticket_scope(&p), StaffTicketScope::Nothing);
    }
}

// --- Error mapping -----------------------------------------------------------------

#[test]
fn denials_map_to_contract_error_codes() {
    let code = |d: Denial| ApiError::from(d).code();
    assert_eq!(code(Denial::NotVisible), ErrorCode::NotFound);
    assert_eq!(code(Denial::Forbidden), ErrorCode::Forbidden);
    assert_eq!(code(Denial::InvalidState), ErrorCode::Conflict);
    assert_eq!(code(Denial::InvalidTarget), ErrorCode::ValidationFailed);
}

// --- Golden table -------------------------------------------------------------------

/// Outcome letters: Y = allowed, N = 404 not visible, F = 403 forbidden,
/// S = 409 invalid state, T = 422 invalid target.
fn letter(result: Authz) -> char {
    match result {
        Ok(()) => 'Y',
        Err(Denial::NotVisible) => 'N',
        Err(Denial::Forbidden) => 'F',
        Err(Denial::InvalidState) => 'S',
        Err(Denial::InvalidTarget) => 'T',
    }
}

/// Pins concrete outcomes for an in-progress ticket submitted by someone
/// else. Columns: view, internal, claim, assign, unassign, dispatch, return,
/// write, priority, propose, reply, confirm, cancel.
#[test]
fn golden_matrix_for_active_human_work() {
    let columns = [
        Action::ViewAsStaff,
        Action::ViewInternal,
        Action::Claim,
        Action::Assign,
        Action::Unassign,
        Action::Dispatch,
        Action::ReturnToQueue,
        Action::StaffWrite,
        Action::ChangePriority,
        Action::ProposeResolution,
        Action::RequesterReply,
        Action::ConfirmOrReject,
        Action::Cancel,
    ];
    #[rustfmt::skip]
    let expected: &[(Kind, Location, &str)] = &[
        (Kind::User,       Location::GeneralQueue,          "NNNNNNNNNNNNN"),
        (Kind::User,       Location::OwnTeamClaimedByOther, "NNNNNNNNNNNNN"),
        (Kind::Operator,   Location::GeneralQueue,          "YYSSSYSFFFFFF"),
        (Kind::Operator,   Location::OwnTeamUnclaimed,      "YYYFSFFFFFFFF"),
        (Kind::Operator,   Location::OwnTeamClaimedBySelf,  "YYSFFYYYYYFFF"),
        (Kind::Operator,   Location::OwnTeamClaimedByOther, "YYSFFFFFFFFFF"),
        (Kind::Operator,   Location::OtherTeamUnclaimed,    "NNNNNNNNNNNNN"),
        (Kind::Operator,   Location::OtherTeamClaimed,      "NNNNNNNNNNNNN"),
        (Kind::TeamLead,   Location::GeneralQueue,          "YYSSSYSFFFFFF"),
        (Kind::TeamLead,   Location::OwnTeamUnclaimed,      "YYYYSYYYYYFFF"),
        (Kind::TeamLead,   Location::OwnTeamClaimedBySelf,  "YYSYYYYYYYFFF"),
        (Kind::TeamLead,   Location::OwnTeamClaimedByOther, "YYSYYYYYYYFFF"),
        (Kind::TeamLead,   Location::OtherTeamUnclaimed,    "NNNNNNNNNNNNN"),
        (Kind::Admin,      Location::GeneralQueue,          "YYFSSYSYYYFFF"),
        (Kind::Admin,      Location::OwnTeamUnclaimed,      "YYFYSYYYYYFFF"),
        (Kind::Admin,      Location::OtherTeamClaimed,      "YYFYYYYYYYFFF"),
    ];
    for (kind, location, row) in expected {
        let s = Scenario {
            kind: *kind,
            active: true,
            own: false,
            location: *location,
            state: State {
                status: TicketStatus::InProgress,
                resolution: None,
                handed_off: true,
            },
            work_started: true,
        };
        let actual: String = columns
            .iter()
            .map(|a| letter(eval(*a, &s, &OrgPolicy::default())))
            .collect();
        assert_eq!(&actual, row, "{kind:?} {location:?}");
    }

    // The same ticket, submitted by the acting account itself.
    #[rustfmt::skip]
    let own: &[(Kind, &str)] = &[
        (Kind::User,     "NNNNNNNNNNYSY"),
        (Kind::Operator, "FFFFFFFFFFYSY"),
        (Kind::TeamLead, "FFFFFFFFFFYSY"),
        (Kind::Admin,    "FFFFFFFFFFYSY"),
    ];
    for (kind, row) in own {
        let s = Scenario {
            kind: *kind,
            active: true,
            own: true,
            location: Location::OwnTeamClaimedByOther,
            state: State {
                status: TicketStatus::InProgress,
                resolution: None,
                handed_off: true,
            },
            work_started: true,
        };
        let actual: String = columns
            .iter()
            .map(|a| letter(eval(*a, &s, &OrgPolicy::default())))
            .collect();
        assert_eq!(&actual, row, "own {kind:?}");
    }
}
