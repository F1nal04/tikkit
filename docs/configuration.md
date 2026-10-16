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
| `TRUST_PROXY_HEADERS` | no | `false` | `true` only behind a reverse proxy that sets `X-Forwarded-For`; the last hop is used as client IP for rate limits. |
| `SHUTDOWN_TIMEOUT_SECS` | no | `20` | Drain window (1-600) after SIGTERM/Ctrl+C before remaining requests are aborted. |
| `RUST_LOG` | no | `info,sqlx=warn` | Log filter, e.g. `info,tower_http=debug`. Request logs include the request ID and path, never query strings or bodies. |
| `TEST_DATABASE_URL` | tests only | `DATABASE_URL` | Maintenance connection used by integration tests to create/drop throwaway databases. |

AI providers and SMTP are not environment configuration. They are optional,
disabled by default, and configured by an Admin at runtime; the API starts and
serves the full service desk without them.

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
