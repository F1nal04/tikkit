# Database schema

PostgreSQL **17** is the supported server (pinned as `postgres:17.6` in
`docker-compose.dev.yml`). Access is through `sqlx` with runtime-checked
queries; no `DATABASE_URL` is needed at compile time.

Migrations live in `apps/api/migrations/` as `NNNN_description.sql`, are
embedded in the API binary, and run automatically at startup (and in tests).
Never edit an applied migration; add a new file. The API refuses to start if the
database has a migration this build does not know (newer schema), a modified
migration (checksum mismatch), or a failed one (dirty state).

All timestamps are `timestamptz` and stored in UTC. Primary keys are UUIDs;
tickets also have an immutable `reference` number for people.

## Stable identifiers

These values are machine identifiers. They never change with the interface
language; translations map them to display text.

- `accounts.role`: `user`, `operator`, `specialist`, `team_lead`, `admin`
- `accounts.language`: `en`, `de`
- `tickets.status`: `draft`, `ai_processing`, `awaiting_ai_decision`, `queued`,
  `in_progress`, `waiting_for_requester`, `awaiting_confirmation`, `closed`,
  `cancelled`, `ai_solved`
- `ticket_events.event_type` for status transitions: `submit`,
  `ai_suggestion_ready`, `ai_failed`, `skip_ai`, `ai_solved`, `ai_not_solved`,
  `start_work`, `request_information`, `requester_responded`,
  `propose_resolution`, `accept_resolution`, `reject_resolution`,
  `confirmation_timeout`, `reopen`, `cancel` (see [lifecycle.md](lifecycle.md));
  queue entry `payload.route`: `ai`, `ai_disabled`, `category_exempt`,
  `requester_skip`, `ai_failed`, `not_solved`
- `tickets.resolution_source`: `ai`, `operator`; `tickets.closure_reason`:
  `requester_confirmed`, `confirmation_timeout`
- `ticket_messages.kind`: `public_reply`, `internal_note`
- `attachments.mime_type`: `image/png`, `image/jpeg`, `image/webp`
- `team_membership_history.action`: `joined`, `left`
- `audit_events.event_type`: `setup_completed`, `login_succeeded`,
  `login_failed`, `logout`, `password_changed`, `session_revoked`,
  `invitation_created`, `invitation_revoked`, `invitation_accepted`,
  `recovery_requested`, `recovery_completed`, `profile_updated`,
  `account_deactivated`, `account_reactivated`, `smtp_configured`,
  `smtp_updated`, `smtp_removed`, `smtp_tested`, `catalog_item_created`,
  `catalog_item_updated`, `catalog_reordered` (metadata `kind`: `category`,
  `priority`, `intake_field`)
- `intake_fields.kind`: `text`, `select`
- `invitations.role`: an `account_role`; `invitations.language`: `en`, `de`

## Invariants enforced by the database

Rust validates and authorizes first; these constraints are the backstop.

- `accounts.username_normalized` (`lower(normalize(btrim(username), NFKC))`)
  is unique, so `Alice`/`alice` and precomposed/decomposed `José` collide.
  Usernames may not contain whitespace or control characters (checked after
  NFKC folding). `email` is not unique. Account IDs cannot be updated.
- `team_memberships.account_id` is the primary key: one team per account.
- Role rules are declarative, so they hold under any isolation level: members
  and leads carry a copy of the account's role (and leads its `is_active`) via
  foreign keys to `accounts (id, role[, is_active])` with `ON UPDATE CASCADE`.
  A role or activation change cascades into those rows, where CHECKs reject it:
  `team_memberships.role NOT IN ('admin', 'user')`, `teams.lead_role = 'team_lead'`, and an
  active team's lead must be active. Foreign-key row locks make a concurrent
  role change and membership/appointment conflict instead of both committing.
  Insert memberships with the account's current role
  (`INSERT ... SELECT id, role, $team FROM accounts WHERE id = $account`).
- `teams.lead_account_id` is unique (one team per lead) and a deferred foreign
  key to `team_memberships (account_id, team_id)`, so the lead is a member of
  that same team. `lead_account_id`, `lead_role` and `lead_active` are NULL
  together (`MATCH FULL`), which only an archived team may do.
- So: admins and users can be neither members nor leads; members cannot be promoted to
  admin; a current lead cannot be demoted or deactivated. These surface as
  `23514`/`23503`; `ApiError` maps the lead checks (`teams_lead_role_check`,
  `teams_active_lead_active`) to `409 active_team_lead`.
- Team creation is atomic: insert the team (with its lead) and the lead's
  membership in one transaction; the deferred key is checked at commit.
- Ticket assignees are members of the ticket's team (deferred composite key)
  and never the requester.
