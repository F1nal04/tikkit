# AI decisions

After an AI suggestion is stored, the requester decides: it solved the problem,
or they still need help. This page describes those decisions, the skip during
processing, what happens when requests repeat or race, why an AI-solved record
is final, who sees what, and what Admins can inspect and count.

Code: `apps/api/src/tickets.rs` (`requester_action`), `apps/api/src/lifecycle.rs`,
`apps/api/src/ai_records.rs`. Migration `0023_ai_decisions.sql`
([schema.md](schema.md#ai-decisions)). The attempt and its suggestion:
[ai-attempts.md](ai-attempts.md). The web requester flow is #36.

## The three actions

All three are `POST /api/tickets/{id}/actions` with a body of exactly
`{"action": "..."}` and answer `200 {"status": "<ticket status>"}`. They are the
lifecycle transitions `AiSolved`, `AiNotSolved`, and `SkipAi`
([lifecycle.md](lifecycle.md)).

| Action | From | To | Condition |
| --- | --- | --- | --- |
| `ai_solved` | `awaiting_ai_decision` | `ai_solved` (final) | a suggestion is stored |
| `ai_not_solved` | `awaiting_ai_decision` | `queued` | a suggestion is stored |
| `skip_ai` | `ai_processing` | `queued` | Admin setting "Allow requester to skip AI" is on |

Only the ticket's requester may send them, in any role (compared by account
ID). Everyone else gets `404`, or `403` for an Admin, who can see the record
but is not its requester. No other party can make a ticket `ai_solved`: not
staff, not a job, not silence.

The decision applies to the ticket's one stored suggestion. `lifecycle` looks
the attempt up under the ticket lock and requires it to be `suggestion_ready`;
without one the decision is `409` and nothing is written.

**Solved** writes, in one transaction: status `ai_solved`, `resolution_source =
ai`, `closure_reason = requester_confirmed`, and one `ai_solved` event with the
requester as actor and payload `{resolution_source, closure_reason, attempt_id}`.
The event's `created_at` (equal to the ticket's `updated_at`) is the
confirmation time. The ticket has no team and no assignee, no staff
notification is queued, and it never appears in a queue.

**Not solved** and **skip** move the same ticket (same ID and reference) to
`queued` with no team and no assignee: the shared general queue, once. The
event is `ai_not_solved` with payload `{route: "not_solved", attempt_id}` or
`skip_ai` with `{route: "requester_skip", attempt_id}`. A skip also ends the
attempt (`handed_off`, outcome `requester_skip`), so a late provider result is
dropped.

### Where the attempt reference lives

In the decision event (`payload.attempt_id`). Attempt rows are never updated by
a decision: a finished attempt is immutable, and history grows only by
appending events. A unique index allows one `ai_solved` or `ai_not_solved`
event per ticket.

### Skip and "still need help" before the suggestion

Skipping is forbidden by default. With the setting off, `skip_ai` is `403` and
the attempt keeps running. `ai_not_solved` while the ticket is still
`ai_processing` is never a way around mandatory AI: it is `403` while skipping
is forbidden, and `409` (there is no suggestion to answer; send `skip_ai`) while
it is allowed. Neither writes anything. A skip at submission is
`requester_skip: true` on the submission, under the same setting
([submission.md](submission.md)). A category marked "Skip AI automatically" is
queued at submission without a provider call and without any decision.

## Repeats and races

Every decision takes the ticket row lock, so concurrent requests run one after
another and exactly one transition commits.

- **The same decision again** (a transport retry, a double click, a second
  tab): `200` with the ticket's current status, nothing written. This holds
  later too: a repeated `ai_not_solved` after operators started work answers
  `200 {"status": "in_progress"}`.
- **Another AI decision** on a ticket that already left that stage (the opposite
  decision, a skip after the suggestion arrived, a stale tab): `409 conflict`,
  and `error.details.status` is the committed ticket status. Nothing is
  written, so a stale "Solved" cannot take a ticket away from operators and a
  stale "Still need help" cannot reopen an AI-solved record.

```json
{ "error": { "code": "conflict", "message": "…",
             "details": { "fields": {}, "status": "ai_solved" } },
  "request_id": "…" }
```

A retry is recognized by the committed event of that type with the requester as
actor, not by a client key: a ticket can only ever hold one of the three.

A skip racing the worker, an Admin configuration change, or a cancellation ends
`ai_processing` once: one of `skip_ai`, `ai_failed`, `ai_suggestion_ready`, or
`cancel` commits and the ticket is queued at most once.

## One suggestion, no chat

There is no endpoint that continues a conversation with the AI, regenerates a
suggestion, or retries an attempt, and none of the three actions accepts any
field besides `action` (`422`). Replies are not possible while a ticket is in
an AI stage (`409` for the requester). `ai_attempts.ticket_id` is unique, so a
ticket cannot get a second attempt. A decision never calls the provider.

## AI-solved records are final

For its requester an `ai_solved` ticket is read-only history: listed in
`GET /api/tickets`, readable with the suggestion in `GET /api/tickets/{id}`.
No role can change it:

| Request on an `ai_solved` ticket | Requester | Admin | Operator, Specialist, Team Lead | Another User |
| --- | --- | --- | --- | --- |
| `actions`: `reopen`, `cancel`, `accept_resolution`, `reject_resolution`, `ai_not_solved`, `skip_ai` | `409` | `403` | `404` | `404` |
| `actions`: `ai_solved` | `200`, unchanged | `403` | `404` | `404` |
| `staff-actions` (start, request information, propose, reopen, priority) | `403` | `403` | `404` | `403` |
| `assignment` (dispatch, return, claim, assign, unassign) | `403` | `403` | `404` | `404` |
| `replies` | `409` | `403` | `404` | `404` |
| `notes` | `403` | `403` | `404` | `404` |
| `tickets/bulk` items | `forbidden` | `forbidden` | `not_found` | request `403` |

