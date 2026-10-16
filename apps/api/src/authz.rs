//! Role/permission matrix (issue #11) as pure, side-effect-free functions.
//!
//! Every ticket, admin and configuration endpoint must call one of these
//! functions *server-side* before reading or mutating a resource; client
//! route guards are navigation aids only. The functions take an already
//! loaded [`Principal`] (reloaded from the database on every request, so role
//! changes, team changes and deactivation apply immediately) and a resource
//! context such as [`TicketAccess`]. They never touch the database, so the
//! whole matrix is unit-tested exhaustively below. Database triggers in
//! `migrations/0001_initial_schema.sql` enforce the structural invariants
//! (team membership, leadership, assignee eligibility, AI-solved finality) a
//! second time.
//!
//! A denial is a [`Denial`]:
//!
//! - [`Denial::NotVisible`] → 404 `not_found`: the actor may not even know the
//!   resource exists (another requester's ticket, another team's ticket,
//!   AI-solved records for operators, drafts for staff).
//! - [`Denial::Forbidden`] → 403 `forbidden`: the actor may see the resource
//!   but not perform this action (e.g. a team member writing on a ticket
//!   claimed by someone else, any staff action on one's own ticket).
//! - [`Denial::InvalidState`] → 409 `conflict`: allowed in principle but not
//!   in the resource's current state (closed, AI-solved, already claimed).
//! - [`Denial::InvalidTarget`] → 422 `validation_failed`: the chosen target
//!   (assignee, destination team) is not eligible.
//!
//! Readable form of the matrix: `docs/permissions.md`.
//!
//! Interpretation decisions (see the doc for the reasoning):
//!
//! - Team Lead authority comes from the *leadership flag of the current
//!   membership*, not from the role name: a `team_lead`-role account that is
//!   a plain member (e.g. a replaced lead) has member permissions only.
//! - In the general queue, non-admin staff may view and dispatch; every other
//!   staff write there (replies, notes, priority, proposals) is Admin-only.
//! - On an unclaimed team ticket, plain members may view and self-claim; other
//!   staff writes need the team's lead or an Admin (claim first, then work).
//! - The submitting account gets no staff view of its own ticket at all
//!   (403); it uses the requester view, which never contains internal content.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::Role,
    error::{ApiError, ErrorCode},
};

// ---------------------------------------------------------------------------
// Principal
// ---------------------------------------------------------------------------

/// The principal's current team membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeamMembershipRef {
    pub team_id: Uuid,
    /// The current membership carries the (single) leadership of the team.
    pub is_lead: bool,
}

/// The authenticated account as loaded for this request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub account_id: Uuid,
    pub role: Role,
    /// Current team membership (never present for Admins).
    pub team: Option<TeamMembershipRef>,
    pub active: bool,
}

impl Principal {
    fn is_admin(&self) -> bool {
        self.active && self.role == Role::Admin
    }

    fn is_staff(&self) -> bool {
        self.active && self.role.is_staff()
    }

    /// Eligible current member of `team` (claim/assignment/read scope).
    pub fn is_member_of(&self, team: Uuid) -> bool {
        self.active && self.role.is_team_eligible() && self.team.is_some_and(|m| m.team_id == team)
    }

    /// The designated lead of `team`: leadership flag *and* Team Lead role.
    pub fn leads(&self, team: Uuid) -> bool {
        self.is_member_of(team)
            && self.role == Role::TeamLead
            && self.team.is_some_and(|m| m.is_lead)
    }
}

// ---------------------------------------------------------------------------
// Ticket and policy context
// ---------------------------------------------------------------------------

/// Mirrors `tickets.status` in the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    Draft,
    AiProcessing,
    AwaitingAiDecision,
    Queued,
    InProgress,
    WaitingForRequester,
    AwaitingConfirmation,
    Closed,
    Cancelled,
}

