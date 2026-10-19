# Configuration

The API reads environment variables at startup. Unless `ENVIRONMENT=production`
it also loads a git-ignored `.env` (see `.env.example`) from the working
directory or a parent, without overriding variables already set; production
reads only the real environment. It validates everything before
connecting, prints **all** problems at once, and exits non-zero. Error messages
name the variable and the fix but never echo values.

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `DATABASE_URL` | yes | | `postgres://user:password@host:5432/db`. PostgreSQL 17. |
| `PUBLIC_ORIGIN` | yes | | Origin users open, e.g. `https://help.example.com` (no path). Used for links and cookies. Must be `https://` in production. |
| `SESSION_SECRET` | yes | | At least 32 random bytes, base64 or hex. `openssl rand -base64 32`. |
| `ENCRYPTION_KEY` | yes | | Exactly 32 random bytes, base64 or hex. Encrypts stored AI/SMTP credentials. Back it up separately from database backups; losing it makes those credentials unreadable. |
| `API_ADDR` | no | `127.0.0.1:3001` | Listen address. Use `0.0.0.0:3001` in a container. |
| `ENVIRONMENT` | no | `development` | `development` or `production` (production enforces https origin and secure cookies). |
| `SESSION_TTL_HOURS` | no | `336` (14 days) | Absolute session lifetime (1-8760). Sign in again afterwards. |
| `SESSION_ROTATE_HOURS` | no | `24` | Session age after which its token is replaced on the next request (1-8760). |
| `TRUST_PROXY_HEADERS` | no | `false` | `true` only behind a reverse proxy that overwrites `X-Forwarded-For` (the production compose sets it for Caddy); the last hop is used as client IP for rate limits. Otherwise every client shares the proxy's IP. |
| `SHUTDOWN_TIMEOUT_SECS` | no | `20` | Drain window (1-600) after SIGTERM/Ctrl+C before remaining requests are aborted. |
| `SMTP_URL` | no | | Outbound mail for invitation and recovery links, e.g. `smtps://user:pass@mail.example.com` or `smtp://user:pass@mail.example.com:587?tls=required` (STARTTLS). Production requires one of those two TLS forms. Treated as a secret: never logged or echoed. Set together with `SMTP_FROM`. |
| `SMTP_FROM` | with `SMTP_URL` | | Sender, e.g. `Tikkit <help@example.com>`. |
| `RUST_LOG` | no | `info,sqlx=warn` | Log filter, e.g. `info,tower_http=debug`. Request logs include the request ID and path, never query strings or bodies. |
| `TEST_DATABASE_URL` | tests only | `DATABASE_URL` | Maintenance connection used by integration tests to create/drop throwaway databases. |

AI providers are not environment configuration: they are optional, disabled by
default, and configured by an Admin at runtime. SMTP is optional too; until
Admin-managed SMTP settings arrive with notifications it is read from
`SMTP_URL`/`SMTP_FROM`. Without it, Admins copy invitation links and recovery
emails are not sent (see [auth.md](auth.md#email-delivery)). The API starts and
serves the full service desk without either.

## Probes and shutdown

- `GET /api/health`: liveness, always `200 {"status":"ok"}` without touching dependencies.
- `GET /api/ready`: readiness, `200 {"status":"ready",...}` when PostgreSQL answers and
  its applied migrations match this build; otherwise `503` with
  `{"status":"unavailable","checks":{"database":"ok|unavailable","migrations":"ok|pending|incompatible|unknown"}}`.
- On SIGTERM or Ctrl+C the API stops accepting connections, drains in-flight
  requests for up to `SHUTDOWN_TIMEOUT_SECS`, closes the database pool (at most
  5 more seconds), and exits.

## Production

`docker-compose.prod.example.yml` sketches the deployment: only the reverse
proxy publishes ports; the API and PostgreSQL sit on an internal network. It
refuses to start unless the required secrets are supplied, e.g. in a `.env`
next to it (never commit it):

```sh
POSTGRES_PASSWORD=$(openssl rand -base64 24 | tr -d '/+=')
SESSION_SECRET=$(openssl rand -base64 32)
ENCRYPTION_KEY=$(openssl rand -base64 32)
PUBLIC_HOST=help.example.com
```

Packaged images and full production documentation are tracked separately.

## Organization settings

Admins configure these at `/admin/settings`; they are persisted in the singleton
organization row and read with `GET /api/organization` by signed-in clients.
No new environment variables are required. The Admin-only PUT endpoint is
`/api/admin/settings`; every successful change is audited.

- `name`: trimmed, nonblank, at most 120 characters; default `Tikkit`.
- `timezone`: standard IANA area/timezone name such as `Europe/Berlin`; default `UTC`.
  Invitation history uses it for display. All persisted timestamps remain UTC.
- `default_language`: `en` (default) or `de`; used for invitations without an
  explicit language. Existing account preferences are unchanged.
- `allow_reopening`: false by default, for human-closed tickets only. AI-solved
  records remain permanently final regardless of this value.
- `requester_cancellation_policy`: `anytime_while_open` (default),
  `before_work_starts`, or `disabled`. Every cancellation requires a reason;
  work starts with the first recorded human In Progress/work-start event and
  is not reset by unassignment or transfer.
- `resolution_confirmation_hours`: positive integer elapsed hours, default 168
  (7 days). Resolution proposals must snapshot the policy and deadline; changes
  affect subsequent proposals. Timeout closure is separate from requester
  confirmation and never implies acceptance. This is not an SLA setting.
- `allow_requester_ai_skip`: false by default; does not enable AI or change
  category exemptions.

This issue stores lifecycle policies for the ticket workflows that follow.
Enforcement, snapshotted proposals and timeout processing belong to their
respective ticket/lifecycle issues. Admin-managed SMTP is tracked in #28;
category/priority configuration is tracked in #15.