- At least one active Admin remains: a trigger rejects demoting, deactivating,
  or deleting the last one (constraint `last_active_admin`, `23514`), serialized
  by an advisory lock so concurrent changes cannot both pass (see
  [authorization.md](authorization.md)). Do not lock Admin rows `FOR UPDATE`
  before changing `role`/`is_active`; let the statement take the advisory lock
  first, or concurrent changes can deadlock (`40P01`, mapped to `409 conflict`).
- Team members are staff: `team_memberships_role_staff` rejects `user` and
  `admin` roles, so demoting a member to User requires leaving the team first.
- `organization` has exactly one row (`id boolean PRIMARY KEY CHECK (id)`);
  `setup_completed_at` closes first-admin setup.
- `sessions.token_hash` (SHA-256 of the cookie token) is unique; tokens are never stored.
  The same holds for `invitations.token_hash` and `password_resets.token_hash`.
- An invitation is accepted (`accepted_at` and the created `account_id`, unique,
  set together) or revoked, never both.
- `smtp_connection` has at most one row (`id boolean PRIMARY KEY CHECK (id)`).
  `security` is `starttls` or `implicit_tls`. `username_ciphertext` and
  `password_ciphertext` are AES-256-GCM (12-byte nonce, then ciphertext and
  tag), never the plaintext credentials.
  `last_delivery_error` and `last_test_error` store a stable error code only.
- `smtp_deliveries.idempotency_key` records a successful send. A later delivery
  with the same key does not send again. The row holds no message body, address,
  or credential. The table is append-only.
- Catalog rows (`categories`, `priorities`, `intake_fields`,
  `intake_field_choices`) are archived (`archived_at`), never deleted, so
  tickets and snapshots keep resolving their IDs. Category and priority names
  are unique (case-insensitive, trimmed) among active rows only. Only `select`
  fields may set `allow_other`.
- Ticket lifecycle (`0008_ticket_lifecycle.sql`): `confirmation_deadline` is
  set exactly while `awaiting_confirmation`; `resolution_source` and
  `closure_reason` are set exactly on `closed` (`operator`) and `ai_solved`
  (`ai`, `requester_confirmed`); drafts, AI-stage, and AI-solved tickets have no
  team. A trigger rejects (`23001`) changing the requester, editing subject,
  description, or category or returning to `draft` after submission, and any
  status, team, or assignee change of an `ai_solved` or `cancelled` ticket.
  Status changes go through `lifecycle::transition` only.
- `ticket_events`, `team_membership_history`, and `audit_events` are append-only: UPDATE,
  DELETE and TRUNCATE raise `restrict_violation`. Membership history is written
  by trigger on join, leave, and team change.

## Admin management (0005)

Migration `0005_admin_management.sql` extends the existing singleton `organization`
with `name` (default `Tikkit`), `timezone` (default `UTC`), `default_language`
(`en`/`de`, default `en`), `allow_reopening` (false),
`requester_cancellation_policy` (`anytime_while_open` by default,
`before_work_starts`, or `disabled`), `resolution_confirmation_hours` (positive
integer, default 168), and `allow_requester_ai_skip` (false). There are no
credentials or AI enablement fields here. Timezone names are validated against
PostgreSQL's `pg_timezone_names` and limited to `UTC` or standard IANA area names; storage timestamps stay UTC.

`accounts.expertise` is nullable, bounded to 120 characters, descriptive Specialist
metadata. The management API clears it when changing to another role. Neither it
nor the Team Lead title grants additional team scope.

Admin account/team edits take the existing `tikkit.active_admins` transaction
advisory lock before row locks, then recheck the actor's current Admin status.
The existing FK/check constraints remain the final backstop. Replacing a lead is
one transaction; the former lead remains a member without leadership. A lead
cannot transfer, leave, deactivate or change role until replaced or archived.
Archiving clears leadership, all memberships and individual assignments;
restoring requires an eligible active Team Lead and appoints them atomically.
Leaving a team or deactivating an assignee clears affected assignments and appends
`ticket_events.assignment_changed` with the Admin actor, previous assignee and
reason. Tickets, messages, membership history and account IDs remain intact.

Additional attributable append-only audit event types are `account_updated`
(role/expertise before/after), `team_created`, `team_updated` (including leadership
and archival), `team_membership_changed` (old/new team IDs), and
`organization_settings_updated` (before/after policy). Invitation reissue appends
`invitation_revoked` with reason `reissued` and `invitation_created` atomically;
neither event contains the link or token.

## Ticket catalog