impl TicketStatus {
    pub const ALL: [TicketStatus; 9] = [
        Self::Draft,
        Self::AiProcessing,
        Self::AwaitingAiDecision,
        Self::Queued,
        Self::InProgress,
        Self::WaitingForRequester,
        Self::AwaitingConfirmation,
        Self::Closed,
        Self::Cancelled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::AiProcessing => "ai_processing",
            Self::AwaitingAiDecision => "awaiting_ai_decision",
            Self::Queued => "queued",
            Self::InProgress => "in_progress",
            Self::WaitingForRequester => "waiting_for_requester",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// Before the human handoff: the draft and the AI phase.
    pub const fn is_pre_handoff(self) -> bool {
        matches!(
            self,
            Self::Draft | Self::AiProcessing | Self::AwaitingAiDecision
        )
    }

    /// Open human work: the ticket is with the help desk.
    pub const fn is_active_human_work(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::InProgress
                | Self::WaitingForRequester
                | Self::AwaitingConfirmation
        )
    }

    /// Closed (any resolution kind) or cancelled.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Cancelled)
    }
}

/// Mirrors `tickets.resolution_kind` (set exactly when `status = closed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    /// The requester explicitly marked the AI suggestion "Solved". Final.
    AiConfirmed,
    /// The requester confirmed a staff resolution proposal.
    RequesterConfirmed,
    /// The confirmation timeout elapsed (system closure, not acceptance).
    AutoClosedTimeout,
}

impl ResolutionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AiConfirmed => "ai_confirmed",
            Self::RequesterConfirmed => "requester_confirmed",
            Self::AutoClosedTimeout => "auto_closed_timeout",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ai_confirmed" => Some(Self::AiConfirmed),
            "requester_confirmed" => Some(Self::RequesterConfirmed),
            "auto_closed_timeout" => Some(Self::AutoClosedTimeout),
            _ => None,
        }
    }
}

/// What authorization needs to know about a ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketAccess {
    /// Immutable owning account (`tickets.requester_account_id`).
    pub requester_id: Uuid,
    pub status: TicketStatus,
    pub resolution_kind: Option<ResolutionKind>,
    /// `None` = shared general queue.
    pub team_id: Option<Uuid>,
    pub assignee_id: Option<Uuid>,
    /// A human work-start / In Progress event was ever recorded
    /// (cancellation policy "before work starts"; never reset).
    pub has_work_started: bool,
    /// The ticket was ever handed off to human support (entered the general
    /// queue). Distinguishes e.g. a ticket cancelled during the AI phase,
    /// which operators never see, from one cancelled while queued.
    pub handed_off: bool,
}

impl TicketAccess {
    /// AI-solved records are final: `closed` + `ai_confirmed`.
    pub fn is_ai_solved(&self) -> bool {
        self.status == TicketStatus::Closed
            && self.resolution_kind == Some(ResolutionKind::AiConfirmed)
    }

    fn is_owned_by(&self, principal: &Principal) -> bool {
        self.requester_id == principal.account_id
    }
}

/// Requester cancellation policy (`organization_settings.cancellation_policy`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancellationPolicy {
    #[default]
    AnytimeWhileOpen,
    BeforeWorkStarts,
    Disabled,
}

impl CancellationPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnytimeWhileOpen => "anytime_while_open",
            Self::BeforeWorkStarts => "before_work_starts",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "anytime_while_open" => Some(Self::AnytimeWhileOpen),
            "before_work_starts" => Some(Self::BeforeWorkStarts),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// Organization settings that influence authorization. `Default` matches the
/// schema defaults (reopen off, cancel anytime while open, no requester skip).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrgPolicy {
    pub reopen_closed_tickets_enabled: bool,
    pub cancellation_policy: CancellationPolicy,
    pub allow_requester_skip_ai: bool,
}

/// A destination team for dispatch/transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeamTarget {
    pub team_id: Uuid,
    pub active: bool,
}

// ---------------------------------------------------------------------------
// Denials
// ---------------------------------------------------------------------------

/// Why an action was denied. See the module docs for the HTTP mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Denial {
    /// 404 `not_found`: do not reveal that the resource exists.
    NotVisible,
    /// 403 `forbidden`: visible, but this actor may not do this.
    Forbidden,
    /// 409 `conflict`: not possible in the resource's current state.
    InvalidState,
    /// 422 `validation_failed`: the chosen assignee/team is not eligible.
    InvalidTarget,
}

