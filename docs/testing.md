# Testing

Run everything from the repo root.

| Layer | Command | Where |
| --- | --- | --- |
| Web components and helpers | `bun run test` | `apps/web/src/**/*.test.ts` (Vitest, jsdom, Testing Library for Svelte) |
| API client | `bun run test` | `packages/api-client/src/*.test.ts` (Vitest, stubbed `fetch`) |
| API unit and HTTP | `bun run test` | `#[cfg(test)]` modules in `apps/api/src/` (drives the router with `oneshot`, no socket) |
| API + PostgreSQL | `bun run test` with `TEST_DATABASE_URL` | `apps/api/tests/*.rs` |
| Browser | `bun run test:e2e` | `apps/web/e2e/*.spec.ts` (Playwright, Chromium) |

`bun run test` runs the first four; browser tests are separate. Single runs:
`cargo test --locked -p tikkit-api <name>`, `bun run --cwd apps/web test -t <name>`,
`bun run --cwd apps/web test:e2e -g <name>` (build first).

## Database tests

```sh
docker compose -f docker-compose.dev.yml up -d --wait
TEST_DATABASE_URL=postgres://tikkit:tikkit@127.0.0.1:5432/tikkit bun run test
```

`TestDb::new()` (`apps/api/tests/support/mod.rs`) creates a fresh
`tikkit_test_<uuid>` database, applies all migrations, and drops it
(`WITH (FORCE)`) when the value is dropped, including when the test panics.
Every test gets its own database, so tests run in parallel and never depend on
order or shared data. Without `TEST_DATABASE_URL` (or `DATABASE_URL`) these
tests print `skipping` and pass; CI always sets it. The role in the URL must be
allowed to create databases. `each_test_database_is_isolated_and_dropped`
checks isolation and cleanup; after a run that was killed (e.g. `SIGKILL`),
list leftovers with
`docker compose -f docker-compose.dev.yml exec postgres psql -U tikkit -c "\l tikkit_test_*"`.

Add `mod support;` to a new `apps/api/tests/*.rs` file to use:

- `TestDb::new().await -> Option<TestDb>` (`.pool`, `.name`)
- `test_app(&pool) -> Router`: the real app; send requests with `tower::ServiceExt::oneshot`
  (state-changing requests need `x-requested-with: tikkit`); `test_app_with(&pool, Settings)`
  for other auth settings, e.g. `Secure` cookies. Screenshots go to
  `screenshot_dir(&pool)`, a temp directory removed with the `TestDb`
- `test_config() -> Config`: a valid configuration with fixed test secrets
- `fixtures::account(&pool, Role::Operator, "name") -> Uuid` for any of the
  five roles (`User`, `Operator`, `Specialist`, `TeamLead`, `Admin`); all share
  one email on purpose and sign in with `fixtures::PASSWORD`
  (stored as the precomputed Argon2id `fixtures::PASSWORD_HASH`)
- `fixtures::bootstrap_admin(&pool)` (first-setup Admin; also marks setup
  complete) and `fixtures::invited_account(&pool, role, "name")` (accepts a real
  invitation over HTTP; needs an active Admin first, e.g. `bootstrap_admin`)
- `call(&app, method, path, body, cookie) -> Reply` (`status`, `cookie`, `body`,
  `.code()`, `.session()`), `call_from(..., ip)` for another client IP, and
  `login(&app, username, password)`
- `support::smtp::listen(Listen::StartTls, Fault::None)`: an in-process TLS SMTP server
  (`.port`, `.ca_pem` for `Settings::smtp_trust_root_pem`, `.messages`); used by the SMTP and
  notification tests, never an external relay
- `fixtures::team(&pool, "Name", lead) -> Result<Uuid>`: team plus lead
  membership in one transaction; `fixtures::member(&pool, team, account)`

### Security boundary and authorization matrix

`tests/authz_matrix.rs` enumerates every operation of the OpenAPI document
(cross-checked against the router) for signed out and the five roles; a new
route fails `every_route_has_a_matrix_row` until it gets a row.
`tests/boundary.rs` covers the origin policy, cookies, headers, proxy trust,
budgets, and injection edges. See [security.md](security.md).

## AI provider tests

`apps/api/tests/ai_providers.rs` drives the three adapters against an in-process
mock HTTP server on loopback (`Endpoint::Override` with
`allow_private_network: true`). It needs no database, credential, or network
access. See [ai-providers.md](ai-providers.md).

The mock lives in `tests/support/provider_mock.rs` (`serve(replies) -> Mock`,
scripted `Reply`s, recorded requests). `test_app` never reaches a real provider:
its AI endpoint is a closed loopback port. To exercise a route that calls a
provider (the Admin connection test in `tests/ai_config.rs`), start a mock and
build the app with `test_app_with_provider(&pool, &mock.url)`.

