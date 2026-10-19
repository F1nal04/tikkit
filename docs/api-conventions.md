# API conventions

All backend routes live under `/api/`. The contract is described by OpenAPI 3.1
at `GET /api/openapi.json`, committed as `apps/api/openapi.json`, and consumed
by the frontend through the generated, typed `@tikkit/api-client` package.
The web app and API ship together from one commit behind one origin, so there is
no URL versioning; contract changes stay additive, and a breaking change would
get a new path.

Rust is the authorization and validation boundary. The client only types and
transports requests.

## Errors

Every non-2xx response (except the `/api/ready` probe body) is:

```json
{
  "error": {
    "code": "validation_failed",
    "message": "Some fields are invalid.",
    "details": { "fields": { "subject": ["required"], "priority_id": ["invalid"] } }
  },
  "request_id": "5f0c..."
}
```

- `code` is a stable snake_case identifier (`ErrorCode` in the OpenAPI doc).
  Clients translate by `code` (English and German); `message` is safe English
  fallback text that never contains internals, SQL, or secrets.
- `details.fields` maps dotted field paths to stable field codes such as
  `required`, `invalid`, `unknown_field`, `invalid_cursor`. More specific codes
  are added per feature.
- `request_id` equals the `x-request-id` response header. Clients may send their
  own `x-request-id`; otherwise the API generates a UUID. Logs carry it.

| Situation | Status | Code |
| --- | --- | --- |
| Body is not JSON / malformed / wrong top-level type | 400 | `invalid_json` |
| Missing `content-type: application/json` | 415 | `unsupported_media_type` |
| Field missing or of the wrong type | 422 | `validation_failed` |
| Field not in the schema (types use `deny_unknown_fields`) | 422 | `unknown_field` |
| Malformed path identifier, e.g. a bad UUID | 400 | `invalid_identifier` |
| Unparsable query parameter | 400 | `invalid_query` |
| JSON body over 1 MiB | 413 | `payload_too_large` |
| Not signed in | 401 | `unauthorized` |
| Login failed (any reason) | 401 | `invalid_credentials` |
| State-changing request without `X-Requested-With: tikkit` | 403 | `csrf_rejected` |
| Too many login/setup attempts (`Retry-After` header set) | 429 | `rate_limited` |
| First-admin setup already done | 409 | `setup_already_completed` |
| Invitation token unknown, expired, revoked, or used | 410 | `invitation_invalid` |
| Password reset token unknown, expired, or used | 410 | `reset_token_invalid` |
| Change would leave no active Admin | 409 | `last_active_admin` |
| Signed in, action not allowed on a visible resource (including Admin-only endpoints) | 403 | `forbidden` |
| Unknown route, or a resource the caller may not see | 404 | `not_found` |
| Wrong method | 405 | `method_not_allowed` |
| Stale or concurrent update lost | 409 | `conflict` |
| Demoting or deactivating the last active Admin | 409 | `last_active_admin` |
| Demoting or deactivating an active team's lead | 409 | `active_team_lead` |
| SMTP is not configured | 409 | `smtp_not_configured` |
| Mail server rejected the username or password | 502 | `smtp_auth_failed` |
| Verified TLS to the mail server failed | 502 | `smtp_tls_failed` |
| Mail server DNS, connect, or timeout | 502 | `smtp_network_failed` |
| Mail server refused the message | 502 | `smtp_delivery_failed` |
| Stored mail password cannot be decrypted | 502 | `smtp_credential_unreadable` |
| Anything unexpected (logged with the request ID) | 500 | `internal` |

**Denied access does not reveal existence.** When the caller may not see a
resource at all (another requester's ticket, a ticket dispatched to another
team, an AI-solved record for operators), respond `not_found`, identical to a
missing resource. Use `forbidden` only when the caller can already see the
resource but may not perform the action (e.g. a read-only team member replying
to a claimed ticket). Capability denials (a role that can never do this, e.g. a
User calling an Admin endpoint) are `forbidden` too. The matrix and helpers are
in [authorization.md](authorization.md).

