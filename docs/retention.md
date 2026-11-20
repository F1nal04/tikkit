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
| `ticket_content_days` | Subject, description, intake answers, every message body (public and internal), and the free text in the ticket's events: the requester's cancellation reason and the staff's resolution proposal summaries | Same | Replaces the text with the marker, in the ticket, its messages, its events, and the copy of the cancellation reason in the audit log. Also removes the ticket's screenshots and AI content, whatever their own periods say |
| `audit_days` | Audit events | Event time | Deletes them through `audit::purge_batch`, 5,000 per transaction; the `audit_retention_purged` records themselves are never deleted |

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
  row with its type, statuses, proposal version, deadline, actor, and time;
  message rows with author, kind, and time; the AI attempt's state and outcome.
  Reports and the Admin submission list are computed from exactly these, so
  **every count, rate, and duration is the same before and after**
  ([admin-reports.md](admin-reports.md)). The submission list keeps listing
  such tickets, with the marker as their subject.
- **What remains of a ticket after its content was removed** is therefore: who
  submitted it and who worked on it (account IDs), what kind it was (category,
  priority), where it went (team), what happened to it and when (events), and
  how many messages there were and who wrote them. No text anybody typed and
  no image remains: `subject`, `description`, the intake answers, every
  message body, `reason` of `cancel` events, and `summary` of
  `propose_resolution` events hold the marker, in `ticket_events` and in the
  audit mirror alike. Those two payload keys are the only free text events
  carry; a new one must be added to `retention_redact_payload()`.
- Screenshot-only retention leaves the IDs in the frozen intake. The API
  returns only the IDs that still exist and a count, `removed_screenshots`,
  which the ticket pages show as a note instead of broken previews. A ticket
  whose text was removed is not offered for reopening (`staff_access.can_reopen`
  is false, the requester page hides the button) and the API refuses it.
