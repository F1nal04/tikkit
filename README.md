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
bun run test      # Rust API tests
bun run start     # Run both production builds (build first)
```

Use `bun run dev --filter=@tikkit/web` or `--filter=@tikkit/api` to run a
single application. Cargo commands also work from the repository root.
`check` builds the frontend first so route types are generated on a fresh clone.
Turbo caches frontend builds; Cargo manages Rust compilation caching.

## Production

The frontend runs with Node.js from `apps/web/.output/server/index.mjs`
(Nitro). Its port defaults to 3000; set `PORT` and `HOST` as needed.
The backend executable is `target/release/tikkit-api`. Set `API_ADDR` to
change its listen address, for example `API_ADDR=0.0.0.0:3001` in a container.

Configure your deployment's reverse proxy to send `/api/*` to Rust and
other requests to the frontend. The Vite API proxy runs only in development.
Changing the API's development port also requires updating the proxy in
`apps/web/vite.config.ts`.
