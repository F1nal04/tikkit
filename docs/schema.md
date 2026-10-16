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
- `ticket_messages.kind`: `public_reply`, `internal_note`
- `attachments.mime_type`: `image/png`, `image/jpeg`, `image/webp`
- `team_membership_history.action`: `joined`, `left`
- `audit_events.event_type`: `setup_completed`, `login_succeeded`,
  `login_failed`, `logout`, `password_changed`, `session_revoked`,
  `invitation_created`, `invitation_revoked`, `invitation_accepted`,
  `recovery_requested`, `recovery_completed`, `profile_updated`,
  `account_deactivated`, `account_reactivated`
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
- `ticket_events`, `team_membership_history`, and `audit_events` are append-only: UPDATE,
  DELETE and TRUNCATE raise `restrict_violation`. Membership history is written
  by trigger on join, leave, and team change.
