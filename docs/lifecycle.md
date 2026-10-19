# Ticket lifecycle

Every ticket status change goes through one function: `lifecycle::transition`
(`apps/api/src/lifecycle.rs`). Submission, AI jobs, staff work, requester
decisions, the auto-close worker, replies, and stale clients have no other way
to change `tickets.status`. The rule itself is the pure `lifecycle::decide`;
`Transition::rule` is its table of who may attempt what from which statuses.

```rust
let mut tx = state.pool.begin().await?;
let status = lifecycle::transition(&mut tx, ticket_id, By::Account(user.account_id()),
    &Transition::AcceptResolution { proposal_version }).await?;
// ... messages, outbox rows, etc. in the same transaction ...
tx.commit().await?;
```

## Statuses and transitions

`draft`, `ai_processing`, `awaiting_ai_decision`, `queued`, `in_progress`,
`waiting_for_requester`, `awaiting_confirmation`, `closed`, `cancelled`,
`ai_solved`. Event types are the `ticket_events.event_type` values.

| Event type | From | Who | To |
| --- | --- | --- | --- |
| `submit` | `draft` | requester | `ai_processing` or `queued` (see routing) |
| `ai_suggestion_ready` | `ai_processing` | system | `awaiting_ai_decision` |
| `ai_failed` | `ai_processing` | system | `queued` |
| `skip_ai` | `ai_processing` | requester, only if "Allow requester to skip AI" | `queued` |
| `ai_solved` | `awaiting_ai_decision` | requester | `ai_solved` (final) |
| `ai_not_solved` | `awaiting_ai_decision` | requester | `queued` |
| `start_work` | `queued`, `waiting_for_requester` | staff writer (`reply_as_staff`) | `in_progress` |
| `request_information` | `in_progress` | staff writer (`reply_as_staff`) | `waiting_for_requester` |
| `requester_responded` | `waiting_for_requester` | requester | `in_progress` |
| `propose_resolution` | `in_progress`, `waiting_for_requester`, `awaiting_confirmation` | staff writer (`propose_resolution`) | `awaiting_confirmation` |
| `accept_resolution` | `awaiting_confirmation` | requester, current proposal version | `closed` |
| `reject_resolution` | `awaiting_confirmation` | requester, current proposal version | `in_progress` |
| `confirmation_timeout` | `awaiting_confirmation` | system, current version, deadline passed | `closed` |
| `reopen` | `closed` | requester or staff writer, only if reopening is enabled | `in_progress` |
| `cancel` | `ai_processing` … `awaiting_confirmation` (submitted, open) | requester, under the cancellation policy | `cancelled` (final) |

Nothing leaves `ai_solved` or `cancelled`. "Staff writer" is the ticket policy
in [authorization.md](authorization.md): the actor sees the ticket as staff, it
belongs to a team, and it is unclaimed or the actor is its assignee, the team's
Lead, or an Admin. The requester never acts as staff on their own ticket, in any
role; requester transitions compare account IDs only.

Checks run in this order: input bounds (`422 validation_failed`: `summary`
up to 5000 characters, cancellation `reason` up to 1000, both required after
trimming), visibility (`404 not_found`), who may attempt it (`403 forbidden`),
the current status (`409 conflict`), then policy (`403 forbidden`) and the
proposal version / deadline (`409 conflict`).

## Routing into human support

`submit` evaluates, in order: AI unavailable (disabled or unconfigured) →
`queued` with route `ai_disabled`; category "Skip AI automatically" → route
`category_exempt`; requester skip requested → `requester_skip` if the
organization toggle allows it, otherwise `403`; else `ai_processing` (route
`ai`). The route is the event's `payload.route`; later entries record
`ai_failed`, `requester_skip`, or `not_solved`. Rust decides the route from the
current policy and the category; nothing the client sends can force it.

Tickets before human support and AI-solved records never have a team
(`tickets_pre_triage_unrouted`), so every entry into `queued` lands in the
shared general queue with no team or assignee. Dispatch and assignment are
separate team/assignee changes, not status transitions.

## Resolution and closure

A staff proposal bumps `tickets.proposal_version` and snapshots
`confirmation_deadline = now + confirmation_timeout_hours` (default 168). Its
event payload holds `proposal_version`, `summary`, `timeout_hours`, and
`deadline`; later settings changes never rewrite it. A new proposal supersedes
the old one, and its deadline replaces the old timer. Requester decisions and
the timeout carry the proposal version they apply to, so stale or duplicate
decisions get `409`.

Closures record `resolution_source` (`operator`, or `ai` for `ai_solved`) and
`closure_reason` (`requester_confirmed` or `confirmation_timeout`) on the
ticket and in the event. A timeout is never an acceptance; replies, views, and
silence never close anything. Rejection keeps the team and assignee and is
allowed with reopening disabled. Reopening clears both columns; the events keep
the history.

