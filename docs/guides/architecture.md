# Architecture at a glance

One page with the facts a new operator or contributor needs, and links to the
reference pages for detail. Each statement here is checked against `package.json`,
`turbo.json`, `apps/web/vite.config.ts`, `apps/web/src/hooks.server.ts`, the
Dockerfiles and `docker-compose.prod.example.yml`.

## Parts

| Part | What it is |
| --- | --- |
| Frontend (`apps/web`) | **SvelteKit 3** with **Svelte 5** and TypeScript, built with Vite 8 and `@sveltejs/adapter-node` 6. Pages render on the server, then hydrate for client navigation. It has no server endpoints and no form actions with business logic. English and German strings live in `src/lib/i18n/`. |
| Backend (`apps/api`) | Rust crate `tikkit-api`: **Axum 0.8** on Tokio with SQLx. It owns authentication, authorization, validation, persistence and every business rule under `/api/*`. The durable background jobs (AI attempts, notification outbox, resolution-confirmation timeouts, retention) run inside this one process. |
| API client (`packages/api-client`) | TypeScript types and client generated from `apps/api/openapi.json`. Checks fail if they drift. |
| Database | PostgreSQL 17. Migrations are embedded in the API and applied at startup (or with `api migrate`). |
| Files | Private screenshot volume, read only by the API. |

The monorepo uses Bun workspaces and Turborepo. Rust stays authoritative: the
frontend only renders pages and hides what a role cannot use
([frontend structure](../frontend.md), [authorization](../authorization.md)).

## Commands

Run from the repository root (Bun 1.4.3+, Node 22.17+, Rust 1.88+ with rustfmt and Clippy).

| Command | What it runs |
| --- | --- |
| `bun run dev` | Web on `:3000` (`vite dev --port 3000 --strictPort`) and API on `127.0.0.1:3001` (`cargo run --locked`). Restart it after Rust changes. |
| `bun run check` | Web: `svelte-kit sync && svelte-check` (Svelte-aware TypeScript and component diagnostics). API client: `openapi-typescript … --check` and `tsc --noEmit`. API: `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings`. |
| `bun run test` | Web: `vitest run` (jsdom). API client: `vitest run`. API: `cargo test --locked`; the database tests need `TEST_DATABASE_URL` and are skipped without it. |
| `bun run test:e2e` | Playwright against the production web build (builds first). |
| `bun run build` | Web: `vite build`. API: `cargo build --release --locked`. |
| `bun run start` | Runs both production builds (build first). |

Details: [testing](../testing.md), [CI](../ci.md), the repository [README](../../README.md).

## Build and start output

- **Frontend:** `vite build` writes the Node adapter output to `apps/web/build/`
  (server bundle and client assets). Start it with `node build/index.js` from
  `apps/web` (`node apps/web/build/index.js` in the image). It listens on
  `HOST`/`PORT` (default `0.0.0.0:3000`) and calls the API at `API_ORIGIN` while
  rendering (default `http://127.0.0.1:3001`).
- **Backend:** `cargo build --release` writes `target/release/tikkit-api`. It listens
  on `API_ADDR` (default `127.0.0.1:3001`; `0.0.0.0:3001` in a container) and
  also answers `migrate`, `restore-check`, `credentials-check` and
  `suppress-restored-email` as arguments.
- **Images:** `apps/web/Dockerfile` and `apps/api/Dockerfile` build exactly these
  outputs ([install, step 1](install.md#1-get-the-files-and-the-images)).

## Development routing

In development Vite serves the pages and proxies `/api` to `http://127.0.0.1:3001`
(`server.proxy` in `apps/web/vite.config.ts`, which also drops any incoming
`X-Request-ID`). Open the portal at `http://localhost:3000`, exactly that address,
because `PUBLIC_ORIGIN` in `.env` is the API's CSRF anchor. Changing the API's
development port means changing that proxy too.

While rendering on the server, `handleFetch` in `src/hooks.server.ts` sends `/api/*`
fetches straight to `API_ORIGIN` with only the session cookie and the client address.

## Production routing

One public hostname, one reverse proxy (Caddy in the reference Compose file):

```
browser ──https──▶ proxy ──/api/*──▶ api:3001   (Rust)
                          └─ everything else ─▶ web:3000   (Node, SvelteKit)
web ──(SSR, private network, API_ORIGIN=http://api:3001)──▶ api
```

- The proxy is the only published service. It rejects other hostnames with HTTP 421,
  answers `/metrics` with 404, strips an inbound `X-Request-ID` on API routes and
  overwrites `X-Forwarded-For` and `X-Forwarded-Proto`.
- The web and API services are on a private network with a fixed subnet, and the
  API trusts forwarded client addresses only from it (`TRUSTED_PROXIES`).
- Run **one** API replica.

Reference: [deployment](../deployment.md#runtime-services-and-probes),
[security](../security.md#reverse-proxy-trust).

## There is no `ORIGIN` setting

adapter-node 6 (SvelteKit 3) ignores the old `ORIGIN` variable. Unless the build sets
`kit.paths.origin` (it does not: one image serves every deployment), the page origin is
`https://` plus the request's `Host` header. SvelteKit's origin check for form posts and
the server-side `/api` rewrite use that origin, so the reverse proxy must accept only
the configured host and pass `Host` unchanged. Set `PROTOCOL_HEADER=x-forwarded-proto`
behind a proxy that overwrites that header, so pages send HSTS for https requests;
`HOST_HEADER` and `XFF_DEPTH` are the other adapter variables. The API's separate
`PUBLIC_ORIGIN` (`https://<PUBLIC_HOST>` in the reference stack) is its CSRF anchor and
the base of the links in email and cookies. See
[configuration](../configuration.md#web-server-adapter-node) and
[security](../security.md#csrf-and-origin-policy).
