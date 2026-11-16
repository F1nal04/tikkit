# AI usage budgets, privacy, and safeguards

Optional AI assistance is bounded and observable. Every provider call goes
through one guarded path that enforces Admin-set budgets, a concurrency limit,
and a circuit breaker, and records what happened as metadata only.

None of this can stop a ticket from being submitted or from reaching human
support. A guard that says no returns a value; the caller hands the ticket to
the operator queue.

Code: `apps/api/src/ai_guard.rs` (the guarded call, circuit breaker,
retention), `apps/api/src/ai_privacy.rs` (input minimization and redaction),
`apps/api/src/ai_usage.rs` (Admin API). Tables: migration
`0021_ai_usage_safeguards.sql` ([schema.md](schema.md#ai-usage-safeguards)).
Provider adapters: [ai-providers.md](ai-providers.md). Stored configuration:
[ai-configuration.md](ai-configuration.md).

## The guarded call

```rust
let guard = AiGuard::from_state(&state); // or AiGuard::new(pool, client, endpoint)
match guard.suggest(requester_account_id, &active, &request, &cancel).await {
    Ok(Guarded { suggestion, usage_id }) => { /* the one suggestion */ }
    Err(Refused { refusal, usage_id }) => { /* hand the ticket to operators */ }
}
```

`active` comes from `ai_config::load_active`; `request` from
`ai_privacy::minimize`. `AiGuard::suggest` is the only caller of
`AiClient::suggest`. It does four things in order:

1. **Admit.** In one transaction it locks the `ai_limits` row, releases
   reservations whose lease ran out, checks the budgets, the circuit breaker,
   and the concurrency limit, and inserts a `reserved` row in `ai_usage`. The
   row lock serializes every admission in the deployment, so concurrent bursts
   cannot overshoot, whichever process they run in.
2. **Wait, bounded.** If only the concurrency limit is in the way, it retries
   admission every 200 ms for at most `concurrency_wait_seconds`, holding no
   reservation while it waits.
3. **Call** the provider through `AiClient::suggest`.
4. **Record** the outcome in the `ai_usage` row and update the circuit breaker.

It reads and writes no ticket state and performs no lifecycle transition.

### Outcomes

Every refusal is recoverable for the ticket. `Refusal::code()` is the stable
identifier stored in `ai_usage.outcome`.

| `Refusal` | Code | Provider called |
|---|---|---|
| `BudgetExhausted(UserRequests)` | `budget_user_requests` | no |
| `BudgetExhausted(OrgRequests)` | `budget_org_requests` | no |
| `BudgetExhausted(UserTokens)` | `budget_user_tokens` | no |
| `BudgetExhausted(OrgTokens)` | `budget_org_tokens` | no |
| `CircuitOpen` | `circuit_open` | no |
| `ConcurrencySaturated` | `concurrency_saturated` | no |
| `RateLimited { retry_after }` | `rate_limited` | yes (429/402) |
| `ProviderFailed(kind)` | the adapter kind: `timeout`, `upstream_unavailable`, `invalid_credentials`, `model_error`, `malformed_output`, `oversized_output`, `vision_failed`, `invalid_input`, `destination_blocked` | yes, except `invalid_input` |
| `Cancelled` | `cancelled` | possibly |
| `GuardUnavailable` | `guard_unavailable` (not stored) | no |

`GuardUnavailable` means the guard could not reach its own tables. No provider
call was made, so the caller may retry later or hand off.

A reservation whose process died is ended as `failed` / `abandoned` once its
lease passes (the client's total timeout plus 30 seconds).

## Budgets

Admins set four budgets over one rolling window of `window_hours` (default 24):

| Setting | Default | Counts |
|---|---|---|
| `org_request_limit` | 500 | admitted provider calls, whole organization |
| `user_request_limit` | 10 | admitted provider calls for one requester |
| `org_token_limit` | none | tokens, whole organization |
| `user_token_limit` | none | tokens for one requester |

`null` means no limit. `0` admits nothing. The window is rolling: a call counts
from the moment it was admitted until `window_hours` later.

**Requests.** Every admitted call counts, whether it then succeeds or fails.
Refusals do not count. The request budgets are exact: they cannot be overshot.

**Tokens.** No provider tells us the cost of a call before it is made, so
admission uses an estimate: 1,000 tokens for the instructions, one token per
two characters of request text, 2,000 per screenshot, and the full output
allowance (8,192). When the call ends, the estimate is replaced:

- by the provider's reported total, or its reported input plus output when it
  reports no total (Anthropic), `budget_basis = reported`;
- by `0` when the provider rejected the call outright (an error status, or it
  was never reached), `budget_basis = none`;
- by nothing when the usage is unknown (no usage in the response, a timeout,
  a cancellation, malformed or oversized output): the estimate stays,
  `budget_basis = estimated`.

A call is admitted only if the tokens already counted, the estimates of calls
in flight, and its own estimate fit the limit. The estimate is deliberately
high, but it is an estimate: a call that really uses more than its estimate
can exceed the token limit by that difference. A token limit below one
estimate (about 10,000 tokens) admits nothing.

Screenshots are part of both: they raise the token estimate, and
`ai_usage.image_count` records how many were sent.

## Concurrency

At most `max_concurrency` provider calls (default 2) are in flight at once,
counted over every process. A call that finds all slots taken waits up to
`concurrency_wait_seconds` (default 30, `0` refuses at once) and is then
refused with `concurrency_saturated`.

## Circuit breaker

One circuit per provider, stored in `ai_circuit`.

- **Closed.** Calls go through. Each failure that says the provider or the key
  is unhealthy (`timeout`, `upstream_unavailable`, `rate_limited`,
  `invalid_credentials`, `destination_blocked`) counts. Failures about one
  request (`model_error`, `vision_failed`, `malformed_output`,
  `oversized_output`, `invalid_input`) and cancellations do not. Any success
  resets the count.
- **Open.** After `breaker_failure_threshold` consecutive counted failures
  (default 5) calls are refused with `circuit_open` for
  `breaker_cooldown_seconds` (default 60), or for the provider's `Retry-After`
  if that is longer (at most one hour). No provider call is made.
- **Half-open.** After the cooldown exactly one call is let through as a probe
  (`ai_usage.breaker_probe`). Other calls are still refused while it runs. If
  it succeeds, or fails for a request-specific reason, the circuit closes. If
  it fails with a counted failure, the circuit opens again for a full cooldown.

Storing a new key for a provider, or selecting another provider or model,
closes that provider's circuit: old failures say nothing about the new setup.

The breaker uses the database clock (`clock_timestamp()`), so the API process
and its workers agree, and tests move time by editing `ai_circuit.open_until`.

## Privacy

### What the provider receives

Only what `SuggestionRequest` can hold: category, subject, description, the
visible conditional answers, and the intake screenshots, plus the fixed system
instructions. Nothing else exists in the type: no internal notes, no later
messages, no other tickets, no account data, no hidden or stale answers.
The attempt worker builds the request from the frozen intake snapshot
([ai-attempts.md](ai-attempts.md)), which is minimized before it is stored.

`minimize(raw) -> Minimized { request, redactions, truncated }`:

- redacts `subject`, `description`, and answer values with `redact_secrets`;
- replaces the whole answer of a question that itself asks for a secret
  (label "Password", "API key", "Kennwort", ...);
- leaves `category` and answer labels (Admin-defined catalog text) as written;
- cuts every text to the adapter bound and the answers to 100, so a minimized
  request never fails the adapters' text limits;
- passes images through unchanged. The adapters enforce the image limits
  (count, bytes, dimensions) and send an image as a real image input or fail
  the call with `vision_failed`; they never downscale or describe it.

`redact_secrets(text) -> Redacted { text, redactions }` replaces these with
`[redacted]`:

- private key blocks (`-----BEGIN ... PRIVATE KEY-----`), also cut-off ones;
- the value after a secret-naming word: `password: x`, `Passwort = x`,
  `"api_key":"x"`, `--token=x`, `?token=x`, `Authorization: Bearer x`,
  `my password is x1!`, `PIN 4821`;
- the password in a URL (`scheme://user:password@host`);
- tokens in well-known formats (`sk-...`, `ghp_...`, `AKIA...`, `xoxb-...`,
  JWTs) and other random-looking strings of 32 or more characters. UUIDs stay.

`AiGuard::suggest` minimizes every request itself, so no caller can send
unredacted or unbounded text to a provider.

Redaction is pattern matching. It catches secrets that look like secrets. Known
limits: a password written as an ordinary word in a sentence without a
separator, a multi-word passphrase (only its first word is replaced), secrets
passed as command-line arguments (`curl -u user:secret`), and one-time codes
without a label. It does not look inside screenshots: a screenshot that shows a
password is sent as it is. Requesters are told before submission that their
description, details, and screenshots go to the configured provider (#36).

The system instructions (in `ai/prompt.rs`) are separate from requester
content, declare it untrusted, and forbid following instructions found in it
or in screenshots, claiming to have executed anything, and asking for or
revealing credentials.

### What is stored

One `ai_usage` row per guarded call or refusal:

- requester account id, provider, model, number of images;
- status (`reserved`, `succeeded`, `failed`, `refused`) and the stable outcome
  code;
- the provider's HTTP status and its short error identifier (at most 64
  characters of `[A-Za-z0-9_.-]`), when there was a response;
- `input_tokens`, `output_tokens`, `total_tokens` exactly as the provider
  reported them. `NULL` means the provider did not report that value. Nothing
  is summed or estimated into these columns;
- `budget_tokens` and `budget_basis` (see Budgets): what the row counts
  against token budgets and whether that number is `reported`, `estimated`, or
  `none`;
- timestamps.

Not stored in `ai_usage`, logs, or audit events: prompt text, ticket content,
the suggestion, image bytes or base64, text read from screenshots, provider
message text, API keys. The minimized input and the one suggestion of a ticket
are stored with its attempt record only ([ai-attempts.md](ai-attempts.md)).

**No cost.** Tikkit has no price information for any model, so it stores and
reports no cost, not even an estimate. Token counts are the provider's own
figures; the only number Tikkit computes is the admission estimate, and it is
always labelled as one (`budget_basis`, `estimated_tokens` in the report).

### Logs

The guard logs one `info` line per finished call or refusal with the usage id,
provider, model, image count, status, outcome code, and reported token counts.
Database errors are logged as a code, never the database message. The adapters
log nothing. Tests capture all log output at `trace` level for requests that
contain secrets and assert that no secret, ticket text, suggestion text, image
data, or key appears in logs, `ai_usage`, `ai_circuit`, or `audit_events`.

## Retention

`retention_days` (default 90, at least as long as the budget window) is how
long `ai_usage` rows are kept. The `ai_usage_retention` background task
(started with the API, same polling pattern as the notification and auto-close
workers) deletes older finished rows hourly and at startup, in batches of
1,000, and writes one `ai_usage_purged` audit event per batch with the number
deleted. It also ends abandoned reservations on every poll. Deleted rows are
gone from reports; no aggregate is kept. Database backups keep their own copy
until the backup itself expires.

## Admin API

All three need `ManageSettings` (Admin). Other roles get 403.

| Endpoint | Purpose |
|---|---|
| `GET /api/settings/ai/limits` | Budgets, limits, retention |
| `PUT /api/settings/ai/limits` | Replace them (all fields required; `null` removes a `*_limit`). Audited as `ai_limits_updated` with old and new values |
| `GET /api/settings/ai/usage?days=30` | Usage report |

Bounds, enforced by the API and again by table constraints:

| Field | Range |
|---|---|
| `window_hours` | 1 to 744 |
| `org_request_limit`, `user_request_limit` | `null` or 0 to 1,000,000 |
| `org_token_limit`, `user_token_limit` | `null` or 0 to 1,000,000,000,000 |
| `max_concurrency` | 1 to 64 |
| `concurrency_wait_seconds` | 0 to 300 |
| `breaker_failure_threshold` | 1 to 100 |
| `breaker_cooldown_seconds` | 1 to 86,400 |
| `retention_days` | 1 to 3,650, and `retention_days * 24 >= window_hours` |

Violations return 422 `validation_failed` with `out_of_range` per field, or
`shorter_than_window` on `retention_days`. Changes apply to the next admission.

The report (`days` 1 to 3,650, default 30) contains:

- `window`: the organization's request and token use in the current budget
  window against the limits, with the estimated part of the tokens shown
  separately, and the calls in flight;
- `totals` and `providers[]` (always all three providers): admitted calls,
  successes, failures, refusals, images sent, the sums of provider-reported
  input, output, and total tokens (`null` when no call reported that value;
  Anthropic never reports a total), and `usage_unreported`, the successful
  calls for which the provider reported no token count;
- `providers[].circuit`: `closed`, `open`, or `half_open`, the consecutive
  failures, and `open_until`;
- `outcomes[]`: failed and refused calls by stable code.

The Admin page `/admin/ai` shows the report and edits the limits.

## Provider differences

Budgets, concurrency, the breaker, redaction, and storage behave the same for
OpenAI, Gemini, and Anthropic. Only the token metadata differs, and it is
stored as it arrives: OpenAI and Gemini report a total (Gemini's includes
thinking tokens, so it can exceed input plus output); Anthropic reports input
and output only, so its `total_tokens` is `NULL` and its budget figure is the
sum of the two reported values.

## The caller

The suggestion-attempt worker is the caller of `AiGuard::suggest`
([ai-attempts.md](ai-attempts.md)). It builds the request from the frozen
intake snapshot, passes the ticket owner's account id, and on any
`Err(Refused)` hands the ticket to operators exactly once, storing
`refusal.code()` and the usage id on the attempt.