`apps/api/tests/retention.rs` covers data retention
([retention.md](retention.md#tests)). It places the run's clock (`as_of`)
exactly on a boundary with `retention::start_run`, drives the worker with
`retention::step`, and simulates crashes with `retention::claim` /
`retention::process` on a transaction it rolls back or commits itself.

`apps/api/tests/ai_guard.rs` covers the usage safeguards
([ai-usage.md](ai-usage.md)) against PostgreSQL and the same mock: budget
races and bursts (`Mock::max_in_flight()` is the most requests the mock ever
answered at once), the circuit breaker, failure outcomes, retention, and what
reaches storage and logs. `TestDb::pool_for(&db)` opens a second pool on the
test database to stand in for a second process. Tests move time by editing
rows (`ai_usage.started_at`, `ai_circuit.open_until`), never by sleeping
through a window.

`apps/api/tests/ai_attempts.rs` covers suggestion attempts end to end
([ai-attempts.md](ai-attempts.md)): it submits through the HTTP API and drives
the worker step by step with `ai_attempts::process_one(&worker)` and
`ai_attempts::sweep_one(&worker)`; one test runs the background loop itself to
check shutdown (it is the only one that calls `stop_workers`). The worker
is built with `Worker::with(&test_state(&pool), guard, key)` and short
`WorkerOptions` (lease, heartbeat, saturation retry). A delayed mock reply
(`Reply::after`) keeps a provider call in flight while the test changes
configuration, cancels, or aborts the worker task.

`apps/api/tests/ai_decisions.rs` covers the requester's decisions
([ai-decisions.md](ai-decisions.md)) without a provider: tickets are put into
`ai_processing` or `awaiting_ai_decision` through the lifecycle with
`fixtures::ai_attempt(db, ticket, state)`, which inserts the attempt row a
submission and the worker would have stored. Use the same fixture in any test
that applies `AiSolved` or `AiNotSolved`: both need a `suggestion_ready`
attempt. Concurrency tests send real concurrent HTTP requests on a
multi-threaded runtime and assert the stored result, not which request won.

## Browser tests

```sh
(cd apps/web && bunx playwright install --with-deps chromium)   # once
bun run test:e2e                                              # builds, then runs Playwright
```

`apps/web/playwright.config.ts` runs `e2e/*.spec.ts` in desktop Chromium and
`e2e/shell.spec.ts` again at phone size (Pixel 7); it starts the production server
(`node build/index.js`) twice: on :4173 with `API_ORIGIN` pointing at a stub
API (`e2e/stub-api.mjs`, :4175; sign in as `requester`, `operator`, `specialist`,
`lead`, or `admin` with `correct horse battery`; it also serves `/api/queue`, `/api/queue/counts`, and `/api/catalog` from fixed queue data; `e2e/support.ts` routes browser
`/api` calls to it), and on :4174 pointing at a closed port for the
API-unavailable state. No Rust build, database, or AI key is needed. Locally,
already-running servers on those ports are reused. Failures keep a trace and
screenshot in `apps/web/test-results/`; open a trace with
`cd apps/web && bunx playwright show-trace <path>`.

## CI

The `checks` job runs `bun run test` against a PostgreSQL service, then
`bun run test:e2e`. Any failing test exits non-zero and fails the job; on
failure, Playwright output is uploaded as the `playwright` artifact. See
[ci.md](ci.md).

Admin management coverage: `apps/api/tests/management.rs` exercises persisted
policy defaults/validation/audit, every non-Admin role, concurrent last-Admin
role edits, atomic leadership/archival/restoration, competing lead appointments,
assignment clearing and immediate membership-scope revocation with requester
access retained, and token-free invitation history/reissue. Existing account,
invitation and authorization tests remain in place. Web tests cover localized
validation with retained input, Admin promotion without membership, lead
eligibility and complete paginated selector options. `e2e/admin.spec.ts` covers
both languages at mobile width, account identity/role controls and denied deep
links against the browser stub; Rust tests use real PostgreSQL.

## Full browser workflows

`bun run test:e2e:full` runs the built Node app through a same-origin loopback
gateway, with the real Axum application router, PostgreSQL, and the AI,
notification, and confirmation-timeout workers. Playwright uses a fresh
deployment for first-admin bootstrap and its organization-wide policy changes.
The test entry point in `apps/api/tests/workflow_server/main.rs` creates its own
migrated database through `TestDb` and drops it with its private screenshot
directory when Playwright closes the launcher. It is a Cargo test target and
ships in neither application. Its only AI destination is the local provider
mock, and mail goes to the existing local STARTTLS capture.

Start dev PostgreSQL and install Chromium as for the other suites. On a shared
machine, run under the same browser lock as the stub suite:

```sh
export TEST_DATABASE_URL=postgres://tikkit:tikkit@127.0.0.1:5432/tikkit
# Optional warm-up, also used in CI.
timeout 900 cargo test --locked -p tikkit-api --test workflow_server --no-run
until mkdir /tmp/tikkit-e2e.lock 2>/dev/null; do sleep 10; done
trap 'rmdir /tmp/tikkit-e2e.lock' EXIT
CI=1 bun run test:e2e:full
```

Set `WORKFLOW_SETUP_LANGUAGE=de` to complete first-admin setup in German. CI runs
both setup languages against separate fresh databases.

Local ports are 18500 (gateway), 18501 (API), 18502 (built portal), and 18503
(SMTP). The in-process provider mock binds an ephemeral loopback port. The
scenarios use the gateway origin for API calls and links. Set
`WORKFLOW_BASE_URL` when pointing Playwright at an already running fresh stack
with a deterministic provider and its own origin/transport configuration.
See [workflow-coverage.md](workflow-coverage.md) for coverage and the remaining
production Compose verification.
