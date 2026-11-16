# AI suggestion attempts

When AI is ready and the category is not exempt, a submitted ticket waits in
`ai_processing` while one AI suggestion is prepared for the requester. This
page describes the record of that attempt, the worker that runs it, and every
way it ends. Whatever happens, the ticket either gets its one suggestion or
reaches the operator queue exactly once (or the requester cancels it).

Code: `apps/api/src/ai_attempts.rs`. Table: migration `0022_ai_attempts.sql`
([schema.md](schema.md#ai-attempts)). Routing: [lifecycle.md](lifecycle.md#routing-into-human-support).
Guarded provider call: [ai-usage.md](ai-usage.md). Configuration:
[ai-configuration.md](ai-configuration.md).

## One attempt per ticket

`lifecycle::submit` decides the route. If it is `ai` (status `ai_processing`),
`submission` calls `ai_attempts::create` in the same transaction, so a ticket is
never in `ai_processing` without an attempt. `ai_attempts.ticket_id` is unique:
a ticket has at most one attempt, ever, and therefore at most one stored
suggestion. There is no retry, regenerate, or chat endpoint. A replayed
submission (same idempotency key) returns the original receipt and creates
nothing.

| State | Meaning | Ticket |
| --- | --- | --- |
| `processing` | Waiting for a worker or for the provider | `ai_processing` |
| `suggestion_ready` | The suggestion is stored (`explanation`, `steps`) | moved to `awaiting_ai_decision` |
| `handed_off` | No suggestion; `outcome` says why | moved to `queued` |
| `cancelled` | The requester cancelled the ticket while it was processing | `cancelled` |

`processing` is the only state that changes. A finished attempt is immutable,
and attempts are never deleted (database trigger). An attempt whose suggestion
the requester never answered stays `suggestion_ready` on a ticket that is not
`ai_solved`: abandoned attempts remain visible as such and are never counted as
solved. Only the explicit requester decision (#37) makes a ticket `ai_solved`.

## The input snapshot

`create` builds the provider input once, from the frozen intake of that ticket
and nothing else, passes it through `ai_privacy::minimize`, and stores it in
`ai_attempts.input`:

- the category name, subject, and description as submitted;
- the visible conditional answers as label and value (for example the program
  name). Answers to hidden or archived fields were already dropped by
  `catalog::validate_intake`;
- the intake screenshots: id, content type, size, and the SHA-256 of the stored
  file, for exactly the uploads this submission attached to this ticket;
- how many secrets were redacted and whether text was cut.

`ai_attempts.language` is the requester's account language at submission. The
suggestion is requested in it and stays in it: changing the interface language
later calls no provider and replaces nothing.

Not in the snapshot: the requester's identity, other tickets, internal notes,
later replies, later screenshots, knowledge-base content. The snapshot never
changes (database trigger), so later replies or screenshots are kept for staff
in the timeline and cannot alter the prompt or cause another call. The input is
stored after redaction; it is ticket content and is kept with the ticket.

The attempt also records the configuration it was created under: `provider`,
`model`, and `credential_set_at` (when the selected provider's key was stored).

## The worker

`ai_attempts::worker_loop` is spawned in `main.rs` and runs in every API
process. It has two loops, both built on `notifications::poll_loop` (two-second
poll, stops on shutdown):

1. **Claim and run.** One statement takes the oldest `processing` attempt that
   no live worker holds (`FOR UPDATE SKIP LOCKED`), sets `claimed_until` to now
   plus the lease (30 s), and increments `claims`. Concurrent workers and
   processes cannot claim the same attempt. A process works on at most four
   attempts at once; provider concurrency across processes is the guard's limit.
2. **Sweep.** Hands off any ticket in `ai_processing` whose attempt passed its
   deadline (or that has no processing attempt at all, outcome `orphaned`).
   This needs no worker to be alive for that attempt.

Running one attempt:

1. **Check the policy** (see below). If AI may no longer run for this attempt,
   no provider call is made.
2. **Load the screenshots.** Each snapshotted screenshot must still be an
   intake upload of this ticket and its requester, and the file's SHA-256 must
   equal the snapshot. Otherwise the attempt fails with `image_unavailable`.
3. **Prepare the images** (see below).
4. **Call the provider** through `AiGuard::suggest` with the ticket owner's
   account id. No transaction or lock is held during the call.
5. **Commit** in one transaction (see below).

While the attempt runs, a heartbeat (every 5 s) renews the lease and checks
that the attempt is still `processing`, still within its deadline, and still
claimed by this worker. If not, the provider call is cancelled.

### Restart and crash recovery

All state is in the database. If a process dies or is stopped, its attempts
keep `claimed_until`; once that passes, any worker claims them again (`claims`
counts how often). The attempt is the same row with the same input: one logical
attempt. A takeover makes a new guarded provider call; only one result can ever
be stored. If the commit transaction fails, it is retried three times; after
that the attempt is left to its lease and is run again.

### Deadline

`deadline_at` is set at submission to five minutes later
(`ATTEMPT_DEADLINE`) and never changes. Past it, nothing new is started: the
sweep (or the worker's own commit) hands the ticket off with outcome `timeout`,
even if the worker died. Transport retries inside one provider call are bounded
separately by the provider client ([ai-providers.md](ai-providers.md#limits)).

## Image preparation

The adapters accept at most 5 MiB and 8,000 px per image and 12 MiB together;
stored screenshots may be larger (10 MiB, 16,384 px). `prepare_images` brings
every screenshot within the limits:

1. An image within the limits is sent byte for byte.
2. An image with a side over 8,000 px is scaled to fit (Lanczos) and kept
   lossless (PNG or lossless WebP) if that fits the byte limit.
3. Otherwise it is encoded as JPEG at full resolution, quality 90, then 80.
4. Otherwise it is scaled down (Lanczos, JPEG quality 85) only as far as
   needed, never below 10 % of its size.

Full resolution is preferred over smaller files because error text in a
screenshot must stay readable. If the images together exceed 12 MiB, each gets
an equal share and is prepared again from the stored file. Transparent pixels
become white. No screenshot is ever left out: if one cannot be prepared, the
attempt fails with `image_preparation_failed` and the ticket is handed off.

## Ending an attempt

Everything that ends an attempt locks the ticket row, then the attempt row, and
acts only while the attempt is `processing`. Lock order everywhere: AI
settings, category, ticket, attempt.

**Commit of a worker result.** In one transaction the worker re-reads the
policy under share locks, locks the ticket and the attempt, and then:

- attempt no longer `processing`, or claimed by another worker: the result is
  dropped (a late result). Nothing is written;
- deadline passed: hand off with `timeout`;
- policy no longer allows it: hand off with the policy code, even if the
  provider returned a suggestion;
- suggestion: store `explanation`, `steps`, and the usage id, and apply
  `AiSuggestionReady` (`awaiting_ai_decision`). Completion never queues or
  resolves the ticket;
- failure or guard refusal: hand off with that code.

**Handoff** is `lifecycle::transition(AiFailed)` (system actor, route
`ai_failed`) plus the attempt update, in one transaction. The transition only
succeeds from `ai_processing` under the ticket lock, so a ticket is queued at
most once however many paths race.

### Policy check and invalidation

The policy is evaluated twice by the worker: before the provider call and again
in the commit transaction. An attempt may run only if

- `ai_config::load_active` returns a usable configuration,
- provider, model, and key version equal the ones the attempt was created with,
- the ticket's category is not "Skip AI automatically".

In addition, Admin changes stop pending attempts at once, in the Admin's own
transaction (`ai_attempts::invalidate`), and record the number in the audit
event (`attempts_handed_off`):

| Admin action | Outcome code |
| --- | --- |
| Disable AI | `ai_disabled` |
| Select another provider or model | `configuration_changed` |
| Replace the selected provider's key | `credential_changed` |
| Remove the selected provider's key | `credential_missing` |
| Mark the category "Skip AI automatically" | `category_exempt` (that category's attempts only) |

The tickets are in the operator queue when the Admin request returns. A
provider call still in flight is cancelled by the next heartbeat; if its result
arrives anyway, the commit finds the attempt finished and drops it. The
worker's own check covers changes that did not go through these endpoints (a
restored database, a rotated `ENCRYPTION_KEY`: `credential_unreadable`; a model
dropped from the supported table: `model_unsupported`).

The next submission after any of these changes is routed by the new
configuration (`lifecycle::submit` reads it in the submission transaction).

### Requester cancellation and skip

Both are lifecycle transitions; `lifecycle` ends the attempt in the same
transaction, under the ticket lock.

- **Cancel** (`POST /api/tickets/{id}/actions`, `{action: "cancel", reason}`,
  reason mandatory): the ticket becomes `cancelled`, the attempt `cancelled`
  with outcome `requester_cancelled`. The request is terminated, not sent to
  operators. Late provider output or a late failure is ignored and can neither
  queue nor resolve it. Mandatory AI does not forbid cancelling.
- **Skip** (`Transition::SkipAi`): allowed only while "Allow requester to skip
  AI" is on; otherwise `403` and the attempt keeps running. When allowed, the
  ticket is queued once (route `requester_skip`) and the attempt becomes
  `handed_off` with outcome `requester_skip`. A skip at submission
  (`requester_skip: true`) under the same toggle creates no attempt at all.
  The HTTP action for a skip during processing belongs to #37.

### Outcome codes

`ai_attempts.outcome` is a stable identifier:

- attempt: `timeout`, `orphaned`, `image_unavailable`,
  `image_preparation_failed`, `requester_skip`, `requester_cancelled`;
- policy: `ai_disabled`, `configuration_changed`, `credential_changed`,
  `credential_missing`, `credential_unreadable`, `model_unsupported`,
  `category_exempt`;
- guard and provider: every `Refusal::code()` of [ai-usage.md](ai-usage.md#outcomes)
  (`budget_org_requests`, `circuit_open`, `rate_limited`,
  `upstream_unavailable`, `invalid_credentials`, ...). A provider call that
  timed out is also `timeout`.

## What the requester sees

`GET /api/tickets/{id}` returns `ai` in the requester view (the ticket's own
requester, any role). It is `null` in every staff view and for tickets that
never had an attempt.

```json
{
  "state": "suggestion_ready",
  "language": "de",
  "suggestion": { "explanation": "…", "steps": ["…", "…"] },
  "reason": null,
  "created_at": "2026-11-16T10:00:00Z",
  "finished_at": "2026-11-16T10:00:12Z"
}
```

- `state`: `processing`, `suggestion_ready`, `handed_off`, `cancelled`.
- `suggestion`: set once the suggestion exists, and kept afterwards.
- `reason`: set for `handed_off`. Requesters get a coarse, stable reason, not
  the internal code: `timeout`, `requester_skip`, `ai_unavailable` (AI was
  switched off, reconfigured, or no longer applies to the category), or
  `ai_failed` (provider, limits, or screenshots).

To wait for the result, poll `GET /api/tickets/{id}/activity`; its `version`
changes with the status transition that ends the attempt. Reload the ticket
then. A page opened later reads the same stored state, so the flow resumes
after navigation or a restart.

Operators never see a ticket in `ai_processing` or `awaiting_ai_decision`:
detail is `404`, and queues, search, counts, screenshots, and notifications
use the same visibility rule ([authorization.md](authorization.md)). After a
handoff the ticket is ordinary queue work.

## Logs and storage

The worker logs attempt and ticket ids and outcome codes only. The minimized
input and the suggestion are stored in `ai_attempts` and nowhere else; usage
metadata stays in `ai_usage` and is linked by `usage_id` (cleared when the
usage row is purged).

## Tests

`apps/api/tests/ai_attempts.rs`, against PostgreSQL and the mock provider:
routing and the stored suggestion, the one-solution rule, snapshot and language
immutability, skip and exemption policy, handoff exactly once for provider
failures, rate limits, budgets, and an open circuit, Admin invalidation before
and during a call, the commit-time policy check, cancellation with late output,
duplicate claims across pools, a worker that dies mid-call, the deadline, a
failed commit, and image preparation. Unit tests in `ai_attempts.rs` cover
`prepare_images`.

## What #37 and #36 build on

- The suggestion of a ticket: `ai_attempts` row with `state =
  'suggestion_ready'` (`explanation`, `steps`, `language`), or
  `ai_attempts::view(conn, ticket_id)`.
- Decisions are the existing transitions `AiSolved` / `AiNotSolved` from
  `awaiting_ai_decision`; they do not touch the attempt row.
- A skip during processing is `lifecycle::transition(tx, id,
  By::Account(requester), &Transition::SkipAi)`; the attempt is ended by the
  lifecycle.