impl From<Denial> for ApiError {
    fn from(denial: Denial) -> Self {
        match denial {
            Denial::NotVisible => ApiError::not_found(),
            Denial::Forbidden => ApiError::forbidden(),
            Denial::InvalidState => ApiError::new(ErrorCode::Conflict),
            Denial::InvalidTarget => ApiError::new(ErrorCode::ValidationFailed)
                .with_message("The selected target is not eligible."),
        }
    }
}

pub type Authz = Result<(), Denial>;

fn check(condition: bool, denial: Denial) -> Authz {
    if condition { Ok(()) } else { Err(denial) }
}

// ---------------------------------------------------------------------------
// Visibility
// ---------------------------------------------------------------------------

/// Requester portal view (public ticket data only, never internal content).
///
/// Spec "Self-submission", "Requester decision": requesters access only their
/// own records, including read-only history of their own AI-solved
/// submissions and their own drafts. Ownership compares account IDs only.
pub fn can_view_ticket_as_requester(principal: &Principal, ticket: &TicketAccess) -> Authz {
    check(
        principal.active && ticket.is_owned_by(principal),
        Denial::NotVisible,
    )
}

/// Staff view of a ticket (detail, list, search, counts, exports,
/// attachments, realtime, notifications).
///
/// - Users and deactivated accounts: never ([`Denial::NotVisible`]).
/// - Own ticket, any role: [`Denial::Forbidden`]. Self-handling ("requester
///   identity takes precedence over staff permissions on that ticket"): the
///   submitting account uses the requester view.
/// - Drafts: never for staff (not a submission).
/// - Admin: every other persisted submission, including the AI phase and
///   final AI-solved records ("Administrators can inspect all persisted
///   submissions").
/// - Operator/Specialist/Team Lead: only tickets handed off to humans, never
///   AI-solved ones; general-queue tickets for every active staff member;
///   after dispatch only current members of the destination team ("other
///   operators, including the original dispatcher, lose access"). Assignment
///   never bypasses team membership.
pub fn can_view_ticket_as_staff(principal: &Principal, ticket: &TicketAccess) -> Authz {
    if !principal.is_staff() {
        return Err(Denial::NotVisible);
    }
    if ticket.is_owned_by(principal) {
        return Err(Denial::Forbidden);
    }
    if ticket.status == TicketStatus::Draft {
        return Err(Denial::NotVisible);
    }
    if principal.is_admin() {
        return Ok(());
    }
    if ticket.is_ai_solved() || !ticket.handed_off {
        return Err(Denial::NotVisible);
    }
    match ticket.team_id {
        None => Ok(()),
        Some(team) => check(principal.is_member_of(team), Denial::NotVisible),
    }
}

/// Internal notes, staff-only screenshots and staff-only context. Same scope
/// as the staff view; requesters (including staff looking at their own
/// ticket) never get internal content.
pub fn can_view_internal_content(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_view_ticket_as_staff(principal, ticket)
}

/// Access to the shared general intake queue list: every active staff role.
pub fn can_view_general_queue(principal: &Principal) -> Authz {
    check(principal.is_staff(), Denial::Forbidden)
}

/// The set of tickets a principal may see through *staff* list/search/count/
/// export queries. Apply it in SQL before pagination; it is equivalent to
/// [`can_view_ticket_as_staff`] row by row (tested).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaffTicketScope {
    /// No staff access.
    Nothing,
    /// `status <> 'draft' AND requester_account_id <> :me` (Admins).
    Organization { account_id: Uuid },
    /// `status <> 'draft' AND handed off AND NOT ai-solved AND
    /// requester_account_id <> :me AND (team_id IS NULL OR team_id = :team)`.
    GeneralQueueAndTeam {
        account_id: Uuid,
        team_id: Option<Uuid>,
    },
}

impl StaffTicketScope {
    pub fn includes(&self, ticket: &TicketAccess) -> bool {
        match *self {
            Self::Nothing => false,
            Self::Organization { account_id } => {
                ticket.status != TicketStatus::Draft && ticket.requester_id != account_id
            }
            Self::GeneralQueueAndTeam {
                account_id,
                team_id,
            } => {
                ticket.status != TicketStatus::Draft
                    && ticket.handed_off
                    && !ticket.is_ai_solved()
                    && ticket.requester_id != account_id
                    && (ticket.team_id.is_none() || ticket.team_id == team_id)
            }
        }
    }
}

