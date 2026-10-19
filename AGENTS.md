## What this is

Tikkit is a self-hosted IT ticketing system (one organization per deployment) with a requester portal, a shared help-desk queue, team-based support, admin, and optional BYOK AI assistance. The code is currently a scaffold (health endpoint + landing page); the agreed product requirements live in `spec.md` and implementation is tracked in GitHub issues (roadmap: F1nal04/tikkit#2). **Read `spec.md` before implementing any feature — it is the source of truth for behavior.**

## Commands

Requires Bun ≥ 1.4.3, Node ≥ 22.17, Rust ≥ 1.88 (with rustfmt + clippy). Run from the repo root; Turbo fans out to `apps/*`.

```sh
bun install
bun run dev                          # web on :3000, API on 127.0.0.1:3001
bun run dev --filter=@tikkit/web     # or --filter=@tikkit/api
bun run check                        # svelte-kit sync + svelte-check, cargo fmt --check, cargo clippy -D warnings
bun run build                        # vite build (adapter-node) + cargo build --release
bun run test                         # vitest (web, api-client) + cargo test
bun run test:e2e                     # Playwright browser tests (builds first; separate from test)
bun run start                        # run production builds (build first)
```

Rust directly (Cargo workspace root is the repo root):

```sh
cargo test -p tikkit-api                              # all API tests
cargo test -p tikkit-api health_returns_json_status   # single test by name
cargo clippy --locked --all-targets -- -D warnings     # must be warning-free; CI fails otherwise
cargo fmt --all
```

Database (PostgreSQL 17, see `docs/schema.md`):

```sh
docker compose -f docker-compose.dev.yml up -d --wait   # dev Postgres on 127.0.0.1:5432
cp .env.example .env                                     # API and tests load .env
TEST_DATABASE_URL=postgres://tikkit:tikkit@127.0.0.1:5432/tikkit cargo test --locked -p tikkit-api
```

Notes:

- Always use `--locked` with Cargo (the package.json scripts do); don't let Cargo rewrite `Cargo.lock` silently.
- `.svelte-kit/` holds generated types and build output; `check` runs `svelte-kit sync` first, so it doesn't depend on `build`. Don't hand-edit it.
- SvelteKit 3 conventions: config is passed to `sveltekit(...)` in `vite.config.ts` (no `svelte.config.js`), `src/lib` is imported as `#lib/...` (package.json subpath imports, explicit `.ts` extension), and env vars are declared in `src/env.ts` and read from `$app/env/private`.
- Migrations are `apps/api/migrations/NNNN_*.sql`, embedded via `sqlx::migrate!` and applied at API startup. Never edit an applied migration. Use runtime `sqlx::query`/`query_as`, not the compile-time `query!` macros.
- Runtime config is validated in `apps/api/src/config.rs` (all errors at once, values never printed); document new variables in `docs/configuration.md` and `.env.example`. `/api/health` is liveness, `/api/ready` checks DB + migrations.
- API conventions (error envelope, extractors, pagination, OpenAPI): `docs/api-conventions.md`. Use `api::{Json, Path, Query, ApiError}`, not raw axum extractors. After changing routes/schemas run `cargo run --locked -p tikkit-api --bin openapi > apps/api/openapi.json` and `bun run --cwd packages/api-client generate`; tests/checks fail on drift.
- Testing layers, commands, and fixtures: `docs/testing.md`. DB integration tests live in `apps/api/tests/`; use `mod support;` for `TestDb` (own database per test, skipped without `TEST_DATABASE_URL`/`DATABASE_URL`), `test_app`, and the five-role `fixtures`. First-time browser setup: `cd apps/web && bunx playwright install --with-deps chromium`.
- The dev server does not hot-reload Rust. Restart `bun run dev` (or just the api filter) after backend changes.

## Architecture

Bun + Turborepo monorepo, two apps, same-origin routing by path prefix:

- `apps/web` — SvelteKit 3 (Svelte 5, TypeScript, file-based routes in `src/routes/`, Vite 8, `@sveltejs/adapter-node`). Pages render on the server, then hydrate for client navigation. In dev, Vite proxies `/api` → `http://127.0.0.1:3001` (`vite.config.ts`). During SSR, `handleFetch` in `src/hooks.server.ts` rewrites `/api/*` fetches to `API_ORIGIN` (default `http://127.0.0.1:3001`) and forwards the request cookie. Production entry: `apps/web/build/index.js`, env `HOST`/`PORT`/`API_ORIGIN`, plus `ORIGIN` (the public portal origin, required behind the reverse proxy for SvelteKit's CSRF/origin checks; `PROTOCOL_HEADER`/`HOST_HEADER`/`XFF_DEPTH` are the alternatives). SSR page responses are `cache-control: private, no-store`. SvelteKit only renders pages: no server endpoints or form actions with business logic. Rust stays authoritative. Locale (`en`/`de`) is resolved in `hooks.server.ts` from `?lang=`, the `lang` cookie, then `Accept-Language`; strings live in `src/lib/i18n/{en,de}.ts`. Shells, role navigation (`src/lib/nav.ts`), route guards, and shared controls are described in `docs/frontend.md`; add new signed-in pages under `src/routes/(app)/` and register them there.
- `apps/api` — Rust crate `tikkit-api` (Axum 0.8, Tokio, serde). `lib.rs` exposes `app() -> Router`; `main.rs` only binds the listener (`API_ADDR`, default `127.0.0.1:3001`) and handles graceful shutdown. Keep routes/handlers in the lib so tests can drive the router with `tower::ServiceExt::oneshot` without a socket (see the existing health test). The `package.json` in this dir exists solely to let Turbo invoke Cargo.
- All backend routes are mounted under `/api/*`. In production a reverse proxy sends `/api/*` to Rust and everything else to the Node frontend; changing the API dev port means also changing the Vite proxy.

Planned per `spec.md` (not yet present): private filesystem volume for screenshots, durable job processing (AI attempts, notification outbox, resolution-confirmation timeouts), Docker Compose deployment. Validation and authorization are enforced in Rust, not the frontend.

## Product invariants that code must preserve

These come from `spec.md` and the implementation issue template (`.github/ISSUE_TEMPLATE/implementation.yml`). Violating them is a bug even if a ticket doesn't mention them.

- **Identity:** accounts are identified by immutable ID and unique normalized username. Email is contact info only; multiple accounts may share an email and must never be merged or compared by it. Ownership and self-handling checks compare account IDs.
- **Self-handling:** the submitting account cannot claim, be assigned, dispatch, reply as staff, note, or resolve its own ticket, regardless of role.
- **Roles/teams:** five roles (User, Operator, Specialist, Team Lead, Admin). Non-admins belong to at most one team; each active team has exactly one lead; Admins cannot be team members or assignees. Protect the last active Admin.
- **Queue/routing:** every human-bound ticket enters one shared general queue with no team or assignee. Dispatch to a team is manual; assignment within a team is manual (claim, or Lead/Admin assign). Never auto-route by category/priority/AI outcome.
- **Team isolation:** after dispatch, staff access is limited to current destination-team members and Admins, everywhere: detail, search, counts, exports, attachments, caches, realtime, queued notifications. On a claimed ticket only the assignee, their Team Lead, or an Admin may write.
- **AI:** disabled by default; one immutable input, one persisted solution per ticket, no chat/regenerate. Only an explicit requester "Solved" marks AI-solved. AI-solved records are final: read-only, never in operator views, never reopenable. Any AI failure must still hand off to humans exactly once. Admin "Skip AI" categories make zero provider calls; requester skip is only allowed if the org toggle is on. Images go to the provider as real image inputs or not at all; never publish private URLs for the provider to fetch.
- **Resolution/closure:** staff-proposed resolution → awaiting requester confirmation; auto-close after Admin-configured timeout (default 168h) with a snapshotted deadline, recorded separately from requester-confirmed closure. Silence never equals acceptance. Rejecting a proposal is not "reopening". Reopening human-closed tickets is behind an Admin toggle, default off.
- **Immutability/audit:** submitted intake is frozen; state transitions are immutable events with provenance; audit events are attributable and never rewritten by normal actions. Secrets (AI keys, SMTP creds) are encrypted, write-only, and redacted from logs/errors/audit.
- **Localization:** English and German everywhere from day one; UTC timestamps; machine identifiers (roles, statuses, errors) stay stable across languages.
- **Scope guard:** no inbound email, no arbitrary file attachments (static PNG/JPEG/WebP screenshots only), no form builder, no multi-tenant, no SLA engine in the first release. Don't build toward deferred scope.

Build authorization, validation, audit events, both languages, and tests alongside each feature rather than deferring them to a "hardening" pass.

## Conventions

- Conventional commits with app scope (`feat(web):`, `fix(api):`). release-please releases `web` and `api` as separate components (`web-vX.Y.Z`, `api-vX.Y.Z`); it attributes commits by touched paths, so keep a commit to one app where possible.
- No `Co-Authored-By` trailers on commits.
