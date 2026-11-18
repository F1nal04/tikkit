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
| `resource_type`, `resource_id` | What the event is about: `ticket`, `account`, `invitation`, `team`, `category`, `priority`, `intake_field`, `organization`, `smtp`, `ai_settings`, `ai_credential`, `screenshot`, `draft`, `audit_log` |
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
- `audit::record` replaces string values under keys containing `password`,
  `token`, `secret`, `api_key`, `authorization`, or `cookie` with `[redacted]`.
  This is a backstop, not permission to pass secrets.

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
| `ai_solved_change_denied` | Any failed attempt to change an AI-solved record through the requester actions, staff actions, or assignment endpoints (also bulk), by any role, whatever the response was (`403`, `404`, `409`). Metadata: `attempted`, `status`, `error`. Repeating the committed `ai_solved` decision is a retry and is not recorded |
| `ticket_reopen_denied` | A reopen answered `403`: the toggle is off or the caller may not write. Metadata includes `allow_reopening` |
| `ticket_cancel_denied` | A cancellation answered `403`: the policy forbids it or the caller is not the requester. Metadata includes `cancellation_policy`, never the reason |
| `invitation_rejected` | An acceptance with a used, revoked, expired, or unknown token (`reason`, and the invitation ID when it is known), or with a field the contract does not have such as `role` (`reason: unexpected_field`, `field`). The token is never stored |
| `setup_rejected` | A bootstrap attempt after setup was completed. Rate limited per client |

Stale or duplicate resolution decisions (`409`) are not recorded and rewrite
nothing.

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
category's `skip_ai`), `catalog_reordered`, `smtp_configured`, `smtp_updated`,
`smtp_removed`, `smtp_tested`, `notifications_retried`, `ai_settings_updated`,
`ai_credential_set`, `ai_credential_removed`, `ai_connection_tested`,
`ai_limits_updated`. Jobs: `ai_usage_purged`, `ai_attempt_orphaned`,
`screenshots_expired`. Files and drafts: `screenshot_uploaded`,
`screenshot_deleted`, `draft_created`, `draft_updated`, `draft_deleted`. The
log itself: `audit_exported`, `audit_retention_purged`.

## Append-only, and the one retention path

`UPDATE`, `DELETE`, and `TRUNCATE` on `audit_events` raise `restrict_violation`
(`23001`) for every database session, including the application's. The only
exception is the SQL function `audit_retention_purge(cutoff, admin)`, called
through:

```rust
audit::purge_before(&mut tx, cutoff, Who::Account(admin_id)).await?; // or Who::Service(audit::RETENTION)
```

It deletes events older than `cutoff`, refuses a cutoff in the future, and
appends `audit_retention_purged` (who, cutoff, count), which stays. Nothing
calls it yet: the retention settings, preview, and job belong to the data
retention feature (#43). There is no endpoint that edits or deletes events.

`ticket_events` stays append-only with no retention path. The audit mirror
holds no foreign key to tickets or teams, so it survives whatever retention
later does to them.

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
starting with `=`, `+`, `-`, `@`, tab, or carriage return is prefixed with `'`
so a spreadsheet shows it as text (`csv::cell`). The response is
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
attribution, redaction, denied mutation and the retention path, AI decision
timelines and denied changes to AI-solved records, reopen and cancellation,
resolution, priority, and assignment history with auto-close, privileged
changes, role denial, paging, filters, and the export.
`apps/web/src/lib/audit.test.ts` and `apps/web/e2e/admin-audit.spec.ts` cover
the page.
