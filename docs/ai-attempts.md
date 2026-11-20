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
and attempts are never deleted (database trigger). The one exception is the
Admin retention policy, which blanks `input`, `explanation`, and `steps` of a
finished attempt of a ticket in a final status and sets `content_purged_at`
([retention.md](retention.md)); state, outcome, provider, model, and times stay. An attempt whose suggestion
the requester never answered stays `suggestion_ready` on a ticket that is not
`ai_solved`: abandoned attempts remain visible as such and are never counted as
solved. Only the explicit requester decision makes a ticket `ai_solved`
([ai-decisions.md](ai-decisions.md)).

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

1. **Claim and run.** A claim takes the oldest `processing` attempt that no
   live worker holds, sets `claimed_until` to now plus the lease (30 s), and
   increments `claims`. Claims are serialized across processes by an advisory
   lock, so no attempt is claimed twice. A process works on at most four
   attempts at once.
2. **Sweep.** Hands off tickets in `ai_processing` whose attempt can no longer
   end by itself (see [Deadline](#deadline) and [Sweep](#sweep)).

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
that the attempt is still `processing` and still claimed by this worker. If
not, the provider call is cancelled.

### Provider slots and fairness

The guard admits at most `max_concurrency` provider calls at once
([ai-usage.md](ai-usage.md#concurrency)). The worker respects that limit
before it claims: an attempt is claimed only while fewer than
`max_concurrency` attempts are held by live workers, counted over all
processes. Attempts beyond that stay unclaimed and are taken oldest first as
slots become free, so an older ticket is never refused while a newer one is
served.

If the guard still refuses with `concurrency_saturated` (slots held by calls
of a crashed process, for example), the attempt keeps its claim, waits two
seconds, and asks again. It is handed off as `concurrency_saturated` only when
its deadline passes while it is still waiting for a slot.

### Deadline

`deadline_at` is set at submission to five minutes later (`ATTEMPT_DEADLINE`)
and never changes. It bounds when a provider call may be **started**:

- An attempt past its deadline is never claimed, and a claimed attempt never
  enters the guard after it. Without a live claim the sweep hands it off as
  `timeout` (or the worker does, if the deadline passed before the call).
- A call that entered the guard in time is not cut short. It ends by the
  guard's own bounds: the wait for a slot (`concurrency_wait_seconds`, default
  30 s, at most 300 s) plus the call timeout (120 s including transport
  retries, [ai-providers.md](ai-providers.md#limits)). Its suggestion is
  accepted even if `deadline_at` passed meanwhile.
- The hard bound is therefore `deadline_at + concurrency_wait_seconds + call
  timeout`: about 7.5 minutes after submission with the defaults. One minute
  after that the sweep hands the ticket off as `timeout` even though a worker
  still holds it; a result that arrives later is dropped. A worker that died
  is noticed sooner, when its lease (30 s) runs out.

A requester therefore waits at most the hard bound before the ticket is with
operators, however the provider or the workers behave.

### Restart, shutdown, and crash recovery

All state is in the database.

- **Shutdown.** When the API stops, the worker cancels the provider calls in
  flight. The guard records each as `cancelled` and frees its slot at once.
  The attempt stays `processing`, its claim is released (`claimed_until`
  NULL), and nothing is handed off. `main.rs` waits for this within its
  five-second worker window. The next process claims the attempt immediately.
- **Crash.** A dead process leaves `claimed_until` set; once it passes, any
  worker claims the attempt again. Its abandoned guard reservation is released
  by the guard after its own lease.
- **Claim cap.** Every claim can cost one provider call, so an attempt is
  claimed at most three times (`MAX_CLAIMS`). After the third claim ended
  without a result, the sweep hands the ticket off as `claims_exhausted` with
  no further call.
- **A result that cannot be stored.** If the commit transaction fails three
  times, the worker hands the ticket off as `commit_failed` in a new
  transaction instead of calling the provider again. Provider output that the
  database cannot hold at all (a NUL character) is rejected by the adapters as
  `malformed_output` before it gets that far.

A takeover is the same row with the same input: one logical attempt, and only
one result can ever be stored. A worker that lost its claim (it stalled past
its lease) finds at commit that the attempt is finished or claimed by another
worker and drops its result.

### Sweep

Each step takes one ticket in `ai_processing` (`FOR UPDATE SKIP LOCKED`,
oldest first) for which one of these holds, and hands it off:

| Condition | Outcome |
| --- | --- |
| No live claim and the deadline passed | `timeout` |
| No live claim and already claimed `MAX_CLAIMS` times | `claims_exhausted` |
| Past the hard bound plus one minute, whatever the claim | `timeout` |
| No processing attempt at all | `orphaned`, plus an `ai_attempt_orphaned` audit event with the ticket id |

A locked ticket is skipped, and a ticket whose handoff failed is left out
until the current cycle has handled the others, so one stuck ticket never
blocks the rest.

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
screenshot must stay readable. One preparation runs at a time per process,
because decoding a large screenshot needs a lot of memory. If the images together exceed 12 MiB, each gets
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
- policy no longer allows it: hand off with the policy code, even if the
  provider returned a suggestion;
- suggestion: store `explanation`, `steps`, and the usage id, and apply
  `AiSuggestionReady` (`awaiting_ai_decision`). Completion never queues or
  resolves the ticket;
- failure or guard refusal: hand off with that code.

**Handoff** is `lifecycle::ai_failed` (the `AiFailed` transition by the system
actor, route `ai_failed`), which also ends the attempt with its outcome code
under the ticket lock, just before the event is written, so the audit mirror of
the event can name the reason ([audit.md](audit.md#ticket-events)). One
transaction. The transition only
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
  The HTTP action is `{action: "skip_ai"}` on `POST /api/tickets/{id}/actions`
  ([ai-decisions.md](ai-decisions.md)).

### Outcome codes

`ai_attempts.outcome` is a stable identifier:

- attempt: `timeout`, `orphaned`, `claims_exhausted`, `commit_failed`,
  `image_unavailable`, `image_preparation_failed`, `requester_skip`,
  `requester_cancelled`;
- policy: `ai_disabled`, `configuration_changed`, `credential_changed`,
  `credential_missing`, `credential_unreadable`, `model_unsupported`,
  `category_exempt`;
- guard and provider: every `Refusal::code()` of [ai-usage.md](ai-usage.md#outcomes)
  (`budget_org_requests`, `circuit_open`, `rate_limited`,
  `upstream_unavailable`, `invalid_credentials`, ...). A provider call that
  timed out is also `timeout`. `concurrency_saturated` appears only when the
  deadline passed while waiting for a slot; `cancelled` is never stored.

## What the requester sees

`GET /api/tickets/{id}` returns `ai` to the ticket's requester (any role) and,
as context, to staff who may see the ticket
([ai-decisions.md](ai-decisions.md#ai-context-in-ticketdetailai)). It is `null`
for tickets that never had an attempt. The object also carries `decision` and
`decided_at`, described there.

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
duplicate claims across pools, a worker that dies mid-call, a takeover while
the first worker still runs, the claim cap, shutdown (one worker and the whole
loop), provider-slot fairness and saturation, the deadline and its hard bound,
a locked ticket in the sweep, a result that cannot be stored, and image
preparation. Unit tests in `ai_attempts.rs` cover
`prepare_images`.

## Decisions

What the requester does with the suggestion (solved, still need help, skip),
the finality of AI-solved records, and the Admin list and statistics are in
[ai-decisions.md](ai-decisions.md). A decision records the attempt it answered
in its ticket event (`payload.attempt_id`) and never changes the attempt row.
