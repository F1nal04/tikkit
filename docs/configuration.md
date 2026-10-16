# Configuration and operations

The API (`tikkit-api`) is configured only through environment variables.
Startup validates all of them, reports every problem at once, and exits with
code 2 without printing secret values. Check a configuration without starting
the server:

```sh
tikkit-api check-config     # prints a redacted summary or the list of problems
```

Examples: [`.env.example`](../.env.example) (development) and
[`deploy/compose.env.example`](../deploy/compose.env.example) (production).
Neither contains usable secrets.

## Variables

| Variable | Default | Notes |
| --- | --- | --- |
| `TIKKIT_ENV` | `development` | `development` or `production`. Production enables the stricter rules below and JSON logs. |
| `DATABASE_URL` | dev: `postgres://tikkit:tikkit@127.0.0.1:5432/tikkit` | **Required in production.** `postgres://` or `postgresql://` URL; percent-encode special characters. Placeholder passwords (`CHANGE_ME`) are rejected in production. Never logged with its password. |
| `DATABASE_MAX_CONNECTIONS` | `10` | Pool size, 1–100. |
| `DATABASE_AUTO_MIGRATE` | `true` | Apply pending migrations on start. With `false`, run `tikkit-api migrate` first. |
| `DATABASE_STARTUP_TIMEOUT_SECS` | `30` | How long startup retries an unreachable database (0–600). |
| `API_ADDR` | `127.0.0.1:3001` | Listen socket `IP:PORT`. Use `0.0.0.0:3001` inside a container. The Vite dev proxy expects the default. |
| `PUBLIC_ORIGIN` | dev: `http://localhost:3000` | **Required in production, https only.** Origin users open (no path). Used for CSRF origin checks and links. |
| `TIKKIT_ENCRYPTION_KEY` | – | **Required in production.** Base64 of exactly 32 random bytes (`openssl rand -base64 32`). Encrypts stored AI/SMTP credentials. Placeholders and low-entropy keys are rejected. Optional in development, validated if set. |
| `SHUTDOWN_TIMEOUT_SECS` | `20` | Drain time for in-flight requests after SIGTERM/Ctrl+C (1–300). |
| `TRUST_PROXY_HEADERS` | `false` | Trust `X-Forwarded-*` from the reverse proxy. Only enable when clients cannot reach the API directly. |
| `RUST_LOG` | `info,sqlx=warn` | [`tracing` filter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html). |
| `TEST_DATABASE_URL` | `postgres://tikkit:tikkit@127.0.0.1:5432/postgres` | Tests only: server where integration tests create disposable databases. |

Blank values count as unset. Booleans accept `true/false`, `1/0`, `yes/no`,
`on/off`. AI providers and SMTP are configured by Admins at runtime and need
no environment variables; the API starts with both disabled.

Secrets (`DATABASE_URL` password, `TIKKIT_ENCRYPTION_KEY`) are redacted from
`Debug` output, logs, errors, and `check-config`. Keep the encryption key in
a secret store separate from database backups.

## Commands

```sh
tikkit-api                # serve (default)
tikkit-api migrate        # apply pending migrations and exit
tikkit-api check-config   # validate configuration and exit
```

From a checkout use `cargo run --locked -- <command>`.

## Health probes

| Endpoint | Purpose | Response |
| --- | --- | --- |
| `GET /api/health` | Liveness (kept for compatibility) | `200 {"status":"ok"}` |
| `GET /api/health/live` | Liveness; never touches dependencies | `200 {"status":"ok"}` |
| `GET /api/health/ready` | Readiness: `SELECT 1` and migration compatibility, each bounded to 2s | `200 {"status":"ready","checks":{"database":"ok","schema":"ok"}}` or `503 {"status":"not_ready","checks":{...}}` |

Readiness check values: `database` is `ok` or `unavailable`; `schema` is
`ok`, `pending_migrations`, `incompatible` (database migrated by a newer
release or an edited migration), or `unknown` (database unavailable). Details
are logged, not returned. Optional AI and SMTP never affect readiness.

Route traffic only to ready instances; restart only on failed liveness.

## Shutdown

On SIGTERM or Ctrl+C the API stops accepting connections, lets in-flight
requests finish for up to `SHUTDOWN_TIMEOUT_SECS`, then drops what remains,
closes the database pool, and exits 0. Give the container a stop grace period
longer than this timeout (e.g. Compose `stop_grace_period: 30s`).

## Production deployment

The production Compose stack (#46) runs the reverse proxy, web, API, and
PostgreSQL 16 on a private network. Only the reverse proxy publishes ports;
the API (`API_ADDR=0.0.0.0:3001` inside its container) and the database stay
internal. Start from `deploy/compose.env.example`:

```sh
cp deploy/compose.env.example deploy/compose.env && chmod 600 deploy/compose.env
openssl rand -base64 32          # -> TIKKIT_ENCRYPTION_KEY
openssl rand -base64 24 | tr -d '/+='   # -> POSTGRES_PASSWORD (repeat in DATABASE_URL)
```

`deploy/compose.env` is ignored by Git. The API refuses to start while any
required value is missing or still `CHANGE_ME`.
