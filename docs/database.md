# Database

Tikkit stores all service-desk data in **PostgreSQL**. There is one supported
database engine and one migration path; no MariaDB/MySQL/SQLite layer exists.

## Supported version

**PostgreSQL 16** is the supported and tested server version
(`postgres:16` in `compose.dev.yml`; CI and the production Compose stack must
use the same major version). Other major versions are untested. The schema uses only standard PostgreSQL
features (`gen_random_uuid()`, identity columns, partial unique indexes,
`jsonb`, PL/pgSQL triggers, deferred constraint triggers).

## Local development

```sh
docker compose -f compose.dev.yml up -d   # PostgreSQL 16 on 127.0.0.1:5432
bun run dev                               # API migrates the database on start
```

With `TIKKIT_ENV=development` (the default) and `DATABASE_URL` unset, the API
uses `postgres://tikkit:tikkit@127.0.0.1:5432/tikkit`, which matches the
Compose file. Production never falls back to a default. If you already run a
PostgreSQL 16 server, create a role and database and point `DATABASE_URL` at it:

```sh
createuser --pwprompt tikkit
createdb --owner tikkit tikkit
export DATABASE_URL=postgres://tikkit:<password>@127.0.0.1:5432/tikkit
```

## Provisioning and migrating

Migrations live in `apps/api/migrations/NNNN_description.sql`, are embedded
into the `tikkit-api` binary at compile time, and are recorded in the
`_sqlx_migrations` table. A fresh database is provisioned by migrating it:

```sh
tikkit-api migrate                 # production binary: apply pending migrations and exit
cargo run --locked -- migrate      # from a checkout (development)
```

On `tikkit-api` (serve) startup the API:

1. connects, retrying transient failures (refused/timed-out connections) with
   exponential backoff for up to `DATABASE_STARTUP_TIMEOUT_SECS` (default 30s);
   server-reported errors such as a wrong password or missing database fail
   immediately. Messages never include the password;
2. compares the applied migrations with the ones built into the binary and
   **refuses to start** if the database contains migrations this binary does
   not know (it was migrated by a newer release), if an applied migration's
   checksum differs (a released migration was edited), or if a migration
   previously failed part-way;
3. applies pending migrations when `DATABASE_AUTO_MIGRATE=true` (default), or
   exits with instructions to run `tikkit-api migrate` when it is `false`.

Migrations run inside a transaction each and take a PostgreSQL advisory lock,
so several API instances starting at once are safe. `GET /api/health/ready`
performs the same compatibility check and returns 503 while migrations are
pending or the schema is incompatible.

Downgrades are not supported: there are no down migrations. Roll back by
restoring the backup taken before the upgrade, together with the matching
screenshot volume.

## Adding a migration

1. Create the next numbered file, e.g. `apps/api/migrations/0002_sessions.sql`
   (four-digit version, snake_case description). Never edit or renumber a
   migration that has been merged; the checksum check will refuse to start
   against databases that applied the original.
2. Write forward-only SQL that preserves existing ticket records (add nullable
   columns or defaults, backfill, then tighten constraints).
3. `cargo test --locked -p tikkit-api` applies all migrations to fresh
   databases; add tests for any new constraint.

## Schema conventions

- **Identifiers:** UUID primary keys (UUID v7 generated in Rust;
  `gen_random_uuid()` is only a default). Account and ticket IDs are
  immutable (enforced by triggers). Tickets also have `reference`, a
  `bigint GENERATED ALWAYS AS IDENTITY` human-facing number that never changes.
- **Time:** every timestamp is `timestamptz`, read and written as UTC.
- **Enumerations:** `text` columns with `CHECK` constraints instead of
  PostgreSQL `ENUM` types. Values are the stable snake_case identifiers used
  by the API. Adding or retiring a value is a normal transactional
  `ALTER TABLE ... DROP CONSTRAINT / ADD CONSTRAINT` in a migration, whereas
  `ENUM` values cannot be removed.
- **Errors:** invariant triggers raise SQLSTATE `23514` (check violation) or
  `23001` (restrict violation) with a named constraint, so the API maps them
  to stable error codes the same way as ordinary constraints.

## Data model (migration 0001)

