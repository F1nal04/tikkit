# Audit log

One append-only table, `audit_events`, records who did what and when:
sign-ins, bootstrap and invitations, role, team, and leadership changes,
catalog and policy settings, SMTP and AI configuration, exports, retention, and
every ticket event. Admins search and export it at `/admin/audit`.

Code: `apps/api/src/audit.rs` (writer, denied attempts, retention hook, search,
export), `apps/api/src/csv.rs`; web `apps/web/src/routes/(app)/admin/audit/` and
`apps/web/src/lib/audit.ts`. Migration `0026_audit_events.sql`
([schema.md](schema.md#audit-events)).

## What an event holds

| Field | Meaning |
| --- | --- |
| `action` (`event_type`) | Stable snake_case identifier, the same in every language |
| `actor` | The account's immutable ID with its username, role, and team **as they were when the event was written**; or, when no account acted, a `service` name |
| `subject` | The account the event is about, if any (ID and username) |
| `resource_type`, `resource_id` | What the event is about: `ticket`, `account`, `invitation`, `team`, `category`, `priority`, `intake_field`, `organization`, `smtp`, `ai_settings`, `ai_credential`, `screenshot`, `draft`, `audit_log`, `retention` |
| `ticket_id`, `ticket_reference` | Set for events about a ticket |
| `created_at` | UTC |
| `request_id` | The correlation ID (UUID) of the HTTP request that wrote it, equal to the response's `x-request-id`; for a background job, the ID of the request that caused the job. `null` only where there is none (maintenance jobs, history from before the column) |
| `metadata` | Identifiers, stable codes, and before/after settings |

Accounts are identified by ID. Two accounts that share an email address, or an
Admin account and an operational account of the same person, are different
actors everywhere; nothing matches or merges by email. The username filter
resolves to exactly one account ID.

Service names: `anonymous` (no session: a failed login, an invitation link),
`auto_close` (confirmation timeout), `ai_attempts` (suggestion worker),
`ai_usage_retention`, `screenshot_cleanup`, `retention`, `system`.

The actor snapshot and the resource are filled by a database trigger
(`audit_events_fill`), so no writer can leave them out. A constraint requires
exactly one of account or service.

### Never in an event

Passwords, password hashes, API keys, SMTP credentials, session, invitation,
and reset tokens or links, and ticket or message text. Events name things by
ID. Two exceptions are deliberate: a requester's cancellation `reason` (the
issue requires it) and catalog item names in catalog events.

- Ticket events are mirrored through an allow-list of payload keys
  (`audit_ticket_metadata`). A resolution proposal is recorded by version,
  deadline, and timeout, without its summary. **A new ticket event payload key
  reaches the audit log only after it is added to that list**; add only
  identifiers, codes, and settings.
- `audit::record` replaces whatever sits under a key containing (in any
  casing) `password`, `pass`, `pwd`, `token`, `secret`, `api_key`,
  `private_key`, `credential`, `bearer`, `authorization`, or `cookie` with
  `[redacted]`: strings, lists, and nested objects alike. Booleans and numbers
  under such keys stay, so flags (`password_changed: true`,
  `credential_replaced`) and numeric settings (`org_token_limit`) remain
  readable. This is a backstop, not permission to pass secrets.

## Writing events

```rust
use crate::audit::{self, Entry, Who};

audit::record(&mut *tx, Entry {
    action: "report_exported",
    who: Who::Account(user.account_id()),
    subject: None,
    ticket: None,
    metadata: json!({ "rows": rows }),
}).await?;
```

- `auth::audit(db, action, Some(actor), subject, metadata)` is the same call in
  its older, shorter form (an absent actor is `anonymous`).
  `audit::system(db, service, action, metadata)` is for background jobs.
- **Atomicity.** Write the event on the transaction of the change. They commit
  or roll back together. For tickets this is not left to callers: an
  `AFTER INSERT` trigger on `ticket_events` (`ticket_events_audit`) writes the
  audit row in the same statement, so a ticket change cannot commit without
  its audit event, and if the audit write fails the change fails.
- **Request ID.** There is one source: `telemetry::request_id()`
  ([operations.md](operations.md)). `audit::record` binds it for every event,
  exactly as the `ticket_events` writers do, and the mirror trigger copies the
  ticket event's `request_id`, so a ticket event and its audit row always
  carry the same ID. Inside a request it is the ID in the `x-request-id`
  response header. Workers run under `telemetry::job` with the ID of the
  request that caused the work (auto-close: the proposal; AI attempts and the
  sweep: the submission; notifications: the source event), so their audit rows
  correlate with that request. The audit module neither reads headers nor
  generates IDs. A client cannot choose an ID: the public proxy strips
  `X-Request-ID`, and the API only takes over a hyphenated UUIDv4 (the private
  SSR server's) and replaces anything else.
- New actions need no registration. Pick a stable name; give the event a
  resource by putting a conventional key in the metadata (`ticket_id`,
  `invitation_id`, ...) or by a rule in `audit_resource_of` in a new migration.

## Ticket events

Every `ticket_events` row appears as `ticket_<event_type>`: `ticket_submit`,
`ticket_ai_suggestion_ready`, `ticket_ai_failed`, `ticket_skip_ai`,
`ticket_ai_solved`, `ticket_ai_not_solved`, `ticket_start_work`,
`ticket_request_information`, `ticket_requester_responded`,
`ticket_propose_resolution`, `ticket_accept_resolution`,
`ticket_reject_resolution`, `ticket_confirmation_timeout`, `ticket_reopen`,
`ticket_cancel`, `ticket_priority_changed`, `ticket_assignment_changed`,
`ticket_public_reply_added`, `ticket_internal_note_added`. Metadata is the
allow-listed payload plus `from_status`, `to_status`, `ticket_event_id`, and:

| Event | Added |
| --- | --- |
| `ticket_submit` | `route` (the effective routing reason), `category_id`, and the requester's original `priority_id` / `priority_name` |
| `ticket_priority_changed` | previous and new priority, ID and name |
| `ticket_assignment_changed` | `action`, previous and new team and assignee |
| `ticket_ai_failed` | `outcome`: the attempt's stable outcome code, e.g. `provider_auth_failed`, `timeout`, `credential_missing`, `category_exempt` ([ai-attempts.md](ai-attempts.md)). A code, never a provider message. It is read from the attempt, not from the ticket event payload, which requesters can read |
| `ticket_propose_resolution` | `proposal_version`, `deadline`, `timeout_hours` |
| `ticket_confirmation_timeout` | the proposal's `deadline` and `timeout_hours` snapshot, `closure_reason: confirmation_timeout`; actor is the `auto_close` service, never the requester |
| `ticket_reopen` | `allow_reopening` as it was |
| `ticket_cancel` | `reason`, `cancellation_policy` as it was |

`GET /api/admin/audit?ticket_id=...` (or `ticket_reference=`) is the ticket's
whole timeline. An AI decision reads, oldest first: `ticket_submit`
(`route: ai`), `ticket_ai_suggestion_ready` (service `ai_attempts`), then
`ticket_ai_solved` or `ticket_ai_not_solved` by the requester with the
`attempt_id`. A bypass is `ticket_skip_ai` or a `ticket_submit` with route
`category_exempt`, `requester_skip`, or `ai_disabled`; a failed generation is
`ticket_ai_failed`.

## Denied attempts

Recorded by `audit::ticket_denied` after the failed transaction is gone. It
writes one audit row and no ticket event, so the ticket, its history,
notifications, and every statistic stay as they were.

| Action | When |
| --- | --- |
| `ai_solved_change_denied` | A failed attempt to reopen, escalate, or otherwise change the status, priority, or assignment of an AI-solved record through the requester actions, staff actions, or assignment endpoints (also bulk), by a caller who can see the record (its requester, an Admin), whatever the response was (`403`, `409`). Metadata: `attempted`, `status`, `error`. Repeating the committed `ai_solved` decision is a retry and is not recorded |
| `ticket_reopen_denied` | A reopen answered `403`: the toggle is off or the caller may not write. Metadata includes `allow_reopening` |
| `ticket_cancel_denied` | A cancellation answered `403`: the policy forbids it or the caller is not the requester. Metadata includes `cancellation_policy`, never the reason |
| `invitation_rejected` | An acceptance with a used, revoked, expired, or unknown token (`reason`, and the invitation ID when it is known), or with a field the contract does not have such as `role` (`reason: unexpected_field`, `field`). The token is never stored |
| `setup_rejected` | A bootstrap attempt after setup was completed |

Limits, so that denials cannot be used to learn anything or to flood the log:

- **Only for tickets the caller may see.** A ticket-specific row is written
  only when the ticket policy lets the caller see that ticket. For a missing
  ticket and for one the caller cannot see (another requester's ticket, a
  ticket of another team, an AI-solved record for operators) the response is
  the same as before and nothing is written, so neither a row nor its absence
  tells them a ticket exists. Operators never see AI-solved records
  ([ai-decisions.md](ai-decisions.md#who-sees-what)); their requests get `404`
  and no row.
- **Bounded repeats.** One account repeating one action on one ticket is
  recorded at most 5 times per minute; further repeats get the same response
  and no row. `invitation_rejected` for unexpected fields and `setup_rejected`
  are bounded per client IP (10 per minute); the other invitation rejections
  sit behind the acceptance endpoint's own limiter. The limiter is the
  in-memory one of `auth.rs` (per process).
- **Not recorded:** replies, notes, and screenshot attachments aimed at an
  AI-solved ticket (they are refused, but they are not a reopen, escalation,
  or status change), and stale or duplicate resolution decisions (`409`),
  which rewrite nothing.

## Other actions

Authentication: `setup_completed`, `login_succeeded`, `login_failed`, `logout`,
`password_changed`, `session_revoked`, `recovery_requested`,
`recovery_completed`, `profile_updated`. Invitations: `invitation_created`
(with the role), `invitation_accepted`, `invitation_revoked` (reissue appends
both). Accounts and teams: `account_updated` (role and Specialist field, before
and after), `account_deactivated`, `account_reactivated`,
`team_membership_changed`, `team_created`, `team_updated` (leadership
replacement is one event with the previous and the new lead). Settings:
`organization_settings_updated` (reopening toggle, cancellation policy,
confirmation timeout, requester AI skip; before and after),
`catalog_item_created`, `catalog_item_updated` (with `before`, e.g. a
category's `skip_ai`), `catalog_reordered`, `smtp_configured`, `smtp_updated` (both with `from`/`to`:
host, port, security, sender address, and `enabled`, so enabling or disabling
is one event; `credential_replaced` says whether a username or password was
replaced, never what it is), `smtp_removed`, `smtp_tested`, `notifications_retried`, `ai_settings_updated`,
`ai_credential_set`, `ai_credential_removed`, `ai_connection_tested`,
`ai_limits_updated`. Jobs: `ai_usage_purged`, `ai_attempt_orphaned`,
`screenshots_expired`, `retention_run_started`, `retention_run_completed`
(periods and counts per category). Retention and personal data:
`retention_settings_updated` (before and after), `personal_data_exported`
(row counts per section), `account_anonymized` (counts),
`retention_account_content_removed` (the anonymized account, the ticket ID, the
category, the number of rows; written by the database). Files and drafts: `screenshot_uploaded`,
`screenshot_deleted`, `draft_created`, `draft_updated`, `draft_deleted`. The
log itself: `audit_exported`, `audit_retention_purged`,
`audit_account_anonymized`, `audit_ticket_text_redacted`.

## Append-only, retention, and anonymization

`UPDATE`, `DELETE`, and `TRUNCATE` on `audit_events` raise `restrict_violation`
(`23001`). This holds for every application code path and for plain SQL sent
through the application's database connection. There are exactly three
exceptions, all SQL functions (migration `0026`, revised and extended by
`0028`). Only the data retention feature calls them
([retention.md](retention.md)): retention runs call `purge_batch` with the
Admin-set audit period, account anonymization calls `anonymize_account`, and
the removal of a ticket's content calls `audit_redact_ticket_text`. There is no
endpoint that edits or deletes individual events.

**Retention**

```rust
audit::purge_before(&mut tx, cutoff, Who::Account(admin_id)).await?; // or Who::Service(audit::RETENTION)
```

`audit_retention_purge(cutoff, admin)` deletes events older than `cutoff` and
appends `audit_retention_purged` (who, cutoff, count) in the same call.
`audit_retention_purge(cutoff, admin, max_rows)` (`audit::purge_batch`) does
the same for at most `max_rows` events, oldest first, so a long history is
deleted in bounded transactions: call it until it returns 0. Each call that
deleted something appends its own purge record; a batched call that found
nothing appends none. The rules below hold for both forms.

- `admin` must be an active Admin account, or NULL for the `retention`
  service; anything else is `42501`.
- **The retention floor is 30 days**: a cutoff less than 30 days in the past
  is refused (`23514`). The number lives in SQL (`audit_retention_floor()`) and
  in Rust (`audit::RETENTION_FLOOR_DAYS`); a retention setting must be
  validated against the same value.
- `audit_retention_purged` events are never deleted, whatever their age.

**Anonymization**

```rust
audit::anonymize_account(&mut tx, account_id, Who::Account(admin_id)).await?;
```

`audit_anonymize_account(account, admin)` sets the username snapshots of that
account to NULL (`actor_username` where it acted, `subject_username` where it
is the subject) and removes the `ip` key from those events' metadata. From
events about the account it also removes the free text they can carry about
it: the Specialist expertise before and after an update (`from_expertise`,
`to_expertise` of `account_updated`). Account
IDs, actions, timestamps, roles, resources, and all other metadata stay, so
the history remains attributable to the (anonymized) account ID. It appends
`audit_account_anonymized` about the account, without a username, and returns
the number of events changed. Same rule for `admin`. New events snapshot
`accounts.username` as it is then, so anonymize the account row first.

**Ticket text**

`audit_redact_ticket_text(ticket)` replaces the one piece of ticket text the
log holds, the requester's cancellation `reason` on `ticket_cancel`, with the
marker `—` for that ticket's events, and appends `audit_ticket_text_redacted`
(the ticket ID and the number of events). It refuses (`42501`) a ticket whose
content retention has not removed, so it cannot be used to edit the log of a
live ticket; it is called by `retention_purge_ticket` and has no Rust wrapper.
The event, its actor, its statuses, and its time stay.

**How the guard tells these apart, and its limit.** The trigger
`audit_events_guard` does not look at a setting. It reads the call stack
(`GET DIAGNOSTICS ... PG_CONTEXT`) and lets a row through only when the
statement is the one `DELETE` inside `audit_retention_purge`, the one
`UPDATE` inside `audit_anonymize_account`, or the one `UPDATE` inside
`audit_redact_ticket_text`. It then checks each row itself: a
delete only of an event older than the floor that is not a purge record; an
anonymization update only if nothing but the username snapshots (to NULL), the
`ip` key, and the two expertise keys changed; a redaction update only of a
`ticket_cancel` event of a ticket whose content was removed, and only if
nothing but `reason` (to the marker) changed. `SET LOCAL`, a `DO` block, or a function of another name cannot
produce that stack.

What this does not protect against: the application's database role owns the
table, the trigger, and these functions. Someone who can run DDL as that role,
or a superuser, can replace a function or disable the trigger; no trigger can
stop its owner. Running the application under a separate role that does not
own the schema would close that gap. It is optional hardening and not part of
this feature. PostgreSQL 17 is pinned; a change to the `PG_CONTEXT` format in a
later major version would make the guard refuse the three functions (fail
closed), which the tests in `apps/api/tests/audit.rs` would catch.

`ticket_events` stays append-only: no event is ever deleted. Ticket content
retention replaces the free text in it, the `reason` of a `cancel` event and
the `summary` of a `propose_resolution` event, with the marker through its own
guarded path (migration `0028`,
[retention.md](retention.md#how-the-immutability-rules-are-kept)), and the
copy of the reason in the audit mirror in the same transaction. The audit
mirror holds no foreign key to tickets or teams.

## Admin API

All three need `view_reports` (Admin only): every other role gets
`403 forbidden` with no data, signed out is `401`.

| Endpoint | |
| --- | --- |
| `GET /api/admin/audit` | Search, newest first, standard cursor paging ([api-conventions.md](api-conventions.md#lists-pagination-ordering-filtering)). Runs in a `READ ONLY` transaction and writes nothing: reviewing the log is not itself logged, like viewing a submission |
| `GET /api/admin/audit/actions` | The action identifiers present in the log, for the filter |
| `POST /api/admin/audit/export` | The same filters as CSV. Needs the CSRF header. Writes `audit_exported` (filters, row count, truncated) |

Filters (all optional, combined with AND, applied in SQL before the limit):

| Parameter | Matches |
| --- | --- |
| `actor` | Acting account ID |
| `actor_username` | Acting account by username (exact, case-insensitive) |
| `action` | One action identifier |
| `resource_type`, `resource_id` | The resource |
| `ticket_id`, `ticket_reference` | Events about one ticket |
| `request_id` | Everything one request wrote, and what the jobs it caused wrote (a UUID; anything else is `400`) |
| `created_from`, `created_before` | RFC 3339 instants: inclusive start, exclusive end |

An unknown parameter, a malformed value, or an empty time range is
`400 invalid_query`. A username or reference that does not exist matches
nothing.

### Export

CSV (RFC 4180, UTF-8, CRLF, every cell quoted) with a header row: `id`,
`time_utc`, `action`, the actor and subject columns, `resource_type`,
`resource_id`, `ticket_reference`, `request_id`, `metadata` (JSON), `ticket_id`.
At most 10,000 rows, newest first; `x-export-rows` and `x-export-truncated`
report the bound, and the page tells the Admin to narrow the filters. A cell
starting with a formula prefix, tab, or line break is prefixed with `'`
so a spreadsheet shows it as text (`csv::cell`). The check covers leading
whitespace and quotes, as well as full-width formula prefixes. Report exports
use the same encoder. The response is
`private, no-store`.

## Upgrading

The migration keeps existing events and adds to them what can still be known:
usernames (as they are at upgrade time), a service name, and the resource.
Role, team, and request ID are `null` for audit events written before the
upgrade. Existing ticket events are copied into the log with their original
timestamps and their `request_id` (migration `0025`), where they have one.

## Tests

`apps/api/tests/audit.rs`: event fields and request correlation, rollback with
the change (and the change failing when the audit write fails), shared-email
attribution, redaction, denied mutation (also with a session setting or a
`DO` block) and the retention and anonymization paths with their refusals,
bounded and visibility-limited denial records, AI decision
timelines and denied changes to AI-solved records, reopen and cancellation,
resolution, priority, and assignment history with auto-close, privileged
changes, role denial, paging, filters, and the export.
`apps/web/src/lib/audit.test.ts` and `apps/web/e2e/admin-audit.spec.ts` cover
the page.
