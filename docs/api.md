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
| `conflict` | 409 | Conflicts with current state (e.g. a concurrent update won) |
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
