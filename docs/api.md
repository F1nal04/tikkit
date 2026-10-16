# API conventions

The Rust API (`apps/api`) is the only place where validation and
authorization happen. The web app talks to it through one generated, typed
client. This page describes the contract every endpoint follows.

## Versioning and routing

- Product endpoints live under **`/api/v1/...`**. Breaking changes need a new
  version prefix; additive changes (new endpoints, new optional fields, new
  error/field codes) do not.
- Operational endpoints are unversioned: `GET /api/health` (liveness),
  `GET /api/health/live`, `GET /api/health/ready`, and
  `GET /api/openapi.json` (the OpenAPI 3.1 document).
- JSON field names are **camelCase**; enum values and all machine
  identifiers (error codes, roles, statuses) are stable English
  **snake_case** strings, independent of the user's language.
- Timestamps are RFC 3339 in UTC.

## Errors

Every non-2xx response has this body (`application/json`):

```json
{
  "error": {
    "code": "validation_failed",
    "message": "Some fields are invalid.",
    "requestId": "01920c5e-7a4b-7cc2-9d1e-5f0c2b8a1e44",
    "fields": [
      { "field": "username", "code": "too_short", "params": { "min": 3 } }
    ]
  }
}
```

- `code` is stable and is what clients branch on and translate.
- `message` is a safe English fallback. It never contains internals (SQL,
  stack traces, secrets, other users' data) and is not meant to be shown
  as-is in a localized UI.
- `requestId` equals the `x-request-id` response header and appears in the
  server logs.
- `fields` is present only when specific inputs are at fault. `field` uses
  the request's camelCase names, `.` for nesting and `[n]` for array items.
  `params` holds interpolation values for the translated message.

| Code | Status | Meaning |
|---|---|---|
| `invalid_request` | 400 | Malformed JSON, missing/wrong `content-type`, unknown fields, missing required fields, wrong types, unknown query parameters |
| `validation_failed` | 422 | Well-formed input whose values are invalid (see `fields`), including bad `limit`/`cursor` |
| `unauthenticated` | 401 | No valid session |
| `forbidden` | 403 | Authenticated but not permitted |
| `csrf_rejected` | 403 | Unsafe method failed the origin check |
| `not_found` | 404 | Unknown route, malformed identifier (e.g. bad UUID), missing resource, **or a resource the caller may not see** |
| `method_not_allowed` | 405 | Route exists, method does not (`Allow` header lists methods) |
| `invalid_credentials` | 401 | Login failed (same for unknown user, wrong password, deactivated account) |
| `conflict` | 409 | Conflicts with current state (e.g. a concurrent update won) |
| `setup_completed` | 409 | First setup has already been completed |
| `last_admin` | 409 | Would demote or deactivate the last active Admin |
| `team_membership_conflict` | 409 | Team membership conflicts with the change (second team, Admin/User with a membership) |
| `team_lead_required` | 409 | The account is a team's designated lead; appoint a replacement first |
| `team_has_open_tickets` | 409 | The team still has open tickets (archiving) |
| `payload_too_large` | 413 | Body exceeds the limit |
| `rate_limited` | 429 | Too many requests (`Retry-After` may be set) |
| `internal_error` | 500 | Unexpected failure; details only in the log under the request id |
| `service_unavailable` | 503 | A dependency (e.g. the database) is unavailable |

Field codes: `required`, `too_short` (`min`), `too_long` (`max`),
`out_of_range` (`min`, `max`), `invalid_format`, `invalid_value`,
`invalid_type`, `unknown_field`, `already_taken`.

Invisible resources return `not_found`, never `forbidden`, so existence is
not leaked. Malformed identifiers return the same `not_found` as missing ones.

### In Rust

Handlers return `Result<T, ApiError>` (`apps/api/src/error.rs`). Use the
extractors from `apps/api/src/extract.rs`:

- `ApiJson<T>` — JSON body; requires `content-type: application/json`.
- `ValidJson<T>` — `ApiJson<T>` plus `T: Validate` (422 with fields).
- `ApiPath<T>` — any unparsable path value → `not_found`.
- `ApiQuery<T>` — query string → `invalid_request` on unknown/mistyped params.

Request DTOs (bodies and query structs) use `#[serde(deny_unknown_fields)]`
and `#[serde(rename_all = "camelCase")]`. Validation uses the `Validate`
trait with the `FieldErrors` builder. `ApiError::internal(detail)` logs
`detail` with the request id and returns only `internal_error`; never pass
secrets.

## Authentication and sessions

Tikkit uses server-side sessions referenced by an HttpOnly cookie. There is
no token-based or public sign-up API: the first Admin is created by first
setup, every later account by an invitation.

### Endpoints

| Endpoint | Purpose |
|---|---|
| `GET /api/v1/setup` | `{"setupRequired": bool}`. Public. |
| `POST /api/v1/setup` | `{username, displayName, email, password, language}` → `201 SessionInfo` + cookie. One-time: afterwards `409 setup_completed`, also after restarts. Concurrent attempts: exactly one wins. |
| `POST /api/v1/auth/login` | `{username, password}` → `200 SessionInfo` + cookie, or `401 invalid_credentials`. |
| `POST /api/v1/auth/logout` | Revokes the presented session and clears the cookie. Always `204`. |
| `GET /api/v1/auth/session` | `200 SessionInfo` or `401 unauthenticated`. The source of truth for the signed-in UI state (e.g. after a refresh); the UI must not persist its own copy. |

`SessionInfo` is `{account: Account, expiresAt}` where `expiresAt` is the
absolute expiry. Responses that carry session data or change the cookie
send `Cache-Control: no-store`.

- **Usernames** are trimmed, NFKC-normalized and lowercased for login and
  uniqueness; after normalization they must be 3–32 characters of
  `[a-z0-9._-]`. The trimmed input is kept for display. A taken username is
  `422` with field code `already_taken`.
- **Email** is a contact address only: not unique, never used to find or
  merge accounts.
- **Passwords**: 12–256 characters and not equal to the username (field
  codes `too_short`, `too_long`, `invalid_value` with
  `params.reason = "matches_username"`). Stored as argon2id (OWASP
  parameters m=19 MiB, t=2, p=1).
- **Login failures** (unknown username, wrong password, deactivated account)
  all return the identical `401 invalid_credentials` and take comparable
  time (unknown accounts are verified against a dummy hash).

### Cookie

| | Development | Production (`TIKKIT_ENV=production`) |
|---|---|---|
| Name | `tikkit_session` | `__Host-tikkit_session` |
| Attributes | `HttpOnly; SameSite=Lax; Path=/; Max-Age=<remaining lifetime>` | same plus `Secure` |

The value is 32 random bytes (base64url); the database stores only its
SHA-256 digest.

### Session lifetime

- **Idle timeout** 12 hours without an authenticated request.
- **Absolute lifetime** 7 days from login, whatever the activity.
- **Rotation**: the first request at least 15 minutes after the last
  rotation receives a new cookie. The previous token stays valid for 60
  seconds so requests already in flight succeed; concurrent requests rotate
  only once.
- **Login always creates a new session.** A session cookie sent with the
  login request is revoked (session-fixation defence). Other sessions of
  the account (other devices) are unaffected.
- **Revocation** is server-side and final: logout, deactivation (the
  account is reloaded on every request, so a deactivated account's sessions
  are rejected and revoked at once), and later password change/reset.
  Role changes apply on the next request.
- An expired, revoked or unknown cookie gets `401 unauthenticated` and a
  `Set-Cookie` that clears it.

### CSRF rule for API clients

Every unsafe request (`POST`, `PUT`, `PATCH`, `DELETE`) under `/api/v1/`
must send `Origin` equal to `PUBLIC_ORIGIN`; when `Origin` is absent the
origin of `Referer` must match instead. Anything else (no header, another
origin, `Origin: null`) is rejected with `403 csrf_rejected` before the
handler runs. Browsers send `Origin` automatically for same-origin `fetch`;
scripts and other non-browser clients must set it explicitly, for example
`curl -H 'Origin: https://help.example.com' …`. `GET`/`HEAD` and the
health probes are not checked.

### Rate limits

In-memory token buckets per API process (reset on restart):

| Scope | Budget |
|---|---|
| Login, per client address | 50 per minute |
| Login, per client address and normalized username | 10 per minute |
| First setup, per client address | 10 per minute |

Exceeding a budget returns `429 rate_limited` with `Retry-After` (seconds).
The client address is the TCP peer, or with `TRUST_PROXY_HEADERS=true` the
right-most `X-Forwarded-For` entry (set it only behind a reverse proxy that
overwrites that header).

### Audit events

Recorded in `audit_events` (never containing passwords or tokens):
`setup.completed`, `auth.login_succeeded`, `auth.login_failed` (anonymous
actor; metadata holds the normalized username attempted, a reason and the
client address), `auth.logout`, `auth.session_revoked`.

### In Rust

- `auth::Actor` extractor (`account_id`, `username`, `display_name`,
  `role`, `language`, `session_id`) for handlers that require a session;
  `Option<Actor>` for optional authentication.
- `auth::SessionCookies` to set/clear the cookie from a handler.
- `auth::sessions::{create, revoke_all_for_account, rotate_now}` and
  `auth::account::load_account` for invitation acceptance, recovery and
  password changes.
- `crate::domain::{Role, AccountStatus}` are the shared enums.

## Administration (Admin only)

Every endpoint requires a session (`401 unauthenticated`) and the Admin role
(`403 forbidden`). The actor is reloaded per request, so a demoted Admin
loses access with its next request. Rules and error codes:
[permissions.md](permissions.md).

| Endpoint | Purpose |
|---|---|
| `GET /api/v1/admin/accounts?role&status&q&limit&cursor` | `Page<AdminAccount>`, ordered by normalized username. Invalid `role`/`status` → 422. |
| `GET /api/v1/admin/accounts/{id}` | `AdminAccount` |
| `PATCH /api/v1/admin/accounts/{id}/role` | `{role, specialization?, replacementLeadId?}` → `AdminAccount`. Unknown roles → 400. `409 last_admin` / `team_membership_conflict` / `team_lead_required`. |
| `GET /api/v1/admin/teams?includeArchived` | `Team[]` (not paginated) |
| `POST /api/v1/admin/teams` | `{name, leadAccountId}` → `201 Team` (team and lead atomically) |
| `GET /api/v1/admin/teams/{id}` | `Team` |
| `PATCH /api/v1/admin/teams/{id}` | `{name}` → `Team` |
| `POST /api/v1/admin/teams/{id}/lead` | `{accountId, previousLead?: "keep_as_member" (default) \| "remove_from_team"}` → `Team` |
| `POST /api/v1/admin/teams/{id}/members` | `{accountId}` → `Team` |
| `DELETE /api/v1/admin/teams/{id}/members/{accountId}` | → `Team`; the lead cannot be removed (`409 team_lead_required`) |
| `POST /api/v1/admin/teams/{id}/archive` | → `Team`; `409 team_has_open_tickets` while open tickets remain |

Deactivation/reactivation and invitations are separate endpoints (#10).

## Pagination, ordering and filtering

- Query parameters: `limit` (integer 1–100, default 25) and `cursor` (opaque
  string from a previous response).
- Response: `{"items": [...], "nextCursor": "…" | null}`. `null` means last
  page. Lists do not return totals.
- Every list has one documented, stable order whose final key is the row id
  (for example `created_at DESC, id DESC`). Pagination is keyset-based: the
  cursor encodes the ordering key of the last returned row. Clients must
  treat it as opaque; a malformed or tampered cursor is `validation_failed`.
- Filters are explicit, documented query parameters validated in Rust.
  Unknown parameters are rejected with `invalid_request`.
- Authorization scoping (requester ownership, team isolation) is part of the
  SQL query itself, so items, cursors and counts never include rows the
  caller may not see. A cursor carries no authority.

In Rust: `ApiQuery<PageParams>` → `params.validate::<Key>()?` →
`PageRequest { limit, after }`; fetch `request.fetch_limit()` rows and build
the response with `Page::from_overfetched(rows, &request, |row| key)`.
Filtered list queries declare `limit`/`cursor` fields themselves (serde
cannot combine `flatten` with `deny_unknown_fields`) and build a
`PageParams` from them.

## Request IDs

Every response carries `x-request-id`. An inbound `x-request-id` (e.g. from
the reverse proxy) is reused only if it is 1–64 characters of
`[A-Za-z0-9._-]`; otherwise a UUID v7 is generated. The id is attached to
the request's tracing span and included in error bodies. Users can quote it
when reporting a problem.

## Limits

- Request bodies: 64 KiB by default (`payload_too_large`). Endpoints that
  need more (e.g. screenshot uploads) raise their own limit explicitly.
- Page size: at most 100 items.
- Cursor length: at most 512 characters.

## OpenAPI spec and typed client

- The spec is generated from `utoipa` annotations (`apps/api/src/openapi.rs`)
  and committed at `apps/api/openapi.json`. Every new endpoint needs
  `#[utoipa::path]` and must be listed in `ApiDoc`; every DTO derives
  `ToSchema`.
- The test `openapi_spec_is_up_to_date` fails when the committed file
  differs from the generated spec (ignoring `info.version`, which follows
  the crate version).
- The web client types in `apps/web/src/api/schema.gen.ts` are generated
  from that file by `openapi-typescript`. `bun run check` (web) fails when
  they are stale.

After changing endpoints or DTOs:

```sh
UPDATE_OPENAPI=1 cargo test --locked -p tikkit-api openapi   # rewrite apps/api/openapi.json
bun run --cwd apps/web generate:api                          # rewrite schema.gen.ts
```

Commit both files. The generator runs from `apps/web/tools/api-codegen`,
which has its own pinned install (`openapi-typescript` needs the
TypeScript 6 JS API, while the app compiles with TypeScript 7).

On the frontend, use the client from `apps/web/src/api/client.ts`:

```ts
import { api, unwrap, isApiError } from '../api/client'
import { errorMessage, fieldErrorMessage } from '../api/errors'

try {
  const health = await unwrap(api.GET('/api/health'))
} catch (error) {
  if (isApiError(error)) showError(errorMessage(error, locale), error.requestId)
}
```

Every non-2xx response and every network failure rejects with `ApiError`
(`code`, `status`, `fields`, `requestId`); network failures use the
client-only code `network_error`. `errors.ts` has English and German text
for every code, and TypeScript fails the build if a new server code has no
translation.