/// See [`StaffTicketScope`].
pub fn staff_ticket_scope(principal: &Principal) -> StaffTicketScope {
    if !principal.is_staff() {
        StaffTicketScope::Nothing
    } else if principal.is_admin() {
        StaffTicketScope::Organization {
            account_id: principal.account_id,
        }
    } else {
        StaffTicketScope::GeneralQueueAndTeam {
            account_id: principal.account_id,
            team_id: principal
                .team
                .map(|m| m.team_id)
                .filter(|_| principal.role.is_team_eligible()),
        }
    }
}

// ---------------------------------------------------------------------------
// Staff handling
// ---------------------------------------------------------------------------

/// Staff may only work on tickets that are with the help desk: not in the
/// AI phase, not closed/cancelled, never AI-solved (final, read-only).
fn open_for_staff_work(ticket: &TicketAccess) -> Authz {
    check(ticket.status.is_active_human_work(), Denial::InvalidState)
}

/// Generic staff mutation: staff replies, internal notes, staff-controlled
/// field/status edits, adding/removing staff screenshots, team transfer and
/// return to the general queue.
///
/// Spec "Human support queues and permissions": for a claimed team ticket,
/// staff writes require the current assignee (still a current member), that
/// team's designated Team Lead, or an Admin; other members are read-only.
/// Unclaimed team tickets: the team's lead or an Admin (members claim first).
/// General queue: Admin only (other staff dispatch, see [`can_dispatch`]).
/// Self-handling and AI-solved finality always apply.
pub fn can_staff_write(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_view_ticket_as_staff(principal, ticket)?;
    open_for_staff_work(ticket)?;
    if principal.is_admin() {
        return Ok(());
    }
    let allowed = match (ticket.team_id, ticket.assignee_id) {
        (None, _) => false,
        (Some(team), None) => principal.leads(team),
        (Some(team), Some(assignee)) => {
            principal.leads(team)
                || (assignee == principal.account_id && principal.is_member_of(team))
        }
    };
    check(allowed, Denial::Forbidden)
}

/// Change the effective priority. Team Leads "prioritize" within their team;
/// otherwise the claimed-ticket write rule applies.
pub fn can_change_priority(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_staff_write(principal, ticket)
}

/// Propose a resolution (enters awaiting requester confirmation). A newer
/// proposal while awaiting confirmation supersedes the old one.
pub fn can_propose_resolution(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_staff_write(principal, ticket)
}

/// Self-claim an unassigned team ticket.
///
/// Spec: "An eligible active member can claim it". Admins never claim (they
/// cannot be assignees); general-queue tickets must be dispatched first; an
/// already claimed ticket cannot be taken over by claiming.
pub fn can_claim(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_view_ticket_as_staff(principal, ticket)?;
    if !principal.role.is_team_eligible() {
        return Err(Denial::Forbidden);
    }
    open_for_staff_work(ticket)?;
    let Some(team) = ticket.team_id else {
        return Err(Denial::InvalidState);
    };
    check(principal.is_member_of(team), Denial::NotVisible)?;
    check(ticket.assignee_id.is_none(), Denial::InvalidState)
}

/// Assign or reassign a team ticket to `target`.
///
/// Spec: "that team's Team Lead or an Admin can explicitly assign it to an
/// eligible member". The target must be an active, eligible current member
/// of the ticket's team and not the requester (self-handling); Admins can
/// never be assignees.
pub fn can_assign(principal: &Principal, ticket: &TicketAccess, target: &Principal) -> Authz {
    can_view_ticket_as_staff(principal, ticket)?;
    open_for_staff_work(ticket)?;
    let Some(team) = ticket.team_id else {
        return Err(Denial::InvalidState);
    };
    check(
        principal.is_admin() || principal.leads(team),
        Denial::Forbidden,
    )?;
    check(
        target.is_member_of(team) && target.account_id != ticket.requester_id,
        Denial::InvalidTarget,
    )
}

