# Frontend performance baseline

Production build of the SvelteKit landing page (`/`) right after the migration
from TanStack Start. Use it as the comparison point for later frontend changes.

## Conditions

- Recorded 2026-10-16 on an Apple M4 Pro, macOS 27.0.1, Node 24.13.0, Bun 1.4.3.
- `bun run build`, with SvelteKit 3.1, Svelte 5.57, Vite 8.3, and `@sveltejs/adapter-node` 6.
- Sizes come from `apps/web/build/client/_app/immutable`. Gzip sizes are from
  `gzip -9 -c <file> | wc -c`. adapter-node also serves precompressed `.br`/`.gz` files.
- The document was served by `PORT=3100 node apps/web/build/index.js` on
  localhost, with the Rust API running.

## Client JavaScript

| File (hash omitted)           | Raw (B) | Gzip (B) | Loaded on `/` |
| ----------------------------- | ------: | -------: | :-----------: |
| `chunks/E_aJWrar.js`          |  44,051 |   16,142 |      yes      |
| `chunks/DjWGI714.js`          |  32,418 |   12,470 |      yes      |
| `chunks/ZI6_7e0h.js`          |   4,511 |    2,016 |      yes      |
| `chunks/CRzLMJPS.js`          |   1,534 |      814 |      yes      |
| `nodes/2.js` (landing page)   |   1,312 |      798 |      yes      |
| `entry/app.js`                |   1,093 |      575 |      yes      |
| `entry/start.js`              |     522 |      330 |      yes      |
| `nodes/0.js` (root layout)    |     146 |      160 |      yes      |
| `entry/payload.js`            |      65 |       97 |      yes      |
| `chunks/Bjy-W4x2.js`          |   1,365 |      666 |      no       |
| `nodes/1.js` (error page)     |     322 |      259 |      no       |
| **Initial load for `/`**      |  **85,652** | **33,402** |           |
| All client JS                 |  87,339 |   34,327 |               |

CSS: one 460 B file (330 B gzip). The files loaded on `/` are the ones the SSR
HTML links with `modulepreload`.

## Document

- SSR HTML for `/`: 2,012 B. The API health status and the `<html lang>` are
  already in the HTML before hydration.
- Time to first byte on localhost, five runs: 1.2–2.0 ms.

## Not measured

- Browser load and hydration timings, plus client-side navigation. The app has one
  route, so there is no navigation to measure yet. Add these once the Playwright
  tooling from #5 lands.
