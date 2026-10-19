# Authentication

Local username/password accounts with server-side sessions. Code: `apps/api/src/auth.rs`
(setup, login, sessions) and `apps/api/src/accounts.rs` (invitations, recovery,
profile, deactivation).

## First-admin setup

`GET /api/setup` returns `{"setup_required": bool}`. While it is `true`, the web
app sends every page to `/setup`. `POST /api/setup` (`username`, `email`,
`password`, `language`) creates the first Admin and signs them in. One
transaction claims the singleton `organization` row
(`UPDATE ... WHERE setup_completed_at IS NULL`); concurrent attempts wait on the
row lock and then match nothing, so exactly one Admin is created. Afterwards,
and across restarts, setup returns `409 setup_already_completed`. Later accounts
come from Admin invitations.

**Complete `/setup` before exposing a new deployment publicly.** There is no
bootstrap secret by design: until setup is done, whoever reaches `/setup` first
becomes the Admin.

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
  adopted (no fixation) and its session is revoked.
- Absolute expiry `SESSION_TTL_HOURS` (default 14 days). Once a session is older
  than `SESSION_ROTATE_HOURS` (default 24h) the next request issues a new token
  with the same expiry. The old token keeps working for one minute
  (`rotated_at`) so parallel in-flight requests are not signed out; signing out
  with either token ends both. `last_seen_at` is refreshed at most hourly.
- Every request re-checks revocation, expiry, and `accounts.is_active`, so a
  deactivated account loses access immediately.
- `POST /api/auth/logout` revokes the session and clears the cookie (idempotent).
  `POST /api/auth/password` revokes all of the account's sessions and issues a new one.
- During SSR the web server forwards only `tikkit_session` to the API, read via
  `event.cookies` so a token rotated earlier in the same render is used, and
  relays a rotated `Set-Cookie` back to the browser (`hooks.server.ts`).

## CSRF

State-changing requests (`POST`/`PUT`/`PATCH`/`DELETE`) need
`X-Requested-With: tikkit`, else `403 csrf_rejected`. Cross-site pages cannot
set custom headers without a CORS preflight, which the API never allows; the
`SameSite=Lax` cookie is the second layer. The typed client sends the header.

## Abuse protection

Login, setup, password change, invitation acceptance, and recovery requests allow 10 attempts per minute per client IP and
then 5 per minute per username; the token endpoints (`/api/invitations/lookup`,
`/api/auth/recovery/reset`) allow 10 per minute per client IP, else `429 rate_limited` with `Retry-After`.
The IP is checked first, so a blocked client creates no further keys. Counters
are in memory (one API process) and capped at 10,000 live keys; beyond that new
keys are refused for the window. The IP is the socket peer, or the last
`X-Forwarded-For` hop when `TRUST_PROXY_HEADERS=true`: set it only behind a
reverse proxy that overwrites that header (the production compose sets it for Caddy).

## Invitations

After setup, accounts are created only from Admin invitations.

- `POST /api/invitations` (Admin): optional `role` (default `user`, any of the
  five), `email`, `language` (default `en`), `expires_in_hours` (1-720, default
  168). Returns the link `PUBLIC_ORIGIN/invite/<token>?lang=<language>` **once**;
  only the token's SHA-256 is stored, so a lost link means revoke and re-invite.
  With SMTP and an `email`, the link is also emailed in that language.
- `GET /api/invitations` (Admin) lists pending ones; `POST /api/invitations/{id}/revoke`
  revokes one (used or revoked ones are `404`).
- `POST /api/invitations/lookup {token}` previews role, email and language without
  consuming anything. Tokens travel in request bodies, never API URLs, so request logs never hold them.
- `POST /api/invitations/accept` (`token`, `username`, `email`, `password`,
  `language`) creates the account with the invitation's stored role and signs it
  in. The invitation row is locked (`FOR UPDATE`) and marked used in the same
  transaction as the account insert, so validation errors, a taken username, or a
  failed transaction leave it usable, and concurrent acceptance has one winner.
  Unknown fields such as `role` are rejected (`422 unknown_field`).
- Unknown, expired, revoked, and used tokens are all `410 invitation_invalid`.
- An existing account with the same email is never reused, linked, or upgraded.

## Password recovery

- `POST /api/auth/recovery {username}` always answers `202` with no body. Only an
  active account whose normalized username matches gets a token; the link
  `PUBLIC_ORIGIN/reset-password/<token>?lang=<account language>` goes to that
  account's stored email. Email is never used to find an account.
- Tokens: 32 random bytes, SHA-256 stored, bound to one account ID, valid **one
  hour**, single use. Rate limited like login (`recovery` scope), and at most
  one link per account every 15 minutes: while an unused link younger than that
  exists, further requests still answer `202` but create and send nothing. The
  cooldown query runs for unknown usernames too, so both paths take similar time.