## Cancellation

The requester cancels a submitted open ticket with a reason. Policy
`anytime_while_open` (default), `before_work_starts` (no `in_progress` event
was ever recorded; unassignment or transfer does not reset that), or
`disabled`. The ticket keeps its team, history, and reference; the confirmation
deadline is cleared, and late AI results or timeouts get `409`.

## Policy

`lifecycle::policy` reads the Admin's organization settings (`organization`
row: `allow_reopening`, `requester_cancellation_policy`,
`resolution_confirmation_hours`, `allow_requester_ai_skip`) `FOR SHARE` in the
transition's transaction, so a concurrent settings change waits until the
transition that relied on the old values commits. AI stays unavailable until AI
configuration is persisted.

## Concurrency

`transition` re-reads the actor's role, activation, and team under share locks,
then locks the ticket row `FOR UPDATE`. Two opposite decisions (accept vs
reject, timeout vs accept, AI solved vs escalation, cancel vs a late AI result)
serialize on the row lock; the second sees the new status and gets `409`, so
exactly one event commits. A concurrent role, membership, or leadership change
waits for the transition to commit, so a revoked staff writer can never commit
afterwards. Deadlocks surface as `409 conflict`.

The API requires a `sqlx::Transaction`, so an autocommit connection cannot
release locks before the state and its event are written. After acquiring the
ticket lock, a fresh statement reads work-start history so cancellation sees
events committed by the previous lock holder. The current database clock,
read after locking, supplies proposal deadlines, timeout checks, and transition
timestamps; transaction-start time does not shorten the confirmation period.

## Database backstops

Migration `0008_ticket_lifecycle.sql`: `confirmation_deadline` is set exactly
while `awaiting_confirmation`; `resolution_source`/`closure_reason` match
`closed`/`ai_solved` and are NULL otherwise; pre-triage and AI-solved tickets
have no team. A trigger (`restrict_violation`, `23001`) rejects changing the
requester, editing subject/description/category or returning to `draft` after
submission, and any status/team/assignee change of an `ai_solved` or
`cancelled` ticket. The one exception is clearing the assignee when that
account is deactivated or leaves the team: the assignee is a foreign key to
the team membership, and the attributed `assignment_changed` event keeps the
history.

## Reading tickets

`GET /api/tickets/{id}` returns `TicketDetail` under `authz::ticket_view`: the
requester view (public data, no staff identities except the
`TicketAssignmentView`, `requester: null`, transition actors `null`) for the
ticket's own requester in every status, or the staff view. Drafts, AI-stage, and
AI-solved records are `404` for operators; Admins see everything but drafts.
`transitions` lists the status events oldest first with their public payloads.

## Requester HTTP decisions

`POST /api/tickets/{id}/actions` accepts one explicit tagged decision:
`{action: "accept_resolution" | "reject_resolution", proposal_version}`,
`{action: "cancel", reason}`, or `{action: "reopen"}`. It calls the same
transactional lifecycle and returns `{status}` only after commit. A duplicate,
stale proposal, or finished state returns `409`; a policy denial returns `403`;
invisible tickets return `404`. Reopen retains the lifecycle's requester-or-
authorized-staff rule. No AI escalation, general state setter, or intake edit
is exposed by this endpoint.

The persisted AI suggestion belongs to the AI attempt records (#35) and is not
exposed yet; AI-solved history shows an explicit unavailable-suggestion message.

## Staff HTTP actions

`POST /api/tickets/{id}/staff-actions` takes one tagged action plus the
`expected_updated_at` the client loaded (`TicketDetail.updated_at`):
`start_work`, `request_information`, `propose_resolution {summary}`, `reopen`,
or `set_priority {priority_id}`. Status actions call `lifecycle::transition`
(so the checks above apply: `404`, `403`, `409`, policy); there is no staff action
that accepts, rejects, or closes. A newer reply, status, or priority change
makes `expected_updated_at` stale: `409 conflict`, nothing written. Private
notes do not move `updated_at`. The check runs after the ticket lock, so
concurrent staff serialize and the second always loses.

`set_priority` needs `change_priority` on the ticket (open, in a team, caller in
charge; self-handling `403`). The level must be active and different from the
current one (`422 priority_id: invalid`). It updates `tickets.priority_id` and
appends a `priority_changed` event (no status columns) with the actor and
`previous_priority_id/name`, `priority_id/name`. `TicketDetail.priority_changes`
lists them, with actor identity in the staff view only.

`TicketDetail.staff_access` (staff view only) reports `can_handle` and
`can_reopen` from the same rules, so clients can show read-only state. They are
affordances; every write is rechecked. Transition and priority entries also carry
`actor_display_name` (staff view), so cancellations and proposals show who and when.

