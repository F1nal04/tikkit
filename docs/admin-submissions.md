# Admin submission view

Admins see every submitted ticket, whatever became of it: tickets still with
the AI, AI-solved records, tickets escalated to the help desk, operator
resolutions, and cancellations. AI-solved records live here and in reporting
only; they never become operator work ([ai-decisions.md](ai-decisions.md)).

Code: `apps/api/src/submissions.rs`; web `apps/web/src/routes/(app)/admin/submissions/`
and `apps/web/src/lib/submissions.ts`. Migration `0024_admin_submissions.sql`
([schema.md](schema.md#admin-submission-view)).

## Access

Both endpoints require `view_all_submissions` (Admin only,
[authorization.md](authorization.md)). Every other role, including Team Leads
and staff who submitted a ticket themselves, gets `403 forbidden` with no data
and no count; signed out is `401`. There is no count endpoint: the list is the
only view of the dataset. The web routes sit under the `/admin` guard.

## Read-only, no side effects

Both handlers run in one `REPEATABLE READ, READ ONLY` transaction, so the
database rejects any write they could attempt. Viewing never assigns, claims,
dispatches, reopens, notifies, writes an event or audit row, or touches a
ticket's `updated_at`. The routes accept `GET` only (`405` otherwise). There is
no reopen, escalate, or decide action here; AI-solved records stay final
through the existing write endpoints for Admins too (`403`,
[ai-decisions.md](ai-decisions.md#ai-solved-records-are-final)). The web pages
have no buttons besides the filter form and do not poll.

Responses carry no secret: no AI credential, key hint, or ciphertext, no
encryption or session secret, and not the attempt's provider input snapshot.
The attempt shows only its provider and model names and the stable outcome code.

## List: `GET /api/admin/submissions`

Newest first by submission time (`created_at`, `id`) with standard cursor
paging ([api-conventions.md](api-conventions.md#lists-pagination-ordering-filtering)).
Drafts are not submissions and never appear. Filters apply in SQL before the
limit:

| Parameter | Values |
| --- | --- |
| `category_id` | The ticket's category (frozen at submission; archived ones included) |
| `status` | Any ticket status except `draft` (`400 invalid_query`) |
| `resolution_source` | `ai`, `operator`, or `none` (open or cancelled) |
| `ai_outcome` | An outcome below, or `none` (never had an AI attempt) |
| `created_from`, `created_before` | RFC 3339 instants: inclusive start, exclusive end; an empty range is `400` |

Each item (`SubmissionSummary`): `id`, `reference`, `subject`, `status`,
`category_id`, `category_name`, `priority_name` (current), `requester`,
`team_name`, `assignee_display_name`, `resolution_source`, `closure_reason`,
`resolution`, `route`, `ai_outcome`, `created_at`, `updated_at`.

`resolution` separates the endings the spec requires reporting to keep apart:

| `resolution` | Meaning |
| --- | --- |
| `open` | Not finished: in AI, waiting for a decision, or with the help desk |
| `ai_confirmed` | The requester explicitly confirmed the AI suggestion (`ai_solved`) |
| `requester_confirmed` | The requester confirmed an operator's resolution |
| `confirmation_timeout` | Closed automatically after an unanswered proposal; not a confirmation |
| `cancelled` | Cancelled by the requester |

`route` is how the ticket entered (the `submit` event): `ai`, `ai_disabled`,
`category_exempt`, or `requester_skip`; `null` for records without that event.

`ai_outcome` is the same classification as the AI records list
([ai-decisions.md](ai-decisions.md#admin-records-and-statistics)). Only
`solved` is an AI resolution; the others are kept distinct from it:

| `ai_outcome` | Shown as |
| --- | --- |
| `solved` | Solved by AI, confirmed by the requester (final) |
| `awaiting_decision` | Unconfirmed: suggestion shown, no decision |
| `processing` | Pending: suggestion being prepared |
| `not_solved` | Not solved: sent to the help desk |
| `skipped` | Skipped by the requester: sent to the help desk |
| `failed` | Failed: no suggestion, sent to the help desk |
| `cancelled` | Abandoned: cancelled before any decision |

Retention is not applied here: every persisted submission is listed. Expiry and
deletion belong to the retention work ([#43](https://github.com/F1nal04/tikkit/issues/43)),
which removes records from this view by deleting them.

## Detail: `GET /api/admin/submissions/{id}`

`SubmissionDetail`: `summary` (the list item), `description`,
`intake_snapshot` (the frozen original intake with category, original priority,
visible answers, and screenshot IDs), `confirmation_deadline`, `ai`, and
`events`. A draft or unknown ID is `404`.

`ai` is `null` without an attempt; otherwise `attempt_id`, `outcome`, `state`,
`outcome_code` (internal, [ai-attempts.md](ai-attempts.md#outcome-codes)),
`provider`, `model`, `language`, `suggestion` (the one stored suggestion,
untrusted text), `decision` (`solved`, `not_solved`, or `null`),
`decision_event_id`, `decided_at`, `created_at`, `finished_at`. For an AI-solved
record `decision_event_id` is the `ai_solved` event in `events`: the explicit
confirmation by the requester, with `payload.resolution_source = "ai"`.

`events` lists every `ticket_events` row oldest first: transitions (with
`from_status`/`to_status`), assignment and priority changes, and message
events (message ID only, never the text). `actor` is the account (`id`,
`username`, `display_name`) or `null` for background jobs.

Screenshots load through the normal screenshot endpoints, which apply the
ticket visibility policy (Admins see every submitted ticket).

## Tests

`apps/api/tests/admin_submissions.rs` against PostgreSQL: a mixed-outcome
fixture (no AI, category exempt, operator-confirmed, confirmation timeout,
processing, awaiting decision, solved, not solved, skipped, failed, abandoned,
and a draft); every filter, date bounds, paging, and invalid values; the
AI-solved detail with intake, suggestion, and confirmation event; unconfirmed,
abandoned, and failed attempts never reading as solved; every non-Admin role
denied; and a byte-for-byte comparison of tickets, events, attempts,
notifications, outbox, audit, and usage before and after viewing, with no
stored credential or secret in any response. Web: `src/lib/submissions.test.ts`
and the page tests next to the routes; `e2e/admin-submissions.spec.ts` covers
mixed outcomes, filtering, the read-only AI-solved record (GET requests only,
suggestion rendered as text), no secrets in the page source, German at phone
width, and denial for requester, operator, and Team Lead.
