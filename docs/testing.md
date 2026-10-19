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
- `fixtures::team(&pool, "Name", lead) -> Result<Uuid>`: team plus lead
  membership in one transaction; `fixtures::member(&pool, team, account)`

## Browser tests

```sh
(cd apps/web && bunx playwright install --with-deps chromium)   # once
bun run test:e2e                                              # builds, then runs Playwright
```

`apps/web/playwright.config.ts` runs `e2e/*.spec.ts` in desktop Chromium and
`e2e/shell.spec.ts` again at phone size (Pixel 7); it starts the production server
(`node build/index.js`) twice: on :4173 with `API_ORIGIN` pointing at a stub
API (`e2e/stub-api.mjs`, :4175; sign in as `requester`, `operator`, `specialist`,
`lead`, or `admin` with `correct horse battery`; `e2e/support.ts` routes browser
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