In Rust: return `api::ApiError`, extract with `api::Json`, `api::Path`,
`api::Query` (not the raw axum extractors) so rejections use the envelope.

## Authentication and CSRF

Sessions are the HttpOnly `tikkit_session` cookie (see [auth.md](auth.md)).
Every `POST`/`PUT`/`PATCH`/`DELETE` under `/api/` must carry
`X-Requested-With: tikkit`, otherwise `403 csrf_rejected`. Browsers cannot add
custom headers to cross-site requests without a CORS preflight, which the API
never grants. `@tikkit/api-client` sends the header on every request.
Handlers take `auth::CurrentUser` to require a session (`401 unauthorized` otherwise).

## Lists: pagination, ordering, filtering

- Query `?limit=` (default 25, clamped to 1-100) and `?cursor=` (opaque; pass
  back `next_cursor`). Use `api::Pagination` as the query struct.
- Response `{ "items": [...], "next_cursor": "..." | null }` (`api::Page<T>`).
  Fetch `limit + 1` rows and build with `Page::from_rows`.
- Default order is newest first by `(created_at, id)`; the cursor encodes that
  pair, so a page never repeats or skips rows that already existed. `created_at`
  is the inserting transaction's start time, so a row committed late can sort
  before a cursor and be missed by a client already paging; refresh from the
  first page when completeness matters. Endpoints that need another order
  document a fixed `sort` enum; there is no free-form ordering.
- Conversation messages use ascending commit sequence and an opaque sequence
  cursor instead of the default timestamp order; see [conversations.md](conversations.md).
- Filters are plain query parameters with documented values (stable
  identifiers, e.g. `status=queued`). Unparsable or unknown filter
  values are rejected with 400 `invalid_query`. Filters and counts always apply the caller's visibility
  rules in SQL, never after fetching.

## Identifiers and language

Role (`user`, `operator`, `specialist`, `team_lead`, `admin`), ticket status,
language (`en`, `de`), and error codes are machine identifiers published as
OpenAPI enums. They never change with the interface language. Clients send
`Accept-Language` for display preferences only; it never affects authorization
or business rules. Timestamps are RFC 3339 UTC.

## Requester ticket assignment (contract for upcoming ticket endpoints)

Requester-facing ticket responses include `TicketAssignmentView`:
`team_name` (`null` = shared general queue) and `assignee_display_name`
(`null` = unassigned). Both keys are always present. Public conversation
messages additionally expose author display names and author kind, with
account IDs redacted. Internal notes and team membership
are never exposed to requesters.

## Typed client and drift checks

```sh
cargo run --locked -p tikkit-api --bin openapi > apps/api/openapi.json  # after API changes
bun run --cwd packages/api-client generate                               # regenerate src/schema.d.ts
```

CI catches drift in two steps, both part of the root `check`/`test` tasks:
`cargo test` fails if `apps/api/openapi.json` differs from the code
(`served_openapi_matches_committed_file`), and `@tikkit/api-client`'s `check`
runs `check:client` (`openapi-typescript --check`) which fails if
`schema.d.ts` differs from `openapi.json`.

Use the client from SvelteKit `load` functions with the provided `fetch`, and
create it per request on the server; it stores no per-user state:

```ts
import { createClient } from "@tikkit/api-client";

export const load = async ({ fetch }) => {
  const api = createClient({ fetch });
  return { health: await api.request("get", "/api/health") };
};
```

Requests always use `credentials: "include"` and send `X-Requested-With: tikkit`.
Failures throw `ApiError` with `status`, `code`, `fields`, and `requestId`.
When no envelope arrives (network failure: `status` 0, or a non-JSON body such
as a proxy's HTML 502) `code` is the client-side `unavailable`.
