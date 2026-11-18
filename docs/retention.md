# Data retention, anonymization, and personal-data export

Admins decide how long stored data is kept, see exactly what a deletion would
remove before it runs, remove the identity of one account, and export one
account's personal data. Everything here is Admin only.

Code: `apps/api/src/retention.rs` (policy, preview, runs, worker),
`apps/api/src/personal_data.rs` (summary, export, anonymization), migration
`0028_data_retention.sql` ([schema.md](schema.md#data-retention)); web
`apps/web/src/routes/(app)/admin/retention/` and `apps/web/src/lib/retention.ts`.

## Defaults

**Nothing is deleted until an Admin configures a policy.** After installation
and after upgrading:

| Setting | Default |
| --- | --- |
| Every retention period (drafts, screenshots, AI content, ticket content, audit log) | not set: keep indefinitely |
| Apply automatically | off |

With no period set there is nothing to preview, a run cannot be started
(`422 policy: not_configured`), and the retention worker does nothing. Two
older, narrower clean-ups keep running as before, because they only remove
data nobody can reach any more: unattached screenshot uploads that no draft
references expire after 24 hours ([screenshots.md](screenshots.md#expiry)), and
AI usage figures (metadata, no content) follow their own period, 90 days by
default ([ai-usage.md](ai-usage.md#retention)).

## The policy

Five periods in whole days, each `null` (keep) or 1 to 3,650; the audit log has
a floor of 30 days (`audit::RETENTION_FLOOR_DAYS`, enforced again by the
database). A record has expired when it is **strictly older** than its period.

| Period | Applies to | Clock starts | What a run does |
| --- | --- | --- | --- |
| `draft_days` | Unsubmitted drafts | Last save (`updated_at`) | Deletes the draft. Uploads only it referenced become orphans and expire through the screenshot clean-up |
| `screenshot_days` | Screenshots (intake, replies, internal notes) of tickets in a final status | When the ticket reached its final status | Deletes the rows, then the files |
| `ai_content_days` | The minimized AI input and the stored suggestion of tickets in a final status | Same | Replaces the input with `{}` and the suggestion with the marker; the attempt row, its outcome, provider, model, and times stay |
| `ticket_content_days` | Subject, description, intake answers, every message body (public and internal), and the free-text cancellation reason of tickets in a final status | Same | Replaces the text with the marker and drops the reason from the `cancel` event. Also removes the ticket's screenshots and AI content, whatever their own periods say |
| `audit_days` | Audit events | Event time | Deletes them through `audit::purge_before`; the `audit_retention_purged` record itself is never deleted |

- **Final status** means `closed`, `cancelled`, or `ai_solved`. The clock is
  the time of the event that put the ticket into the status it has now
  (`ticket_final_at()`); a reopened ticket is open again and does not expire.
  Open tickets, AI-stage tickets, and everything attached to them are never
  touched, however old.
- **AI-solved records are treated exactly like operator-handled ones.** Being
  hidden from operators is not deletion: their content stays stored until the
  ticket content (or AI content) period removes it, and the same run removes
  both kinds.
- The marker is `—` (`retention_marker()`, `retention::REMOVED`). Tickets carry
  `content_removed_at` and AI attempts `content_removed` in the API, and the
  ticket pages show a localized notice. A ticket whose content was removed
  cannot be reopened (`409`).
- **What stays, always:** the ticket row with its ID, reference number, status,
  category, priority, team, assignee, requester ID, provenance
  (`resolution_source`, `closure_reason`), and timestamps; every `ticket_events`
  row; message rows with author, kind, and time; the AI attempt's state and
  outcome. Reports and the Admin submission list are computed from exactly
  these, so **every count, rate, and duration is the same before and after**
  ([admin-reports.md](admin-reports.md)). The submission list keeps listing
  such tickets, with the marker as their subject.
- AI usage rows (`ai_usage`) hold no content and keep their own period
  ([ai-usage.md](ai-usage.md#retention)); this policy does not touch them.
- The cancellation reason is also copied into the audit log
  (`ticket_cancel`, [audit.md](audit.md#never-in-an-event)). That copy leaves
  only with the audit period. Set `audit_days` if the reason must not outlive
  the ticket content indefinitely.

## Preview and runs

1. **Preview** (`POST /api/admin/retention/preview`): exact counts per category
   for the periods sent, saved or not. A read-only transaction; nothing
   changes and nothing is audited.
2. **Save** (`PUT /api/admin/retention`): stores the policy and writes
   `retention_settings_updated` with the old and new values. Saving deletes
   nothing, unless "apply automatically" is on (then `confirm: "DELETE"` is
   required, because the worker will delete without asking again).
3. **Run** (`POST /api/admin/retention/runs` with `confirm: "DELETE"`):
   starts one run under the saved policy. The page only enables the button
   after the word is typed; the API checks it too. One run at a time (`409`).

A run (`retention_runs`) snapshots the periods and its clock (`as_of`), so it
means the same thing after a restart. The preview and the run share their
predicates: a run started at the instant of a preview removes exactly the
previewed counts.

**Restart safety.** The retention worker (`retention::worker_loop`, in the API
process) takes the pending run with `FOR UPDATE SKIP LOCKED`, removes one batch
(100 tickets or drafts) and adds the batch's numbers to the run's `counts` in
the same transaction. A crash or a failed batch rolls back the removals and
their counts together; the next poll continues. A second API process skips the
locked run, so nothing is counted or removed twice. The removal functions are
no-ops on anything already removed. Tickets are locked while their content is
removed, so a concurrent reopen either happens first (the ticket is then open
and skipped) or is refused afterwards.

**Files.** Screenshot rows are deleted in the batch; their files are removed
right after the commit. If the process dies in between, the file has no row
any more and the existing stray-file sweep of `screenshots::cleanup` deletes
it (files older than 24 hours without a row). A row never points at a missing
file because of retention, and a live ticket's file is never a candidate: the
database refuses to delete a screenshot row unless its ticket is in a final
status.

**Automatic.** With "apply automatically" on, the worker starts one run per
day under the saved policy (actor: the `retention` service).

**Audit.** `retention_run_started` (who, the periods, the clock) and
`retention_run_completed` (the periods, who requested it, and the counts per
category), plus `audit_retention_purged` from the audit path. Counts and
settings only: never ticket text, file names, or account names.

**Observability.** The worker reports as `worker="retention"` in
`tikkit_worker_failures_total`, `_retries_total`,
`_last_success_timestamp_seconds`, and the backlog gauges (a pending run is a
pending and due job; lag is its age). It runs under `telemetry::job` with the
request ID of the Admin request that started the run, so its log lines and
audit events correlate with it. See [operations.md](operations.md#data-retention).

## How the immutability rules are kept

Submitted intake, AI-solved records, messages, ticket events, attached
screenshots, and AI attempts are immutable for every normal action. Migration
`0028` does not disable those triggers. It adds three SQL functions, and the
guards recognise the one statement inside each by the call stack, the same
mechanism as the audit log's two exceptions ([audit.md](audit.md#append-only-retention-and-anonymization)):

| Function | May |
| --- | --- |
| `retention_delete_screenshots(ticket)` | delete screenshot rows of a ticket in a final status |
| `retention_purge_ai(ticket)` | blank `input`, `explanation`, `steps` of a finished attempt of such a ticket |
| `retention_purge_ticket(ticket)` | set subject, description, and the snapshot's text to the marker, message bodies to the marker, and drop `reason` from `cancel` events |

Each guard then checks the row itself: only a final-status ticket, only those
columns, only to the marker. The same statement sent directly, from a `DO`
block, or from a function of another name is refused (`23001`). No event row
is ever deleted and no status, team, assignee, or timestamp can change this
way. The limit is the one stated for the audit log: the database role that owns
these objects could replace them.

## Anonymizing an account

`POST /api/admin/accounts/{id}/anonymize` removes the identity of one account
for good. Preconditions:

- The account is **deactivated**. Deactivation already refuses the last active
  Admin and the lead of an active team, so neither can be anonymized, and an
  Admin cannot anonymize themselves.
- The body's `username` is the current username of the account with that ID
  (`422 username: mismatch` otherwise). Accounts are found by ID and username
  only. Email addresses are never compared: another account with the same
  address is a different account and is not touched.

What is removed or replaced:

| Data | After |
| --- | --- |
| `username`, `display_name` | `anonymized-<account id without dashes>` |
| `email` | empty |
| `password_hash` | unusable |
| Specialist `expertise`, notification preferences | cleared / off |
| Sessions, password-reset tokens | deleted |
| Unsubmitted drafts, unattached screenshot uploads (rows and files) | deleted |
| Saved views, in-app notifications, queued notifications | deleted |
| The email address its invitation was sent to | cleared |
| Audit log | `audit::anonymize_account`: the username snapshots of this account become `null` and the `ip` key is removed from its events |

What stays, so that history and statistics still add up:

- The account row with its **ID**, role, team membership, creation time, and
  `anonymized_at`. It stays deactivated: it cannot sign in, recover a password,
  or be reactivated (`409`).
- Every reference to the ID: tickets it submitted, tickets it worked on,
  messages it wrote, ticket events, audit events, AI usage, membership history.
  Staff views show the placeholder where the name used to be.
- Reports are computed from tickets and events and never from names, so they
  are identical before and after.
- **Ticket content is not identity.** By default the account's tickets keep
  their text until the ticket content period removes it. With
  `purge_ticket_content: true` the content of the account's own tickets that
  are in a final status is removed in the same transaction, as a run would.
  Open tickets stay open with their content so the help desk can finish them;
  they follow the ticket content period afterwards.

Audit events that carry an IP address but no account (`invitation_rejected`,
`setup_rejected`) cannot be attributed to an account and leave only through
the audit period.

The call writes `account_anonymized` (who, the account ID, counts) and the
audit log's own `audit_account_anonymized`. `GET /api/admin/personal-data?username=`
shows, without changing anything, how much is stored about an account.

## Personal-data export

`POST /api/admin/accounts/{id}/personal-data/export` returns one JSON document
(`format: tikkit.personal_data.v1`) as an attachment, `private, no-store`. It
requires the same typed `username` check and writes `personal_data_exported`
(who, the account ID, row counts per section).

| Section | Contents |
| --- | --- |
| `account` | ID, username, display name, email, role, language, active flag, creation time, expertise, notification preferences, current team |
| `team_history` | Joins and leaves with team name and time |
| `sessions` | Creation, last use, expiry, revocation, user agent, IP address |
| `tickets` | Tickets the account submitted: reference, status, subject, description, category, priority, intake answers, times, outcome |
| `ticket_actions` | Ticket events the account caused (on any ticket): ticket reference, event, statuses, time, and its own cancellation reason |
| `conversation` | Public replies on the account's own tickets: time, text, and whether the account or `staff` wrote it |
| `staff_messages` | Messages the account wrote on other people's tickets: ticket reference, kind, time. No text |
| `ai_suggestions` | The AI suggestion shown on the account's tickets |
| `screenshots` | Metadata of screenshots the account uploaded to its own tickets or has pending. No image bytes |
| `drafts` | Unsubmitted drafts with their input |
| `saved_views` | Names and filters |
| `ai_usage` | Provider, model, status, tokens, time of AI calls made for the account |
| `audit_events` | Time, action, and IP address of audit events the account caused, newest first |

Never included: password and token hashes; AI provider keys and SMTP
credentials (no credential table is read); internal notes, including those
staff wrote about this requester (they are the help desk's private working
notes); the names or IDs of staff who replied; the text of anything the account
wrote as staff on other people's tickets (it is about those people); other
accounts' tickets; image bytes; the AI input (a copy of the ticket's own
intake, which is already in `tickets`).

Bounded: each section holds at most 10,000 rows; `truncated` lists the sections
that were cut.

## Admin API

All need the Admin role; every other role gets `403 forbidden` with no data,
signed out is `401`. Mutations need the CSRF header.

| Endpoint | |
| --- | --- |
| `GET /api/admin/retention` | Policy, bounds, and the latest 10 runs with their counts |
| `PUT /api/admin/retention` | Replace the policy. All five periods required (`null` = keep). `422`: `out_of_range`, `confirm: invalid` |
| `POST /api/admin/retention/preview` | Dry run for the periods in the body |
| `POST /api/admin/retention/runs` | Start a run (`202`). `422 confirm: invalid`, `422 policy: not_configured`, `409` while one is pending |
| `GET /api/admin/personal-data?username=` | Counts of what is stored about one account; `404` if no account has that username |
| `POST /api/admin/accounts/{id}/personal-data/export` | The export |
| `POST /api/admin/accounts/{id}/anonymize` | Anonymize. `422`: `username: mismatch`, `account: active`, `account: already_anonymized` |

## Backups

Deleting or anonymizing data changes the live database and the live
screenshot volume. **It does not change backups made earlier.** A backup taken
before a run still contains the removed drafts, screenshots, ticket text, AI
content, audit events, and the identity of an anonymized account, until that
backup itself expires.

Consequences for the operator (the backup and restore procedure itself is
described with the backup tooling; this is the retention side of it):

- **Backup retention bounds real deletion.** Data is gone for good only once
  every backup that contains it has expired. Keep backups no longer than you
  are prepared to keep deleted data, and state the two periods together: for
  example, "ticket content 365 days, backups 35 days" means removed content can
  exist for up to 400 days.
- **A restore brings removed data back.** After restoring a backup, expired
  data and anonymized identities from before that backup are live again.
  Retention is idempotent and its state is in the database, so:
  - With "apply automatically" on, the worker removes the expired data again
    within a day of the restore (the periods are in the restored database; if
    the policy was changed after the backup, set it again first).
  - Otherwise start a run after every restore.
  - **Anonymizations are not replayed automatically.** Keep a record outside
    the database of the account IDs anonymized since the oldest backup you
    retain (the `account_anonymized` audit events list them; note them before
    they are lost with a restore) and anonymize those accounts again after a
    restore.
- **Database and screenshot volume go together.** Restoring a database dump
  with a newer screenshot directory leaves files without rows, which the
  stray-file sweep removes. Restoring an older directory under a newer dump
  only loses files of rows that still exist ([screenshots.md](screenshots.md#storage-backup-and-restore));
  retention never creates that state.
- Retained data restores as itself: nothing retention keeps (ticket skeletons,
  events, counts) depends on anything it removes.
- Backups should be encrypted and access-controlled like the live data: they
  are the place where "deleted" data lives longest.

## Tests

`apps/api/tests/retention.rs`: the defaults delete nothing; expiry just
before, at, and after the boundary for tickets and drafts; dry-run counts equal
the deletions for operator-handled, cancelled, and AI-solved tickets together;
reports identical before and after retention and after anonymization; a crash
mid-batch and two concurrent workers; a crash between row and file deletion;
screenshots of live versus expired tickets; the immutability triggers still
refusing everything else; the 30-day audit floor; settings validation,
confirmation, and audit events; the daily automatic run and its metrics; role
denial for every non-Admin role; export contents and exclusions; anonymization
with the shared-email account, the last active Admin, and reactivation.
`apps/web/src/lib/retention.test.ts` and `apps/web/e2e/admin-retention.spec.ts`
cover the page.
