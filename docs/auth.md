# Authentication

Local username/password accounts with server-side sessions. Code: `apps/api/src/auth.rs`.

## First-admin setup

`GET /api/setup` returns `{"setup_required": bool}`. While it is `true`, the web
app sends every page to `/setup`. `POST /api/setup` (`username`, `email`,
`password`, `language`) creates the first Admin and signs them in. One
transaction claims the singleton `organization` row
(`UPDATE ... WHERE setup_completed_at IS NULL`); concurrent attempts wait on the
row lock and then match nothing, so exactly one Admin is created. Afterwards,
and across restarts, setup returns `409 setup_already_completed`. Later accounts
come from Admin invitations.

## Credentials

- Usernames: `lower(trim(username))` is unique; 3-64 of `a-z 0-9 . _ -`. Email is
  contact data only, never unique, never used to find an account.
- Passwords: 12-256 characters, not a well-known password, not one repeated
  character, not the username. Hashed with Argon2id (default parameters).
- Login failures are always `401 invalid_credentials`, for unknown users, wrong
  passwords, and deactivated accounts alike; unknown users are verified against a
  dummy hash so timing matches.

## Sessions

- Cookie `tikkit_session`: `HttpOnly; SameSite=Lax; Path=/; Max-Age=<lifetime>`,
  plus `Secure` when `ENVIRONMENT=production`. The value is 32 random bytes
  (base64url); only its SHA-256 is stored in `sessions`.
- Every login creates a new session; a cookie presented before login is never
  adopted (no fixation).
- Absolute expiry `SESSION_TTL_HOURS` (default 14 days). Once a session is older
  than `SESSION_ROTATE_HOURS` (default 24h) the next request revokes it and
  issues a new token with the same expiry. `last_seen_at` is refreshed at most hourly.
- Every request re-checks revocation, expiry, and `accounts.is_active`, so a
  deactivated account loses access immediately.
- `POST /api/auth/logout` revokes the session and clears the cookie (idempotent).
  `POST /api/auth/password` revokes all of the account's sessions and issues a new one.
- During SSR the web server forwards the browser cookie to the API and relays a
  rotated `Set-Cookie` back to the browser (`hooks.server.ts`).

## CSRF

State-changing requests (`POST`/`PUT`/`PATCH`/`DELETE`) need
`X-Requested-With: tikkit`, else `403 csrf_rejected`. Cross-site pages cannot
set custom headers without a CORS preflight, which the API never allows; the
`SameSite=Lax` cookie is the second layer. The typed client sends the header.

## Abuse protection

Login, setup, and password change allow 10 attempts per minute per client IP and
5 per minute per username, then `429 rate_limited` with `Retry-After`. Counters
are in memory (one API process). The IP is the socket peer, or the last
`X-Forwarded-For` hop when `TRUST_PROXY_HEADERS=true` behind a trusted proxy.

## Audit events

Append-only `audit_events`: `setup_completed`, `login_succeeded`, `login_failed`
(normalized username, reason, IP), `logout`, `password_changed`,
`session_revoked`. Passwords and tokens are never stored or logged.

## Endpoints

| Method | Path | Result |
| --- | --- | --- |
| GET | `/api/setup` | `{setup_required}` |
| POST | `/api/setup` | `200 Account` + cookie, `409`, `422`, `429` |
| POST | `/api/auth/login` | `200 Account` + cookie, `401`, `429` |
| POST | `/api/auth/logout` | `204`, cookie cleared |
| GET | `/api/auth/me` | `200 Account` or `401` |
| POST | `/api/auth/password` | `204` + new cookie, `401`, `422`, `429` |
