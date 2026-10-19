//! The ticket lifecycle: one state machine for every status change. See docs/lifecycle.md.
//!
//! [`decide`] is the single authoritative rule. Given the locked ticket, the
//! actor, and the current organization [`Policy`], it accepts or rejects one
//! [`Transition`]; [`Transition::rule`] is its table of who may attempt what
//! from which statuses. [`transition`] runs it inside the caller's transaction:
//! it re-reads the actor's role and team under share locks, locks the ticket row
//! (`FOR UPDATE`), and writes the new state plus one immutable `ticket_events`
//! row. Concurrent opposite decisions serialize on the row lock and the loser
//! sees the new status (`409 conflict`), so they can never both commit.
//!
//! Nothing else may change `tickets.status`: submission, AI jobs, staff work,
//! requester decisions, the auto-close worker, replies, and stale clients all go
//! through [`transition`].

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, Postgres, Transaction};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    api::{ApiError, FieldErrors, TicketStatus},
    auth::{ident, ident_str},
    authz::{self, Actor, Permission, TeamScope, TicketScope, TicketView},
};

pub const SUMMARY_MAX: usize = 5000;
pub const REASON_MAX: usize = 1000;

pub use crate::management::CancellationPolicy;

/// The organization settings transitions depend on, read inside each transition's transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// AI is enabled and configured with a usable credential.
    pub ai_available: bool,
    /// "Allow requester to skip AI".
    pub allow_requester_skip: bool,
    /// "Allow reopening operator-resolved tickets". Never applies to AI-solved records.
    pub allow_reopen: bool,
    pub cancellation: CancellationPolicy,
    /// Elapsed hours from a staff resolution proposal to its automatic closure.
    pub confirmation_timeout_hours: i64,
}

impl Default for Policy {
    /// The spec defaults.
    fn default() -> Self {
        Self {
            ai_available: false,
            allow_requester_skip: false,
            allow_reopen: false,
            cancellation: CancellationPolicy::AnytimeWhileOpen,
            confirmation_timeout_hours: 168,
        }
    }
}

/// The current organization settings, read `FOR SHARE` so a concurrent
/// settings change waits for the transition that relied on them.
pub async fn policy(db: &mut PgConnection) -> Result<Policy, ApiError> {
    let (allow_requester_skip, allow_reopen, cancellation, hours): (bool, bool, String, i32) =
        sqlx::query_as(
            "SELECT allow_requester_ai_skip, allow_reopening, requester_cancellation_policy,
                    resolution_confirmation_hours
             FROM organization WHERE id FOR SHARE",
        )
        .fetch_one(db)
        .await?;
    Ok(Policy {
        // ponytail: AI stays unavailable until AI configuration (#33) is persisted.
        ai_available: false,
        allow_requester_skip,
        allow_reopen,
        cancellation: ident(cancellation)?,
        confirmation_timeout_hours: hours.into(),
    })
}

/// Every allowed status change. Event types (`ticket_events.event_type`) are
/// the snake_case variant names; see [`Transition::event_type`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    /// Requester submits their draft. Rust picks the route: AI, or the general
    /// queue because AI is unavailable, the category is exempt, or the requester
    /// skips (only when the Admin toggle allows it).
    Submit { requester_skip: bool },
    /// AI job persisted the single suggestion.
    AiSuggestionReady,
    /// AI job failed, timed out, or was stopped by policy: hand off to humans.
    AiFailed,
    /// Requester skips AI while it runs; only when the Admin toggle allows it.
    SkipAi,
    /// Requester explicitly confirms the AI suggestion solved it. Final.
    AiSolved,
    /// Requester still needs help after the suggestion.
    AiNotSolved,
    /// Staff starts (or resumes) work on a team ticket.
    StartWork,
    /// Staff waits for information from the requester.
    RequestInformation,
    /// Requester answered a request for information.
    RequesterResponded,
    /// Staff proposes a resolution with a public summary (also supersedes a pending one).
    ProposeResolution { summary: String },
    /// Requester accepts this proposal version: closed, operator resolution.
    AcceptResolution { proposal_version: i32 },
    /// Requester rejects this proposal version: back to work with the same team and assignee.
    /// Not a reopen, so it never needs the reopening toggle.
    RejectResolution { proposal_version: i32 },
    /// Auto-close worker: this proposal version passed its deadline unanswered.
    ConfirmationTimeout { proposal_version: i32 },
    /// Reopen a human-closed ticket; only while the Admin toggle allows it.
    Reopen,
    /// Requester cancels a submitted open ticket with a reason, under the cancellation policy.
    Cancel { reason: String },
}