- AI usage rows (`ai_usage`) hold no content and keep their own period
  ([ai-usage.md](ai-usage.md#retention)); this policy does not touch them.
## Preview and runs

1. **Preview** (`POST /api/admin/retention/preview`): exact counts per category
   for the periods sent, saved or not, with the clock `as_of` and a `token`. A
   read-only transaction; nothing changes and nothing is audited.
2. **Save** (`PUT /api/admin/retention`): stores the policy, bumps its
   `version`, and writes `retention_settings_updated` with the old and new
   values. Saving deletes nothing, unless "apply automatically" is on; then
   `confirm: "DELETE"` and the `preview_token` of a preview of exactly these
   periods are required, because the worker will delete without asking again.
3. **Run** (`POST /api/admin/retention/runs`): needs `confirm: "DELETE"`, the
   `preview_token`, and the preview's `as_of`. It starts one run under the
   saved policy with the preview's clock. One run at a time (`409 conflict`).

**A confirmation applies only what was previewed.** The token is a hash of the
previewed periods and the settings version they were previewed under
(`retention::preview_token`). The server recomputes it from what is stored at
the moment of the confirmation. If another Admin saved in between, if the
periods differ from the stored ones, or if the preview is in the future or
more than an hour old, the request is refused with `409
retention_preview_stale` and nothing starts or is saved. Saving the old
numbers again does not revive an old preview, because the version moved on.
The page clears a displayed preview whenever a period is edited and after
every save, only offers the run after a preview of the saved periods, and on a
stale refusal reloads the stored periods. The token is a binding, not a
secret: authorization is the Admin session and the typed word.

A run (`retention_runs`) snapshots the periods and its clock (`as_of`, the
preview's; never in the future, the database refuses that), so it means the
same thing after a restart. The preview and the run share their predicates: a
run removes what its preview counted, except what changed in between (for
example a ticket that was reopened), which is judged again when it is taken.

**Restart safety.** The retention worker (`retention::worker_loop`, in the API
process) takes the pending run with `FOR UPDATE SKIP LOCKED` and does one
batch per transaction; the removal routines add each removal to the run's
`counts` in that transaction. A crash or a failed batch rolls back the
removals and their counts together; the next poll continues. A second API
process skips the locked run, so nothing is counted or removed twice. The
routines are no-ops on anything already removed.

**Each ticket is judged again when it is taken.** The worker's candidate list
is only a plain read. The routine that removes something locks the ticket
first and then, in a new statement, checks against what is committed at that
moment that the ticket is still in a final status and that its latest move
into that status is older than the run's cutoff. A ticket that was reopened
and closed again between the scan and the lock is judged by its newest
closure and left alone.

**A run is completed only when nothing expired is left.** A ticket or draft
that another transaction holds is skipped, not waited for. Before completing,
the worker counts what is still expired without skipping locked rows; if
anything is left, the run stays pending, the retry counter goes up, and the
run is taken again after a backoff (5 seconds, doubling, at most 10 minutes).

**Bounds.** Per transaction: at most 100 tickets or drafts examined, and the
batch stops once 1,000 rows changed (ticket, message, event, and screenshot
rows). One ticket is never split, so a single ticket with more messages than
that still goes in one transaction. The audit period deletes 5,000 events per
transaction, oldest first, each with its own `audit_retention_purged` record.
Progress is durable after every transaction.

**Files.** Screenshot rows are deleted in the batch; their files are removed
right after the commit. If the process dies in between, or a backup holds the
file lock ([Backups](#backups)), the file has no row any more and the existing
stray-file sweep of `screenshots::cleanup` deletes it later (files older than
24 hours without a row). A row never points at a missing file because of
retention, and a live ticket's file is never a candidate: the database refuses
to delete a screenshot row unless its ticket is in a final status.

**Automatic.** With "apply automatically" on, the worker starts one run per
day under the saved policy (actor: the `retention` service).

**Audit.** The database itself writes `retention_run_started` (who, the
periods, the clock) when a run row is inserted and `retention_run_completed`
(the periods, who requested it, and the counts per category) when it
completes, so no run exists without its record. Plus `audit_retention_purged`
and `audit_ticket_text_redacted` from the audit paths. Counts and settings
only: never ticket text, file names, or account names.

**Observability.** The worker reports as `worker="retention"` in
`tikkit_worker_failures_total`, `tikkit_worker_retries_total` (a batch that
failed and will be taken again, or a run waiting for locked rows),
`_last_success_timestamp_seconds`, and the backlog gauges (a pending run is a
pending and due job; lag is its age). It runs under `telemetry::job` with the
request ID of the Admin request that started the run, so its log lines and
audit events correlate with it. See [operations.md](operations.md#data-retention).

## How the immutability rules are kept

Submitted intake, AI-solved records, messages, ticket events, attached
screenshots, and AI attempts are immutable for every normal action. Migration
`0028` does not disable those triggers. It adds three SQL routines:

| Routine | May |
| --- | --- |
| `retention_delete_screenshots(ticket, run, account)` | delete the screenshot rows of the ticket |
| `retention_purge_ai(ticket, run, account)` | blank `input`, `explanation`, `steps` of the ticket's finished attempt |
| `retention_purge_ticket(ticket, run, account)` | set subject, description, and the snapshot's text to the marker, message bodies to the marker, the free text of `cancel` and `propose_resolution` events to the marker, and (through `audit_redact_ticket_text`) the cancellation reason in the audit log |

**Two things must hold for a row to change.**

1. **Authorization, inside the routine** (`retention_ticket_due`), under the
   ticket's row lock and against data read after the lock was taken. Exactly
   one of `run` and `account` is given:
   - `run`: a retention run that exists, is `pending`, has a period for this
     category in its snapshot, and for whose clock this ticket has expired
     now. The snapshot of a run cannot be changed, its clock cannot be in the
     future, a completed run authorizes nothing, and runs cannot be deleted
     (`retention_runs_guard`). The removal is added to the run's counts, and
     the run was audited when it was inserted.
   - `account`: the ticket's requester, already anonymized. Each removal
     writes its own audit event, `retention_account_content_removed` (the
     account, the ticket ID, the category, the number of rows).

   Without one of the two, with both, with a run that is unknown, completed,
   or has no period for the category, or with an account that is not
   anonymized or did not submit the ticket, the routine raises `42501` and
   nothing changes. A ticket that is not in a final status, has not expired
   for the run, or is held by another transaction is left untouched (the
   routine returns nothing, `false`, or `0`). So calling
   `retention_purge_ticket` by hand on a ticket closed a minute ago, with
   every period unset, does nothing but fail: there is no destructive routine
   without one of these two authorizations, and none without an audit record.
2. **The guards**, by the same mechanism as the audit log's exceptions
   ([audit.md](audit.md#append-only-retention-and-anonymization)): each
   trigger recognises the one statement inside its routine by the call stack
   and then checks the row itself: only a final-status ticket, only those
   columns, only to the marker. The same statement sent directly, from a `DO`
   block, or from a function of another name is refused (`23001`). No event
   row is ever deleted and no status, team, assignee, or timestamp can change
   this way.

**What this does not protect against.** The same limit as stated for the audit
log: the application's database role owns the tables, the triggers, and these
routines. Someone who can run DDL as that role, or a superuser, can replace a
routine or disable a trigger. The authorization also rests on tables that role
can write: plain SQL as that role can insert a retention run (it is audited by
trigger, and it authorizes only what its periods and a non-future clock cover)
or mark an account as anonymized. Running the application under a role that
does not own the schema would close the DDL gap; it is optional hardening and
not part of this feature. PostgreSQL 17 is pinned; a change to the `PG_CONTEXT`
format in a later major version would make the guards refuse the routines
(fail closed), which `apps/api/tests/retention.rs` would catch.

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
| `username`, `display_name` | `anonymized-<account id without dashes>` (see the reserved namespace below) |
| `email` | empty |
| `password_hash` | unusable |
| Specialist `expertise`, notification preferences | cleared / off |
| Sessions, password-reset tokens | deleted |
| Unsubmitted drafts, unattached screenshot uploads (rows and files) | deleted |
| Saved views, in-app notifications, queued notifications | deleted |
| The email address its invitation was sent to | cleared |
| Audit log | `audit::anonymize_account`: the username snapshots of this account become `null`, the `ip` key is removed from its events, and the Specialist expertise text (`from_expertise`, `to_expertise`) is removed from the account updates about it |

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
  are in a final status is removed in the same transaction, exactly as a run
  would (text, messages, proposal summaries, cancellation reason also in the
  audit log, screenshots, AI content), authorized by the anonymized account
  and audited per removal. If one of those tickets is being changed at that
  moment the whole request is `409 conflict` and nothing is committed. Open
  tickets stay open with their content so the help desk can finish them; they
  follow the ticket content period afterwards.

**What remains after anonymization:** the account ID with its role, team, and
times; the IDs on tickets, messages, events, audit events, and usage rows; and,
unless `purge_ticket_content` was set or until the ticket content period
applies, the text of the tickets the account submitted and of messages it
wrote on other people's tickets (those belong to their tickets and go with
them). Nothing else that names or addresses the person is kept: no username,
display name, email address, IP address, session, expertise text, draft, or
unattached upload.

**No request can write the data back.** Anonymization locks the account row
(`FOR UPDATE`) before it deletes anything. Every transaction that stores
personal data for an account (profile, email, password, password reset,
session, notification preferences, draft, upload, saved view) first takes a
share lock on that row and is refused unless the account is active
(`auth::lock_active_account`; ticket and message writes use the same lock in
`lifecycle::lock_actor`). A request that was authenticated while the account
was still active therefore either commits before the erasure looks, which then
removes what it wrote, or waits and gets `401`. A new writer of personal data
must take this lock.

**The placeholder namespace is reserved.** No account can be created with a
username starting with `anonymized-` (any casing): setup and invitation
acceptance answer `422 username: reserved`, and the database refuses it too
(`accounts_username_reserved`). The constraint is `NOT VALID`, so an
installation where somebody already had such a name upgrades without failing;
anonymization then takes the first free name, appending `-1`, `-2`, and so on,
so a collision can never block an erasure.

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
that were cut. The whole export holds at most 16 MiB of JSON
(`personal_data::EXPORT_MAX_BYTES`): its size is measured in the database
before anything is loaded, and a larger one is refused with `413
export_too_large` and no audit event. For such an account, read the data from
a restored backup or directly from the database.

## Admin API

All need the Admin role; every other role gets `403 forbidden` with no data,
signed out is `401`. Mutations need the CSRF header.

| Endpoint | |
| --- | --- |
| `GET /api/admin/retention` | Policy, bounds, and the latest 10 runs with their counts |
| `PUT /api/admin/retention` | Replace the policy. All five periods required (`null` = keep). `422`: `out_of_range`, `confirm: invalid`; `409 retention_preview_stale` with automatic deletion on and no current preview of these periods |
| `POST /api/admin/retention/preview` | Dry run for the periods in the body: counts, `as_of`, `token` |
| `POST /api/admin/retention/runs` | Start a run from a preview (`202`): `confirm`, `preview_token`, `as_of`. `422 confirm: invalid`, `422 policy: not_configured`, `409 retention_preview_stale`, `409 conflict` while one is pending |
| `GET /api/admin/personal-data?username=` | Counts of what is stored about one account; `404` if no account has that username |
| `POST /api/admin/accounts/{id}/personal-data/export` | The export. `413 export_too_large` |
| `POST /api/admin/accounts/{id}/anonymize` | Anonymize. `422`: `username: mismatch`, `account: active`, `account: already_anonymized`; `409 conflict` if one of its tickets is being changed |

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
- **Database and screenshot volume go together, and the backup must hold the
  file lock.** A backup dumps the database and then copies the screenshot
  volume. If the API deletes a file between the two steps (a retention run, an
  expired upload, a deleted pending upload, an anonymization), the dump still
  has the row and the copy lacks the file: after a restore that screenshot's
  preview fails. The backup prevents this with the contract below; the
  procedure that uses it is in [backup-restore.md](backup-restore.md).

  **Contract.** While any database session holds

  ```sql
  SELECT pg_advisory_lock(screenshot_files_lock_key());
  ```

  the API deletes no file from `SCREENSHOT_DIR`. Take it before the dump,
  keep that session open until the volume is copied, then
  `SELECT pg_advisory_unlock(screenshot_files_lock_key());` (closing the
  session releases it too). `screenshot_files_lock_key()` is a constant
  `bigint` defined in migration `0028`; use the function, not the number.
  - Every file deletion of the API goes through `screenshots::deleting_files`,
    which takes the same lock in shared mode
    (`pg_try_advisory_xact_lock_shared`) for the duration of the deletion and
    does nothing if it cannot. `pg_advisory_lock` therefore also waits until
    deletions already in flight are finished.
  - Only file deletion is deferred. Retention runs, anonymization, uploads,
    and every other database change proceed and commit while the lock is
    held; rows deleted meanwhile are simply not in a dump taken afterwards,
    and their files are in the copy as harmless extras.
  - Deferred files have no row. The hourly stray-file sweep, which honours
    the lock as well, removes them once the lock is released.
  - New uploads still write files during a backup. A file newer than the dump
    has no row in it and is an extra.
- Restoring a dump with a newer directory than the dump leaves files without
  rows, which the stray-file sweep removes. Restoring an older directory under
  a newer dump loses files of rows that still exist
  ([screenshots.md](screenshots.md#storage-backup-and-restore)); take both in
  one backup, under the lock.
- Retained data restores as itself: nothing retention keeps (ticket skeletons,
  events, counts) depends on anything it removes.
- Backups should be encrypted and access-controlled like the live data: they
  are the place where "deleted" data lives longest.

## Tests

`apps/api/tests/retention.rs`: the defaults delete nothing; expiry just
before, at, and after the boundary for tickets and drafts, and a clock that
cannot be in the future; dry-run counts equal the deletions for
operator-handled, cancelled, and AI-solved tickets together, with no typed
text left in tickets, messages, events, the audit mirror, or AI attempts;
reports identical before and after retention and after anonymization; a crash
mid-batch and two concurrent workers; a crash between row and file deletion;
a ticket closed again between the scan and the lock; a candidate held by
another transaction when the run would complete, with the retry counter; the
row budget and the batched audit purge; the backup file lock deferring file
deletion but not database work; the routines refusing a call without a run,
with a run that has no period for the category, with a completed run, and for
a ticket that has not expired; the immutability triggers still refusing
everything else; the 30-day audit floor; settings validation, confirmation
bound to the preview, the stale tab with two Admins, and audit events; the
daily automatic run and its metrics; role denial for every non-Admin role;
export contents, exclusions, and the byte budget; anonymization with the
shared-email account, the last active Admin, reactivation, and the audited
account path; a request authenticated before the erasure, in both orders; the
reserved username namespace and a pre-existing collision; detail views after
screenshot-only and text retention. `apps/api/tests/audit.rs` covers the
expertise redaction.
`apps/web/src/lib/retention.test.ts` and `apps/web/e2e/admin-retention.spec.ts`
cover the page.