- `POST /api/auth/recovery/reset {token, new_password}` sets the password, marks
  every unused recovery token of the account used, and revokes all of its
  sessions (only that account's, even when others share the email). A rejected
  password leaves the token usable. Bad tokens are `410 reset_token_invalid`.
  The user then signs in normally.

## Email delivery

SMTP is optional (`SMTP_URL`, `SMTP_FROM`, see [configuration.md](configuration.md)).
Messages are plain text in the recipient's language and sent in the background,
so the response time does not depend on delivery. Without SMTP, or when sending
fails, nothing is retried: Admins copy invitation links from the response, and a
recovery request only logs `no email was sent` at info level. Logs never contain
tokens, links, addresses, or SMTP credentials. (ponytail: no outbox or retries
yet; the notification outbox and Admin-managed SMTP settings replace this.)

The `/invite/...` and `/reset-password/...` page URLs do contain the token; keep
reverse-proxy access logs off or path-redacted. Every web page is served with
`Referrer-Policy: no-referrer` so the token never leaks through `Referer`.

## Profile

`PATCH /api/auth/me` (`email`, `language`, both optional) updates the signed-in
account. The web app renders in the account's language once signed in, unless a
request picks one with `?lang=`.

## Deactivation

`POST /api/accounts/{id}/deactivate` and `.../reactivate` (Admin) flip
`accounts.is_active`. Deactivation revokes all the account's sessions in the same
transaction; login is refused and every request re-checks `is_active`. Tickets,
messages, and audit history keep referring to the account. The same transaction
marks the account's unused recovery tokens used (reactivation does not revive
them) and revokes the pending invitations it issued (`invitation_revoked`,
reason `issuer_deactivated`). A database trigger keeps the last active Admin
(`409 last_active_admin`), even against two Admins at once (see
[authorization.md](authorization.md)). Deactivating an active team's lead is
`409 active_team_lead`. `GET /api/accounts` (Admin)
lists accounts. Non-Admins get `403 forbidden` on Admin endpoints.

## Audit events

Append-only `audit_events`: `setup_completed`, `login_succeeded`, `login_failed`
(reason and IP; the account as subject when the username matched, never the typed username), `logout`, `password_changed`,
`session_revoked` (with `reason`), `invitation_created`, `invitation_revoked`,
`invitation_accepted`, `recovery_requested` (IP, `matched`), `recovery_completed`,
`profile_updated` (changed field names), `account_deactivated`,
`account_reactivated`. Passwords and tokens are never stored or logged.

## Endpoints

| Method | Path | Result |
| --- | --- | --- |
| GET | `/api/setup` | `{setup_required}` |
| POST | `/api/setup` | `200 Account` + cookie, `409`, `422`, `429` |
| POST | `/api/auth/login` | `200 Account` + cookie, `401`, `429` |
| POST | `/api/auth/logout` | `204`, cookie cleared |
| GET | `/api/auth/me` | `200 Account` or `401` |
| POST | `/api/auth/password` | `204` + new cookie, `401`, `422`, `429` |
| PATCH | `/api/auth/me` | `200 Account`, `401`, `422` |
| POST | `/api/auth/recovery` | `202` always, `429` |
| POST | `/api/auth/recovery/reset` | `204`, `410`, `422` |
| GET/POST | `/api/invitations` | Admin: `200 Page<Invitation>` / `201 CreatedInvitation` |
| POST | `/api/invitations/{id}/revoke` | Admin: `204`, `404` |
| POST | `/api/invitations/lookup` | `200 InvitationPreview`, `410` |
| POST | `/api/invitations/accept` | `200 Account` + cookie, `410`, `422`, `429` |
| GET | `/api/accounts` | Admin: `200 Page<AccountSummary>` |
| POST | `/api/accounts/{id}/deactivate`, `.../reactivate` | Admin: `204`, `404`, `409` |

## Admin roles, teams and organization settings

`PUT /api/accounts/{id}` takes `{role, expertise, team_id}` as the complete desired
role/membership state. `expertise` and `team_id` may be null. Only Specialists may
have nonblank expertise (at most 120 characters). Only active Operators,
Specialists and Team Leads may join a non-archived team. Admins and Users must
have no team. Membership is changed in the same transaction as the role; an
invalid target rolls back everything. The sole lead is blocked with
`active_team_lead` until replaced or archived. The existing last-active-Admin
constraint also applies to role edits. Account lists include immutable ID,
username, role, expertise, team ID and explicit `is_team_lead`; email never
identifies the account being edited.

`GET /api/teams` is a cursor-paginated Admin list including archived teams.
`POST /api/teams {name, lead_account_id}` requires an active Team Lead who does
not already lead another team. `PUT /api/teams/{id}
{name, lead_account_id, archived}` renames, replaces leadership or archives.
Names are nonblank, trimmed, unique and at most 120 characters. Replacing a lead
removes only their leadership designation; membership remains. Selecting a
replacement from another team explicitly moves that account, provided it does
not lead that team. Archiving requires `lead_account_id: null`; restoring requires
an eligible lead. Role alone never grants team leadership. Admin and operational
duties need separate accounts, which may share an email.

`GET /api/invitations?include_history=true` includes pending, accepted, expired
and revoked invitations with a stable `status`. Accepted rows include the new
account ID and username; tokens and links are never returned in lists. Omitting
this flag retains the existing pending-only behavior. `POST
/api/invitations/{id}/reissue` atomically revokes an unused, not-yet-revoked
invitation and creates a replacement with the same role/contact/language,
valid for 168 hours. Only this response reveals the new link. Accepted, revoked
or already-reissued invitations return `409 conflict`; the Admin may create a
separate invitation if needed. A concurrent acceptance/reissue has one winner.
An omitted invitation language uses the organization default.

`GET /api/admin/settings` and `PUT /api/admin/settings` are Admin-only. The PUT
replaces the complete settings object. `GET /api/organization` gives signed-in
clients the same non-secret effective policy. Settings are described in
[configuration.md](configuration.md#organization-settings). The app reflects the
organization name after refresh, formats invitation times in the organization's
timezone and preselects its default invitation language. Existing accounts keep
their own language preference. All API responses use `private, no-store` so
permission-sensitive results are fetched again rather than reused from caches.