/// Remove the individual assignee (the ticket stays with its team).
/// Team Lead of that team or Admin.
pub fn can_unassign(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_view_ticket_as_staff(principal, ticket)?;
    open_for_staff_work(ticket)?;
    let (Some(team), Some(_)) = (ticket.team_id, ticket.assignee_id) else {
        return Err(Denial::InvalidState);
    };
    check(
        principal.is_admin() || principal.leads(team),
        Denial::Forbidden,
    )
}

/// Send a ticket to `target` team.
///
/// - From the general queue: any active Operator, Specialist, Team Lead or
///   Admin with general-queue access ("General-queue dispatch ... retain[s]
///   [its] established permissions").
/// - From a team (transfer): the claimed-ticket write rule
///   ([`can_staff_write`]); the issue lists "dispatch/reassign" among the
///   writes other members may not perform on a claimed ticket.
///
/// The destination must be an active team other than the current one. Never
/// on one's own ticket.
pub fn can_dispatch(principal: &Principal, ticket: &TicketAccess, target: TeamTarget) -> Authz {
    can_view_ticket_as_staff(principal, ticket)?;
    open_for_staff_work(ticket)?;
    match ticket.team_id {
        None => {}
        Some(_) => can_staff_write(principal, ticket)?,
    }
    check(
        target.active && ticket.team_id != Some(target.team_id),
        Denial::InvalidTarget,
    )
}

/// Return a team ticket to the general queue (clears team and assignee).
pub fn can_return_to_general_queue(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_view_ticket_as_staff(principal, ticket)?;
    open_for_staff_work(ticket)?;
    check(ticket.team_id.is_some(), Denial::InvalidState)?;
    can_staff_write(principal, ticket)
}

// ---------------------------------------------------------------------------
// Requester actions
// ---------------------------------------------------------------------------

/// Requester actions belong to the owning account only. Non-owners who may
/// see the ticket as staff get 403, everyone else 404.
fn owning_requester(principal: &Principal, ticket: &TicketAccess) -> Authz {
    if !principal.active {
        return Err(Denial::NotVisible);
    }
    if ticket.is_owned_by(principal) {
        return Ok(());
    }
    match can_view_ticket_as_staff(principal, ticket) {
        Ok(()) => Err(Denial::Forbidden),
        Err(_) => Err(Denial::NotVisible),
    }
}

/// Public reply by the requester. Spec "Conversations and resolution":
/// requester updates are append-only public replies on a permitted open
/// ticket; the AI phase has one immutable input and no chat; closed,
/// cancelled and AI-solved records accept no replies (reopen first where
/// allowed).
pub fn can_requester_reply(principal: &Principal, ticket: &TicketAccess) -> Authz {
    owning_requester(principal, ticket)?;
    check(ticket.status.is_active_human_work(), Denial::InvalidState)
}

/// Additional requester screenshots: same rule as replies. Original intake
/// screenshots are frozen and can never be removed.
pub fn can_requester_add_screenshots(principal: &Principal, ticket: &TicketAccess) -> Authz {
    can_requester_reply(principal, ticket)
}

/// Accept or reject ("Still not solved") the current staff resolution
/// proposal. Only the owning requester, only while awaiting confirmation; no
/// staff or Admin permission impersonates requester confirmation.
pub fn can_confirm_or_reject_resolution(principal: &Principal, ticket: &TicketAccess) -> Authz {
    owning_requester(principal, ticket)?;
    check(
        ticket.status == TicketStatus::AwaitingConfirmation,
        Denial::InvalidState,
    )
}

/// Decide on the persisted AI suggestion ("Solved" / "Still need help").
/// Only the owning requester; only the explicit decision marks AI-solved.
pub fn can_decide_ai_suggestion(principal: &Principal, ticket: &TicketAccess) -> Authz {
    owning_requester(principal, ticket)?;
    check(
        ticket.status == TicketStatus::AwaitingAiDecision,
        Denial::InvalidState,
    )
}

/// Requester skips AI at submission. Only allowed when the organization
/// toggle is on (Admin "Skip AI" categories are a separate, policy-driven
/// path that needs no requester permission).
pub fn can_requester_skip_ai(
    principal: &Principal,
    ticket: &TicketAccess,
    policy: &OrgPolicy,
) -> Authz {
    owning_requester(principal, ticket)?;
    check(ticket.status == TicketStatus::Draft, Denial::InvalidState)?;
    check(policy.allow_requester_skip_ai, Denial::Forbidden)
}