/// Who may attempt a transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Party {
    /// The ticket's requester, whatever their role.
    Requester,
    /// Staff holding this permission on the ticket ([`authz::authorize_ticket`]).
    Staff(Permission),
    /// The requester, or staff with write access ([`authz::is_staff_writer`]).
    RequesterOrStaff,
    /// Background jobs only (AI attempts, the confirmation timeout).
    System,
}

/// Submitted and not finished: the statuses a requester may cancel from.
const OPEN: &[TicketStatus] = &[
    TicketStatus::AiProcessing,
    TicketStatus::AwaitingAiDecision,
    TicketStatus::Queued,
    TicketStatus::InProgress,
    TicketStatus::WaitingForRequester,
    TicketStatus::AwaitingConfirmation,
];

impl Transition {
    /// The transition table: who may attempt it, and from which statuses.
    /// No row starts from `ai_solved` or `cancelled`: those records are final.
    pub fn rule(&self) -> (Party, &'static [TicketStatus]) {
        use TicketStatus as S;
        match self {
            Self::Submit { .. } => (Party::Requester, &[S::Draft]),
            Self::AiSuggestionReady | Self::AiFailed => (Party::System, &[S::AiProcessing]),
            Self::SkipAi => (Party::Requester, &[S::AiProcessing]),
            Self::AiSolved | Self::AiNotSolved => (Party::Requester, &[S::AwaitingAiDecision]),
            Self::StartWork => (
                Party::Staff(Permission::ReplyAsStaff),
                &[S::Queued, S::WaitingForRequester],
            ),
            Self::RequestInformation => (Party::Staff(Permission::ReplyAsStaff), &[S::InProgress]),
            Self::RequesterResponded => (Party::Requester, &[S::WaitingForRequester]),
            Self::ProposeResolution { .. } => (
                Party::Staff(Permission::ProposeResolution),
                &[
                    S::InProgress,
                    S::WaitingForRequester,
                    S::AwaitingConfirmation,
                ],
            ),
            Self::AcceptResolution { .. } | Self::RejectResolution { .. } => {
                (Party::Requester, &[S::AwaitingConfirmation])
            }
            Self::ConfirmationTimeout { .. } => (Party::System, &[S::AwaitingConfirmation]),
            Self::Reopen => (Party::RequesterOrStaff, &[S::Closed]),
            Self::Cancel { .. } => (Party::Requester, OPEN),
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Submit { .. } => "submit",
            Self::AiSuggestionReady => "ai_suggestion_ready",
            Self::AiFailed => "ai_failed",
            Self::SkipAi => "skip_ai",
            Self::AiSolved => "ai_solved",
            Self::AiNotSolved => "ai_not_solved",
            Self::StartWork => "start_work",
            Self::RequestInformation => "request_information",
            Self::RequesterResponded => "requester_responded",
            Self::ProposeResolution { .. } => "propose_resolution",
            Self::AcceptResolution { .. } => "accept_resolution",
            Self::RejectResolution { .. } => "reject_resolution",
            Self::ConfirmationTimeout { .. } => "confirmation_timeout",
            Self::Reopen => "reopen",
            Self::Cancel { .. } => "cancel",
        }
    }
}

/// Who resolved a finished ticket (`tickets.resolution_source`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    Ai,
    Operator,
}

