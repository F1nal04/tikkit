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
| `PUBLIC_ORIGIN` | yes | | Origin users open, e.g. `https://help.example.com` (no path). Used for links in mail, cookies, HSTS, and as the CSRF anchor: a browser request that changes something is refused (`403 csrf_rejected`) unless its `Origin` is exactly this value, so the portal must be opened under this address (in development `http://localhost:3000`, not `127.0.0.1` or a LAN address). Must be `https://` in production. |
| `SESSION_SECRET` | yes | | At least 32 random bytes, base64 or hex. `openssl rand -base64 32`. |
| `ENCRYPTION_KEY` | yes | | Exactly 32 random bytes, base64 or hex. Encrypts stored AI/SMTP credentials. Back it up separately from database backups; losing it makes those credentials unreadable. |
| `API_ADDR` | no | `127.0.0.1:3001` | Listen address. Use `0.0.0.0:3001` in a container. |
| `ENVIRONMENT` | no | `development` | `development` or `production` (production enforces https origin and secure cookies). |
| `SESSION_TTL_HOURS` | no | `336` (14 days) | Absolute session lifetime (1-8760). Sign in again afterwards. |
| `SESSION_ROTATE_HOURS` | no | `24` | Session age after which its token is replaced on the next request (1-8760). |
| `TRUSTED_PROXIES` | no | empty | Comma-separated IP addresses or CIDR networks of the peers allowed to supply `X-Forwarded-For`: the public reverse proxy and the web (SSR) server, e.g. `172.30.0.0/24`. The client IP for rate limits, sessions, and audit events is the socket peer, or, when that peer is listed, the rightmost `X-Forwarded-For` entry that is not itself listed (entries with a port such as `1.2.3.4:5678` or `[::1]:5678` are read without it). Networks wider than IPv4 /8 or IPv6 /16 are refused; IPv4-mapped networks (`::ffff:10.0.0.0/104`) mean the IPv4 network. Empty: forwarding headers are ignored and every client behind a proxy shares its IP. Replaces `TRUST_PROXY_HEADERS` (see [Upgrading](#upgrading)). See [security.md](security.md#reverse-proxy-trust). |
| `SHUTDOWN_TIMEOUT_SECS` | no | `20` | Drain window (1-600) after SIGTERM/Ctrl+C before remaining requests are aborted. |
| `SCREENSHOT_DIR` | in production | `data/screenshots` | Private directory for validated screenshot files, created at startup. Production requires an absolute path on a persistent volume (the production compose uses `/var/lib/tikkit/screenshots`). Only the API reads it; never serve it. Back it up with the database ([screenshots.md](screenshots.md#storage-backup-and-restore)). |
| `LOG_FORMAT` | no | `json` | API structured logging. Set `text` for human-readable output. The web server logs JSON. |
| `RUST_LOG` | no | `info` | API log filter, e.g. `info,tikkit_api::ai_attempts=debug`. Only Tikkit targets are emitted. |
| `METRICS_PORT` | no | disabled | Optional port (1–65535) for `GET /metrics` on the API process's IPv4 loopback. Use a dedicated port, e.g. `9091`. |
| `RESTORE_MODE` | no | `false` | `true` only on a restored copy under verification ([backup and restore](backup-restore.md#isolated-restore)). The API serves requests but starts no background job (notification outbox, AI attempts, confirmation timeouts, file sweeps, AI usage retention, the data retention worker and audit purge), sends no mail, and refuses the SMTP and AI connection tests (`409 outbound_disabled`), and refuses retention runs and account anonymization (`409 restore_mode_blocked`). Visible on `/api/ready` (`restore_mode`), as the metric `tikkit_restore_mode` and as a banner for Admins. The image also answers `tikkit-api restore-check`, `credentials-check` and `suppress-restored-email --before <time>` for the restore script. Remove it, then restart, to go live. |
| `TEST_DATABASE_URL` | tests only | `DATABASE_URL` | Maintenance connection used by integration tests to create/drop throwaway databases. |

AI providers are not environment configuration: they are optional, disabled by
default, and configured by an Admin at runtime (`/admin/ai`). Provider API keys
are encrypted with `ENCRYPTION_KEY`, bound to their provider, and write-only;
with a missing or unreadable key, tickets go straight to operators. Key rotation
and recovery: [ai-configuration.md](ai-configuration.md#encryption-key-rotation-and-recovery).
No new environment variables. Outbound SMTP is optional too
and is **not** an environment variable. An Admin saves the host, port, sender,
username, and password in the app (`/admin/email`). Username and password are
encrypted with `ENCRYPTION_KEY` (AES-256-GCM) and are write-only: never returned,
logged, or written into audit events. Losing `ENCRYPTION_KEY` makes them
unreadable; enter them again. Transport security is either required
STARTTLS or implicit TLS, and the server certificate is always verified. Without
a saved, enabled connection, Admins copy invitation links and recovery emails
are not sent (see [auth.md](auth.md#email-delivery)). The API starts and serves
the full service desk either way. Each delivery attempt is limited to 15 seconds.
Background sends try transient failures at most 3 times; the Admin test sends once.

## Upgrading

**`TRUST_PROXY_HEADERS` was replaced by `TRUSTED_PROXIES` (#44).** The old
switch trusted `X-Forwarded-For` from any peer. An API refuses to start if the old switch is enabled. A disabled
legacy switch means no proxy is trusted.
Migrate by removing `TRUST_PROXY_HEADERS` and setting `TRUSTED_PROXIES` to
the network of the reverse proxy and the web server, e.g.
`TRUSTED_PROXIES=172.30.0.0/24` with a fixed Compose subnet (see
`docker-compose.prod.example.yml`). Without it, every client behind the proxy
shares one rate-limit bucket and audit IP.

## Web server (adapter-node)

`apps/web/build` reads `HOST`, `PORT`, `API_ORIGIN`, and adapter-node's own
`PROTOCOL_HEADER`, `HOST_HEADER`, `ADDRESS_HEADER`, `XFF_DEPTH`. adapter-node 6
(SvelteKit 3) has **no `ORIGIN` variable**: unless the build sets
`kit.paths.origin`, the page origin is `https://` plus the request's `Host`
header (`HOST_HEADER` to read another header, `PROTOCOL_HEADER` for the
protocol). SvelteKit's origin check for form posts and the SSR fetch rewrite
compare against that origin, so the reverse proxy must accept only the
configured site host and pass it unchanged. Set
`PROTOCOL_HEADER=x-forwarded-proto` behind a proxy that overwrites that header:
pages then send HSTS for https requests (without it they never do).

## Probes and shutdown

- `GET /api/health`: liveness, always `200 {"status":"ok"}` without touching dependencies.
- `GET /api/ready`: readiness, `200 {"status":"ready",...}` when PostgreSQL answers and
  its applied migrations match this build; otherwise `503` with
  `{"status":"unavailable","checks":{"database":"ok|unavailable","migrations":"ok|pending|incompatible|unknown"}}`.
- On SIGTERM or Ctrl+C the API stops accepting connections, drains in-flight
  requests for up to `SHUTDOWN_TIMEOUT_SECS`, waits up to 5 seconds for
  workers and up to 5 more seconds for the database pool to close, then exits.

## Operational access

Keep the API socket private to the public reverse proxy and trusted internal
services. The public proxy is responsible for stripping the inbound
`X-Request-ID` on every `/api/*` request before forwarding to the API. The
checked-in Caddy example uses `header_up -X-Request-ID`, and the Vite dev proxy
applies the same rule. The SSR server connects directly over the private
network and supplies an ID it generated. Forward the API's response header
back to the browser so users can report it.

Set `METRICS_PORT` on the API service to enable scraping. The binary binds the
metrics router to `127.0.0.1` in its own network namespace. The portal and the
public API listener have no metrics route. For Compose, run a trusted local
collector in the API container's network namespace (e.g. Compose
`network_mode: "service:api"`) and scrape `127.0.0.1:9091` there. Do not put a
public reverse proxy in that namespace or use host networking for the API.
With a shared namespace, start Prometheus with
`--web.listen-address=127.0.0.1:9090`. Keep both that port and the metrics port
unpublished and outside public proxy routes. Local processes in that namespace can scrape without
credentials. Restrict log access and the collector's UI and storage to
operators. The [optional Compose overlay](../docker-compose.observability.example.yml)
sets up the listener, scraper, and rules together. Keep the probe status codes available to the deployment's health
check. See [the operations runbook](operations.md) for the scrape configuration,
alert rules and diagnostic commands.

## Production

See the [single-host deployment guide](deployment.md) and
[production environment example](../.env.production.example).

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
  category exemptions. AI itself is configured separately
  ([ai-configuration.md](ai-configuration.md)). AI budgets, the concurrency
  limit, the circuit breaker, and usage retention are Admin settings stored in
  the database, not environment variables ([ai-usage.md](ai-usage.md)).
- Data retention periods and automatic deletion are Admin settings stored in
  the database as well ([retention.md](retention.md)). Every period defaults
  to "keep"; there is no environment variable for them.

This issue stores lifecycle policies for the ticket workflows that follow.
Enforcement, snapshotted proposals and timeout processing belong to their
respective ticket/lifecycle issues. Admin-managed SMTP is tracked in #28;
category/priority configuration is tracked in #15.