/// Requester cancellation (spec "Requester cancellation"): owning requester,
/// submitted and still open (not closed, cancelled or AI-solved), subject to
/// the Admin policy. "Before work starts" means no human work-start event was
/// ever recorded. The nonblank bounded reason is validated separately
/// ([`validate_cancellation_reason`]).
pub fn can_cancel(principal: &Principal, ticket: &TicketAccess, policy: &OrgPolicy) -> Authz {
    owning_requester(principal, ticket)?;
    check(
        ticket.status != TicketStatus::Draft && !ticket.status.is_terminal(),
        Denial::InvalidState,
    )?;
    match policy.cancellation_policy {
        CancellationPolicy::AnytimeWhileOpen => Ok(()),
        CancellationPolicy::BeforeWorkStarts => {
            check(!ticket.has_work_started, Denial::InvalidState)
        }
        CancellationPolicy::Disabled => Err(Denial::Forbidden),
    }
}

/// Maximum cancellation reason length (characters).
pub const CANCELLATION_REASON_MAX: usize = 1000;

/// Validates a cancellation reason: trimmed, nonblank, at most
/// [`CANCELLATION_REASON_MAX`] characters. Returns the trimmed reason.
pub fn validate_cancellation_reason(reason: &str) -> Result<String, ApiError> {
    use crate::error::{FieldError, FieldErrorCode};
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err(ApiError::field("reason", FieldErrorCode::Required));
    }
    if trimmed.chars().count() > CANCELLATION_REASON_MAX {
        return Err(ApiError::validation(vec![
            FieldError::new("reason", FieldErrorCode::TooLong)
                .with_param("max", CANCELLATION_REASON_MAX),
        ]));
    }
    Ok(trimmed.to_owned())
}

/// Reopen a human-closed ticket (spec "Reopening").
///
/// Requires the Admin toggle (default off; no role bypasses it) *and* a
/// normal permission: the owning requester, or staff under the claimed-ticket
/// write rule (current assignee, the team's lead, Admin). AI-solved records
/// are never reopenable; cancelled tickets are not "closed".
pub fn can_reopen(principal: &Principal, ticket: &TicketAccess, policy: &OrgPolicy) -> Authz {
    if !principal.active {
        return Err(Denial::NotVisible);
    }
    let owner = ticket.is_owned_by(principal);
    if !owner {
        can_view_ticket_as_staff(principal, ticket)?;
    }
    check(
        ticket.status == TicketStatus::Closed && !ticket.is_ai_solved(),
        Denial::InvalidState,
    )?;
    check(policy.reopen_closed_tickets_enabled, Denial::Forbidden)?;
    if owner || principal.is_admin() {
        return Ok(());
    }
    let allowed = match (ticket.team_id, ticket.assignee_id) {
        (None, _) => false,
        (Some(team), None) => principal.leads(team),
        (Some(team), Some(assignee)) => {
            principal.leads(team)
                || (assignee == principal.account_id && principal.is_member_of(team))
        }
    };
    check(allowed, Denial::Forbidden)
}

/// Create a ticket for `requester_id`. Every role submits only for its own
/// signed-in account; there is no submission on behalf of others.
pub fn can_submit_ticket_for(principal: &Principal, requester_id: Uuid) -> Authz {
    check(
        principal.active && principal.account_id == requester_id,
        Denial::Forbidden,
    )
}

// ---------------------------------------------------------------------------
// Organization administration
// ---------------------------------------------------------------------------

/// Roles, invitations, teams/leadership, categories, organization settings,
/// AI/BYOK provider settings, SMTP, reporting and exports: Admin only.
/// Team Leads have no user-role administration or global settings.
pub fn can_manage_org(principal: &Principal) -> Authz {
    check(principal.is_admin(), Denial::Forbidden)
}

/// The Admin submission view (AI attempts, AI-solved records, escalations,
/// human resolutions, cancellations). Viewing never mutates a record.
pub fn can_view_admin_submissions(principal: &Principal) -> Authz {
    can_manage_org(principal)
}

#[cfg(test)]
mod tests;