/// How a resolved ticket was closed (`tickets.closure_reason`). A timeout is never an acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClosureReason {
    RequesterConfirmed,
    ConfirmationTimeout,
}

/// Facts about the locked ticket that the rules depend on.
#[derive(Clone, Copy, Debug)]
pub struct TicketState {
    pub scope: TicketScope,
    pub proposal_version: i32,
    pub confirmation_deadline: Option<OffsetDateTime>,
    /// A human work-start (`in_progress`) event was ever recorded. Unassignment
    /// or transfer never resets it.
    pub work_started: bool,
    /// The ticket's category is "Skip AI automatically".
    pub category_skip_ai: bool,
}

/// What an accepted transition writes.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub to: TicketStatus,
    /// `ticket_events.payload`: route, resolution provenance, proposal snapshot, or reason.
    pub payload: Value,
    /// Set exactly when entering `awaiting_confirmation`.
    pub confirmation_deadline: Option<OffsetDateTime>,
    /// Set exactly when entering `closed` or `ai_solved`.
    pub resolution: Option<(ResolutionSource, ClosureReason)>,
}

/// The lifecycle rule. `actor` is `None` for background jobs. Checks, in order:
/// input bounds (`422`), visibility (`404`), who may attempt it (`403`), the
/// current status (`409`), then policy (`403`) and proposal version/deadline (`409`).
pub fn decide(
    ticket: &TicketState,
    actor: Option<&Actor>,
    t: &Transition,
    policy: &Policy,
    now: OffsetDateTime,
) -> Result<Outcome, ApiError> {
    use TicketStatus as S;
    validate(t)?;
    let scope = &ticket.scope;
    let (party, from) = t.rule();
    let allowed = match actor {
        None => party == Party::System,
        Some(a) => {
            let view = authz::ticket_view(a, scope).ok_or(ApiError::NotFound)?;
            match party {
                Party::Requester => view == TicketView::Requester,
                Party::Staff(permission) => authz::authorize_ticket(a, scope, permission).is_ok(),
                Party::RequesterOrStaff => {
                    view == TicketView::Requester || authz::is_staff_writer(a, scope)
                }
                Party::System => false,
            }
        }
    };
    if !allowed {
        return Err(ApiError::Forbidden);
    }
    if !from.contains(&scope.status) {
        return Err(ApiError::Conflict);
    }
    let same_proposal = |version: i32| {
        if version == ticket.proposal_version {
            Ok(())
        } else {
            Err(ApiError::Conflict)
        }
    };
    let deny_unless = |ok: bool| if ok { Ok(()) } else { Err(ApiError::Forbidden) };
    let queue = |route: &str| (S::Queued, json!({ "route": route }));
    let mut deadline = None;
    let mut resolution = None;
    let (to, payload) = match t {
        Transition::Submit { requester_skip } => {
            // Global availability, then category exemption, then requester skip.
            let route = if !policy.ai_available {
                "ai_disabled"
            } else if ticket.category_skip_ai {
                "category_exempt"
            } else if *requester_skip {
                deny_unless(policy.allow_requester_skip)?;
                "requester_skip"
            } else {
                "ai"
            };
            let to = if route == "ai" {
                S::AiProcessing
            } else {
                S::Queued
            };
            (to, json!({ "route": route }))
        }
        Transition::AiSuggestionReady => (S::AwaitingAiDecision, json!({})),
        Transition::AiFailed => queue("ai_failed"),
        Transition::SkipAi => {
            deny_unless(policy.allow_requester_skip)?;
            queue("requester_skip")
        }
        Transition::AiSolved => {
            resolution = Some((ResolutionSource::Ai, ClosureReason::RequesterConfirmed));
            (
                S::AiSolved,
                json!({ "resolution_source": ResolutionSource::Ai }),
            )
        }
        Transition::AiNotSolved => queue("not_solved"),
        Transition::StartWork | Transition::RequesterResponded => (S::InProgress, json!({})),
        Transition::RequestInformation => (S::WaitingForRequester, json!({})),
        Transition::ProposeResolution { summary } => {
            let hours = policy.confirmation_timeout_hours;
            let due = now + Duration::hours(hours);
            deadline = Some(due);
            let payload = json!({
                "proposal_version": ticket.proposal_version + 1,
                "summary": summary.trim(),
                "timeout_hours": hours,
                "deadline": due.format(&Rfc3339).map_err(|_| ApiError::Internal)?,
            });
            (S::AwaitingConfirmation, payload)
        }
        Transition::AcceptResolution { proposal_version } => {
            same_proposal(*proposal_version)?;
            resolution = Some((
                ResolutionSource::Operator,
                ClosureReason::RequesterConfirmed,
            ));
            (S::Closed, closure(*proposal_version, resolution))
        }
        Transition::RejectResolution { proposal_version } => {
            same_proposal(*proposal_version)?;
            let payload = json!({ "proposal_version": proposal_version });
            (S::InProgress, payload)
        }
        Transition::ConfirmationTimeout { proposal_version } => {
            same_proposal(*proposal_version)?;
            if ticket.confirmation_deadline.is_none_or(|due| due > now) {
                return Err(ApiError::Conflict);
            }
            resolution = Some((
                ResolutionSource::Operator,
                ClosureReason::ConfirmationTimeout,
            ));
            (S::Closed, closure(*proposal_version, resolution))
        }
        Transition::Reopen => {
            deny_unless(policy.allow_reopen)?;
            (S::InProgress, json!({}))
        }
        Transition::Cancel { reason } => {
            deny_unless(match policy.cancellation {
                CancellationPolicy::AnytimeWhileOpen => true,
                CancellationPolicy::BeforeWorkStarts => !ticket.work_started,
                CancellationPolicy::Disabled => false,
            })?;
            (S::Cancelled, json!({ "reason": reason.trim() }))
        }
    };
    Ok(Outcome {
        to,
        payload,
        confirmation_deadline: deadline,
        resolution,
    })
}