Admins manage categories (with the "Skip AI automatically" flag `skip_ai`,
default off), priority levels, and conditional intake fields
(`apps/api/src/catalog.rs`, migration `0007_ticket_catalog.sql`). Every
category uses the one shared base form; a field is shown while one of its
categories (`intake_field_categories`) is selected. `select` fields have
choices with stable IDs and optionally a free-text alternative (`allow_other`).
This is deliberately not a form builder: no nesting, no field-to-field rules,
and a category never selects a team.

`GET /api/catalog` serves the active catalog to the ticket form;
`GET /api/catalog/all` includes archived rows for Admins.
`catalog::validate_intake` is what ticket submission must call: it rejects
unknown/archived category, priority, and choice IDs and unknown field IDs,
enforces required visible fields, drops answers to hidden or archived fields,
and returns an `IntakeSnapshot` (names, labels, `skip_ai`, answers) for the
ticket to store, so later catalog edits never rewrite submitted tickets.

## Drafts and immutable submission

Migration `0009_ticket_submission.sql` adds:

- `ticket_drafts`: private incomplete JSON input, owner account ID, monotonic
  version, timestamps, and a unique optional `submitted_ticket_id`. Input is
  present exactly while unsubmitted; successful submission replaces it with a
  tombstone. An owner index supports bounded pagination. Drafts are retained
  until explicit owner deletion or submission, with no automatic expiry.
- `tickets.intake_snapshot`: immutable original subject/description, catalog
  `IntakeSnapshot` (including original priority and visible answers), and
  screenshot IDs (attached rows in `screenshots`). Nullable for pre-existing records;
  the new submission service always sets it. `tickets_intake_snapshot_immutable`
  rejects updates after submission (`23001`). Effective `priority_id` remains
  separate so staff priority changes cannot rewrite the original selection.
- `ticket_submission_keys`: append-only receipts keyed by
  `(requester_account_id, idempotency_key)`, SHA-256 request fingerprint, unique
  ticket ID, and original submitted status. Retained for the ticket lifetime;
  UPDATE/DELETE/TRUNCATE are rejected. Atomic with the ticket and transition.
- Audit events `draft_created`, `draft_updated`, `draft_deleted` contain only
  draft ID/version, attributable to the owning account. Submission uses the
  existing immutable `submit` transition event, not a second routing event.

See [submission.md](submission.md) for endpoints, concurrency, retention and
backup deletion limits. No new configuration variables are needed.

## Screenshots

Migration `0010_screenshots.sql` adds `screenshots`: metadata of validated,
re-encoded images whose bytes are `SCREENSHOT_DIR/<id>`. Uploader account ID,
optional `ticket_id` (NULL while pending, set once by `screenshots::attach`
together with `attached_at`), normalized filename, stored `content_type`
(`image/png`, `image/jpeg`, `image/webp`), stored size and dimensions,
`created_at`, and `touched_at` (upload or last draft save, the orphan-expiry
clock). `screenshots_frozen` rejects any update or delete of an attached row
and any change of a pending row's content columns (`23001`). Partial indexes
cover ticket lookup and per-account pending uploads. Audit events
`screenshot_uploaded`, `screenshot_deleted`, `screenshots_expired` carry IDs,
type, and size only. See [screenshots.md](screenshots.md).

## Operator queues

Migration `0011_operator_queues.sql` adds the partial index
`tickets_open_work_idx (created_at, id)` over tickets in an open human-work status
(`queued`, `in_progress`, `waiting_for_requester`, `awaiting_confirmation`). No
new columns: queue membership is derived from `status`, `team_id`, and
`assignee_account_id` (see [queues.md](queues.md)).

## Conversations

Migration `0012_ticket_messages.sql` extends the existing `ticket_messages`
table (it does not duplicate it): immutable identity `sequence` for stable
per-ticket commit ordering, server-derived `author_kind` (`requester`,
`staff`), optional UUID `idempotency_key` and SHA-256 `request_hash` (null only
for legacy rows). `(ticket_id, author_account_id, idempotency_key)` is unique.
Bodies are nonempty plain text, at most 10,000 characters; internal notes have
staff authors. UPDATE/DELETE/TRUNCATE are rejected (`23001`). Full and
public-only `(ticket_id, sequence)` indexes support bounded ascending pages.

`screenshots.message_id` is null for intake/pending uploads, otherwise bound
once during attachment with a composite `(message_id, ticket_id)` foreign key
to `ticket_messages`. The existing screenshot freeze trigger protects this
association. Note screenshots require staff-only message access on every read.

Send events `public_reply_added` and `internal_note_added` in `ticket_events`
record the actor and `{message_id}` only, without content or screenshot IDs;
they have no status columns. Requester transition history never includes
these events. See [conversations.md](conversations.md) for endpoints,
authorization, rendering, ordering, retries, and notification integration.
