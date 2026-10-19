# Tikkit

A Bun + Turborepo workspace with a SvelteKit frontend and Rust backend.

## Requirements

- Bun 1.4.3 or newer
- Node.js 22.17 or newer
- Rust 1.85 or newer, including Cargo, rustfmt, and Clippy

## Getting started

```sh
bun install
bun run dev
```

Open <http://localhost:3000>. The page checks the Rust API through Vite's
`/api` proxy. Add `?lang=de` for German. The API listens on `127.0.0.1:3001` and exposes
`GET /api/health`, returning `{"status":"ok"}`.

The frontend reloads automatically. Restart `bun run dev` after Rust changes.
Cargo downloads and compiles Rust dependencies on the first run.

## Layout

```text
apps/
  web/      SvelteKit, Svelte, TypeScript, Vite (adapter-node)
  api/      Axum and Tokio; package.json connects Cargo to Turbo
Cargo.toml  Rust workspace (shared target directory and lockfile)
turbo.json  Workspace tasks and caching
```

## Commands

```sh
bun run dev       # Run both applications
bun run build     # Build the frontend and release API binary
bun run check     # svelte-check, rustfmt, and Clippy
bun run test      # Vitest component tests and Rust API tests
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