fn closure(version: i32, resolution: Option<(ResolutionSource, ClosureReason)>) -> Value {
    let (source, reason) = resolution.expect("closures set a resolution");
    json!({ "proposal_version": version, "resolution_source": source, "closure_reason": reason })
}

/// Required, bounded free text: the proposal summary and the cancellation reason.
fn validate(t: &Transition) -> Result<(), ApiError> {
    let (field, text, max) = match t {
        Transition::ProposeResolution { summary } => ("summary", summary, SUMMARY_MAX),
        Transition::Cancel { reason } => ("reason", reason, REASON_MAX),
        _ => return Ok(()),
    };
    let text = text.trim();
    let code = if text.is_empty() {
        "required"
    } else if text.chars().count() > max {
        "too_long"
    } else {
        return Ok(());
    };
    Err(ApiError::Validation(FieldErrors::from([(
        field.into(),
        vec![code],
    )])))
}

/// Who performs a transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    Account(Uuid),
    /// A background job: AI attempts and the confirmation timeout.
    System,
}

/// Applies one transition in the caller's transaction under the current
/// [`policy`] and returns the new status. The caller commits; on error it must
/// roll back (dropping the transaction does).
///
/// Autocommit connections must not release the authorization and ticket locks
/// before the state and event have both been written:
/// ```compile_fail
/// use sqlx::PgConnection;
/// use tikkit_api::lifecycle::{transition, By, Transition};
/// async fn without_transaction(conn: &mut PgConnection, id: uuid::Uuid) {
///     transition(conn, id, By::System, &Transition::AiFailed).await.unwrap();
/// }
/// ```
pub async fn transition(
    tx: &mut Transaction<'_, Postgres>,
    ticket_id: Uuid,
    by: By,
    t: &Transition,
) -> Result<TicketStatus, ApiError> {
    transition_with(tx, ticket_id, by, t, None).await
}

