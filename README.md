# Tikkit

A Bun + Turborepo workspace with a TanStack Start frontend and Rust backend.

## Requirements

- Bun 1.4.3 or newer
- Node.js 22.12 or newer
- Rust 1.85 or newer, including Cargo, rustfmt, and Clippy

## Getting started

```sh
bun install
bun run dev
```

Open <http://localhost:3000>. The page checks the Rust API through Vite's
`/api` proxy. The API listens on `127.0.0.1:3001` and exposes
`GET /api/health`, returning `{"status":"ok"}`.

The frontend reloads automatically. Restart `bun run dev` after Rust changes.
Cargo downloads and compiles Rust dependencies on the first run.

## Layout

```text
apps/
  web/      TanStack Start, React, TypeScript, Vite
  api/      Axum and Tokio; package.json connects Cargo to Turbo
Cargo.toml  Rust workspace (shared target directory and lockfile)
turbo.json  Workspace tasks and caching
```

## Commands

```sh
bun run dev       # Run both applications
bun run build     # Build the frontend and release API binary
bun run check     # TypeScript, rustfmt, and Clippy
bun run test      # Web component tests (Vitest) and Rust API tests
bun run test:e2e  # Browser tests (Playwright); requires PostgreSQL
bun run start     # Run both production builds (build first)
```

### Browser tests

`bun run test:e2e` starts the API and the web dev server (or reuses ones
already running locally), runs the Playwright suite in `apps/web/e2e/` with
Chromium, and stops them again. Install the browser once with
`bunx playwright install chromium` from `apps/web`.

Each run uses its own throwaway PostgreSQL database, created on the server in
`E2E_DATABASE_URL` (default
`postgres://tikkit:tikkit@127.0.0.1:5432/postgres`; the role needs
`CREATEDB`) and dropped afterwards. On failure, traces and screenshots are
written to `apps/web/test-results/` and an HTML report to
`apps/web/playwright-report/` (`bunx playwright show-report` from
`apps/web`). No AI provider key is needed.

Use `bun run dev --filter=@tikkit/web` or `--filter=@tikkit/api` to run a
single application. Cargo commands also work from the repository root.
`check` builds the frontend first so route types are generated on a fresh clone.
Turbo caches frontend builds; Cargo manages Rust compilation caching.

## CI

`.github/workflows/ci.yml` runs one `checks` job on every pull request to
`main` and every push to `main`: `bun install --frozen-lockfile`, then
`bun run check`, `bun run build`, and `bun run test` against a PostgreSQL 16
service, then the browser tests. `checks` is the required status check. See
[`docs/ci.md`](docs/ci.md) for pinned versions and branch-protection setup.

`.github/workflows/security.yml` checks Rust advisories (`cargo deny`),
JavaScript vulnerabilities (`bun audit`), and secrets (`gitleaks`) on pull
requests and weekly. Dependabot opens grouped weekly Bun, Cargo, and GitHub
Actions updates. Exception policy and local commands are in `docs/ci.md`.

## Production

The frontend runs with Node.js from `apps/web/.output/server/index.mjs`
(Nitro). Its port defaults to 3000; set `PORT` and `HOST` as needed.
The backend executable is `target/release/tikkit-api`. Set `API_ADDR` to
change its listen address, for example `API_ADDR=0.0.0.0:3001` in a container.

Configure your deployment's reverse proxy to send `/api/*` to Rust and
other requests to the frontend. The Vite API proxy runs only in development.
Changing the API's development port also requires updating the proxy in
`apps/web/vite.config.ts`.