The reopening setting does not apply: it covers operator-resolved tickets only.
Jobs cannot change it either: no transition starts from `ai_solved`, the
attempt worker and its sweep only touch `processing` attempts, the auto-close
worker only `awaiting_confirmation`, and a late provider result finds its
attempt finished. A denied request writes no event, so statistics, queue
counts, and notifications do not move.

The database enforces the same (migrations `0008`, `0022`, `0023`): an
`ai_solved` ticket row cannot change in any column, the attempt cannot change
or be deleted, events cannot be changed or deleted, a second decision event cannot be
inserted, and no other event can be appended to an AI-solved record. A later
migration that backfills a new `tickets` column must disable
`tickets_lifecycle_guard` for AI-solved rows.

## Who sees what

The one visibility rule is `authz::ticket_view` and its SQL twin
`authz::staff_visible_sql` ([authorization.md](authorization.md)).

Tickets in `ai_processing` and `awaiting_ai_decision`, and `ai_solved` records,
are not operator work. For Operators, Specialists, and Team Leads they do not
exist: ticket detail, the activity change marker, messages, and screenshots are
`404`; queues, search, and counts (and the counts' change marker) leave them
out; `status=` of those values is `400` and cannot be stored in a saved view;
single and bulk writes are `404` / `not_found`; no staff notification is queued
for them, and none is delivered or listed. There is no export endpoint; any
future one must use `staff_visible_sql`.

Admins can open them with `GET /api/tickets/{id}` (staff view, every
`staff_access` flag false) and find them through the Admin list below. They are
never queue items, not even for an Admin.

After an escalation the ticket is ordinary general-queue work. Staff who may
see it get the original intake (frozen `intake_snapshot`, intake screenshots)
and the AI context in `TicketDetail.ai`.

### AI context in `TicketDetail.ai`

`GET /api/tickets/{id}` returns `ai` to the requester and to staff who may see
the ticket; it is `null` for tickets that never had an attempt.

```json
{
  "state": "suggestion_ready",
  "language": "de",
  "suggestion": { "explanation": "…", "steps": ["…", "…"] },
  "reason": null,
  "created_at": "2026-11-16T10:00:00Z",
  "finished_at": "2026-11-16T10:00:12Z",
  "decision": "not_solved",
  "decided_at": "2026-11-16T10:03:40Z"
}
```

`decision` is `solved`, `not_solved`, or `null` (undecided, or there never was
a suggestion); `decided_at` is the confirmation time for `solved`. Staff see
the stored suggestion because it is what the requester already tried, and it
was written from the ticket's own intake only. Staff do not get the provider
input snapshot (a redacted copy of the intake they already see), usage records,
or the internal outcome code; `reason` is the same coarse value the requester
sees. Internal codes are in the Admin list.

## Admin: records and statistics

Both are Admin only (`403` otherwise) and contain no ticket text beyond the
subject.

`GET /api/admin/ai/records?outcome=&limit=&cursor=` (`view_all_submissions`)
lists every ticket that has an AI attempt, newest first, with standard cursor
paging. Each item: attempt `id`, `ticket_id`, `reference`, `subject`, ticket
`status`, `requester`, `outcome`, `attempt_state`, `outcome_code` (the internal
attempt code, [ai-attempts.md](ai-attempts.md#outcome-codes)), `provider`,
`model`, `created_at`, `decided_at`. `outcome` is one of:

| `outcome` | Meaning |
| --- | --- |
| `processing` | The suggestion is being prepared |
| `awaiting_decision` | A suggestion is stored, the requester has not decided |
| `solved` | Requester confirmed: `ai_solved` |
| `not_solved` | Escalated after the suggestion |
| `skipped` | Requester skipped during processing |
| `failed` | Handed off without a suggestion |
| `cancelled` | Requester cancelled during an AI stage |

`GET /api/admin/ai/outcomes?days=30` (`view_reports`, `days` 1 to 3650) counts
the immutable ticket events of the period: `solved` (`ai_solved`), `not_solved`
(`ai_not_solved`), `skipped` (`skip_ai`, plus submissions routed
`requester_skip`), `failed` (`ai_failed`), `category_exempt` (submissions that
bypassed AI by category), and `cancelled` (cancellations from an AI stage).
`processing` and `awaiting_decision` are the current ticket counts. `failures`
breaks `failed` down by attempt outcome code. Because the counts come from
events, a repeated or denied request never changes them.

In the web app: Administration → AI assistance → "AI outcomes and records"
(`/admin/ai/records`). A record opens the read-only staff ticket page, which
shows the suggestion and the decision.

## Tests

`apps/api/tests/ai_decisions.rs`, against PostgreSQL: each decision and its
stored result; ownership and cross-user denial; repeated requests; concurrent
opposite decisions and a skip racing the worker and cancellation (real
concurrent requests); the skip policy; the missing chat and regenerate routes;
every staff read and write path for the three hidden statuses and every staff
role; every mutation endpoint on an AI-solved record for every role, with the
stored state, statistics, and counts compared before and after; jobs, late
transitions, and direct SQL; the Admin list and statistics.
`apps/api/tests/ai_attempts.rs` runs the whole branch with the worker and the
mock provider.