/// [`transition`] with a fixed policy instead of the current one, for tests of
/// non-default settings. Production code calls [`transition`].
#[doc(hidden)]
pub async fn transition_with(
    tx: &mut Transaction<'_, Postgres>,
    ticket_id: Uuid,
    by: By,
    t: &Transition,
    fixed_policy: Option<Policy>,
) -> Result<TicketStatus, ApiError> {
    let conn = &mut **tx;
    let actor = match by {
        By::Account(id) => Some(lock_actor(conn, id).await?),
        By::System => None,
    };
    let row: Option<StateRow> = sqlx::query_as(
        "SELECT t.requester_account_id, t.team_id, t.assignee_account_id, t.status::text AS status,
                t.proposal_version, t.confirmation_deadline, c.skip_ai
         FROM tickets t JOIN categories c ON c.id = t.category_id
         WHERE t.id = $1
         FOR UPDATE OF t",
    )
    .bind(ticket_id)
    .fetch_optional(&mut *conn)
    .await?;
    let row = row.ok_or(ApiError::NotFound)?;
    let policy = match fixed_policy {
        Some(p) => p,
        None => policy(conn).await?,
    };
    // A fresh statement sees events committed by a previous ticket lock holder;
    // the clock is read after locking, not at the start of a possibly old transaction.
    let (work_started, now): (bool, OffsetDateTime) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM ticket_events
                        WHERE ticket_id = $1 AND to_status = 'in_progress'), clock_timestamp()",
    )
    .bind(ticket_id)
    .fetch_one(&mut *conn)
    .await?;
    let state = TicketState {
        scope: TicketScope {
            requester_account_id: row.requester_account_id,
            team_id: row.team_id,
            assignee_account_id: row.assignee_account_id,
            status: ident(row.status)?,
        },
        proposal_version: row.proposal_version,
        confirmation_deadline: row.confirmation_deadline,
        work_started,
        category_skip_ai: row.skip_ai,
    };
    let out = decide(&state, actor.as_ref(), t, &policy, now)?;
    sqlx::query(
        "UPDATE tickets SET status = $2, proposal_version = proposal_version + $3,
                confirmation_deadline = $4, resolution_source = $5, closure_reason = $6,
                updated_at = $7
         WHERE id = $1",
    )
    .bind(ticket_id)
    .bind(ident_str(out.to))
    .bind(i32::from(out.to == TicketStatus::AwaitingConfirmation))
    .bind(out.confirmation_deadline)
    .bind(out.resolution.map(|(source, _)| ident_str(source)))
    .bind(out.resolution.map(|(_, reason)| ident_str(reason)))
    .bind(now)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, payload, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(ticket_id)
    .bind(actor.map(|a| a.id))
    .bind(t.event_type())
    .bind(ident_str(state.scope.status))
    .bind(ident_str(out.to))
    .bind(&out.payload)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(out.to)
}

#[derive(sqlx::FromRow)]
struct StateRow {
    requester_account_id: Uuid,
    team_id: Option<Uuid>,
    assignee_account_id: Option<Uuid>,
    status: String,
    proposal_version: i32,
    confirmation_deadline: Option<OffsetDateTime>,
    skip_ai: bool,
}

/// The actor as of this transaction. Share locks make a concurrent role,
/// activation, membership, or leadership change wait until this transition
/// commits, so a revoked writer can never commit after the revocation.
async fn lock_actor(conn: &mut PgConnection, id: Uuid) -> Result<Actor, ApiError> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role::text FROM accounts WHERE id = $1 AND is_active FOR SHARE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let team: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT t.id, t.lead_account_id IS NOT DISTINCT FROM m.account_id
         FROM team_memberships m JOIN teams t ON t.id = m.team_id
         WHERE m.account_id = $1 AND t.archived_at IS NULL
         FOR SHARE",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(Actor {
        id,
        role: ident(role.ok_or(ApiError::Forbidden)?)?,
        team: team.map(|(team_id, is_lead)| TeamScope { team_id, is_lead }),
    })
}
