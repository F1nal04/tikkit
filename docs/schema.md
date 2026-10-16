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

## Invariants enforced by the database

Rust validates and authorizes first; these constraints are the backstop.

- `accounts.username_normalized` (`lower(btrim(username))`) is unique; `email`
  is not. Account IDs cannot be updated.
- `team_memberships.account_id` is the primary key: one team per account.
- `teams.lead_account_id` is unique (one team per lead) and a deferred foreign
  key to `team_memberships (account_id, team_id)`, so the lead is a member of
  that same team. An active team (`archived_at IS NULL`) must have a lead.
- Triggers reject admins as members or leads, leads without the `team_lead`
  role, promoting a member to admin, and demoting a current lead. Role checks
  take row locks so concurrent role changes and appointments serialise.
- Team creation is atomic: insert the team (with its lead) and the lead's
  membership in one transaction; the deferred key is checked at commit.
- Ticket assignees are members of the ticket's team (deferred composite key)
  and never the requester.
- `ticket_events` and `team_membership_history` are append-only (updates and
  deletes raise `restrict_violation`). Membership history is written by trigger.
