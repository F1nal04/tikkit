# Tikkit

A Bun + Turborepo workspace with a SvelteKit frontend and Rust backend.

## Requirements

- Bun 1.4.3 or newer
- Node.js 22.17 or newer
- Rust 1.88 or newer, including Cargo, rustfmt, and Clippy

## Getting started

```sh
bun install
bun run dev
```

Open <http://localhost:3000>. The page checks the Rust API through Vite's
`/api` proxy. Add `?lang=de` for German. The API listens on `127.0.0.1:3001` and exposes
`GET /api/health`, returning `{"status":"ok"}`.

On a fresh database every page redirects to <http://localhost:3000/setup>:
create the first Admin there (username, contact email, password, language) and
you are signed in. Setup works exactly once; afterwards sign in at `/login`.
In production, complete `/setup` before exposing the deployment publicly:
until then whoever opens it first becomes the Admin.
Sessions, cookies, CSRF, and rate limits are described in [docs/auth.md](docs/auth.md).

The frontend reloads automatically. Restart `bun run dev` after Rust changes.
Cargo downloads and compiles Rust dependencies on the first run.

## Database

The API needs PostgreSQL 17. For development, start the pinned container and
copy the example environment (the API loads `.env` from the repository root):

```sh
docker compose -f docker-compose.dev.yml up -d --wait
cp .env.example .env
# then replace both CHANGE_ME values in .env with `openssl rand -base64 32` output
```

All API settings, probes (`/api/health`, `/api/ready`), graceful shutdown, and a
production Compose sketch are described in
[docs/configuration.md](docs/configuration.md).

The API applies migrations from `apps/api/migrations/` on startup, so
`bun run dev` or `cargo run -p tikkit-api` migrates a fresh database; it exits
with an error if `DATABASE_URL` is missing, the database is unreachable, or the
schema is newer than the build. Integration tests create and drop a throwaway
database per test through `TEST_DATABASE_URL` and are skipped when it is unset:

```sh
TEST_DATABASE_URL=postgres://tikkit:tikkit@127.0.0.1:5432/tikkit cargo test --locked -p tikkit-api
```

Stop with `docker compose -f docker-compose.dev.yml down` (add `-v` to delete
the data volume). See [docs/schema.md](docs/schema.md) for identifiers and
database-enforced invariants.

## API contract

The API publishes OpenAPI at `/api/openapi.json` (committed as
`apps/api/openapi.json`); `packages/api-client` is the typed client generated
from it. Error format, pagination, and regeneration commands are in
[docs/api-conventions.md](docs/api-conventions.md).

## Layout

```text
apps/
  web/      SvelteKit, Svelte, TypeScript, Vite (adapter-node)
  api/      Axum and Tokio; package.json connects Cargo to Turbo
packages/
  api-client/  Typed API client generated from apps/api/openapi.json
Cargo.toml  Rust workspace (shared target directory and lockfile)
turbo.json  Workspace tasks and caching
```

## Commands

```sh
bun run dev       # Run both applications
bun run build     # Build the frontend and release API binary
bun run check     # svelte-check, rustfmt, and Clippy
bun run test      # Vitest and Rust API tests (see docs/testing.md)
bun run test:e2e  # Playwright browser tests
bun run start     # Run both production builds (build first)
```

Use `bun run dev --filter=@tikkit/web` or `--filter=@tikkit/api` to run a
single application. Cargo commands also work from the repository root.
`check` runs `svelte-kit sync` first, so route types exist on a fresh clone.
Turbo caches frontend builds; Cargo manages Rust compilation caching.

## Production

The frontend runs with Node.js from `apps/web/build/index.js`
(SvelteKit adapter-node). It listens on `0.0.0.0:3000` by default; set `HOST`
and `PORT` as needed. During server-side rendering it calls the API directly
at `API_ORIGIN` (default `http://127.0.0.1:3001`).
Behind a reverse proxy, set `ORIGIN` to the public portal origin (for example
`https://help.example.com`). SvelteKit needs it for its CSRF/origin checks. Use
`PROTOCOL_HEADER`/`HOST_HEADER`/`XFF_DEPTH` only if you can't set `ORIGIN`.
The backend executable is `target/release/tikkit-api`. Set `API_ADDR` to
change its listen address, for example `API_ADDR=0.0.0.0:3001` in a container.

Configure your deployment's reverse proxy to send `/api/*` to Rust and
other requests to the frontend. The Vite API proxy runs only in development.
Changing the API's development port also requires updating the proxy in
`apps/web/vite.config.ts`.