| Table | Purpose |
| --- | --- |
| `organization_settings` | Singleton row: first-setup record, default language, requester AI skip toggle (off), reopen toggle (off), cancellation policy (`anytime_while_open`), confirmation timeout (168 h). Bootstrap can be recorded only once. |
| `accounts` | Immutable ID, display `username`, unique `username_normalized`, **non-unique** `email`, role (`user`, `operator`, `specialist`, `team_lead`, `admin`), optional specialization, language (`en`/`de`), password hash, status. |
| `teams` | Name unique among active teams, `active`/`archived`. |
| `team_memberships` | Membership history; `ended_at IS NULL` marks the current row; `is_lead` marks leadership. |
| `categories`, `priorities` | Admin-managed, ordered, archived instead of deleted. `categories.skip_ai` = "Skip AI automatically". |
| `tickets` | Requester, intake fields, frozen `intake_snapshot` (jsonb), requested and current priority, status, resolution kind, team (NULL = general queue) and assignee. |
| `ticket_messages` | Public replies and internal notes. Append-only. |
| `attachments` | Screenshot metadata (PNG/JPEG/WebP only); bytes live in the private screenshot volume under `storage_key`. |
| `ticket_events` | Immutable state-transition and ticket history with actor provenance. Append-only. |
| `audit_events` | Attributable audit trail (actor, action, target, metadata, request ID). Append-only. |

Sessions, invitations, conditional fields, AI attempts and notification
outboxes are added by later migrations with their features.

### Ticket states and AI-solved records

`tickets.status` is exactly one of `draft`, `ai_processing`,
`awaiting_ai_decision`, `queued`, `in_progress`, `waiting_for_requester`,
`awaiting_confirmation`, `closed`, `cancelled`.

How a ticket was closed is recorded separately in `resolution_kind`, which is
set if and only if `status = 'closed'`:

- `ai_confirmed` – the requester explicitly chose "Solved" for the AI solution
  (an **AI-solved** record);
- `requester_confirmed` – the requester confirmed a staff resolution proposal;
- `auto_closed_timeout` – the confirmation timeout elapsed.

AI-solved tickets can never have a team or assignee, reject every later
`UPDATE`, and accept no messages or screenshots. Cancellation is a status, not
a resolution. Reopening a human-closed ticket clears `resolution_kind` and
`closed_at`; the history stays in `ticket_events`.

### Invariants enforced by the database

In addition to Rust validation, the schema rejects (also under concurrency):

- duplicate normalized usernames (emails may repeat across accounts, e.g. an
  Admin and an operational account of the same person);
- more than one current team per account and more than one current lead per
  team (partial unique indexes);
- memberships for Admin, deactivated, or archived-team cases; a lead whose
  role is not `team_lead`; promoting a current member to Admin; demoting or
  deactivating a current lead (row locks serialize these checks with
  concurrent membership changes);
- an active team without exactly one current lead, or an archived team with
  current members, **checked at commit** (deferred constraint trigger). Create
  a team and its lead in one transaction; replace a lead by ending the old
  lead row and inserting the new one in one transaction;
- rewriting or deleting membership history (rows may only be ended);
- assignees who are not current members of the ticket's team, and assigning
  a ticket to its own requester;
- changes to submitted intake (`subject`, `description`, requested priority,
  `intake_snapshot`, `submitted_at`) and to ticket identity/requester;
- `UPDATE`, `DELETE`, or `TRUNCATE` on `ticket_events`, `audit_events`, and
  `ticket_messages`;
- messages on draft, closed, or cancelled tickets and screenshots on closed or
  cancelled tickets.

## Library choice

The API uses [**sqlx 0.8**](https://github.com/launchbadge/sqlx) (async,
Tokio, rustls, PostgreSQL only) because it is a thin layer over SQL with a
connection pool and an embedded, checksummed migrator, and adds no ORM or
code-generation step. Queries use the runtime-checked `sqlx::query`/
`query_as` APIs with `FromRow`, so builds never need a live database.
Diesel (heavier ORM, sync core) and SeaORM (ORM on top of sqlx) were not
needed for a schema this explicit.

## Tests

API integration tests (`apps/api/tests/`) create a uniquely named database
(`tikkit_test_<uuid>`) per test on the server named by `TEST_DATABASE_URL`
(default `postgres://tikkit:tikkit@127.0.0.1:5432/postgres`; the role needs
`CREATEDB`), apply all migrations, and drop it `WITH (FORCE)` afterwards, even
when the test panics. Tests fail with instructions when the server is not
reachable. A killed test run can leave databases behind; list them with
`SELECT datname FROM pg_database WHERE datname LIKE 'tikkit_test_%'`.
