# CI

`.github/workflows/ci.yml` has one job, `checks`. It runs on every pull request
targeting `main`, including Dependabot and release PRs, and on every push to
`main`. From a clean runner with no caches, with a `postgres:17` service
container and `TEST_DATABASE_URL` pointing at it, it runs:

```sh
bun install --frozen-lockfile
bun run check   # svelte-kit sync + svelte-check, cargo fmt --check, cargo clippy -D warnings
bun run build   # vite build (adapter-node), cargo build --release --locked
bun run test    # vitest (web, api-client), cargo test --locked incl. DB integration tests
bunx playwright install --with-deps chromium   # in apps/web
bun run test:e2e  # Playwright against the production build and a stub API
```

On failure, Playwright traces, screenshots, and the HTML report are uploaded
as the `playwright` artifact. See [testing.md](testing.md).

The Bun version comes from `packageManager` in the root `package.json`. Node is
24, and Rust is pinned in the workflow. Actions are pinned to commit SHAs, and
Dependabot updates them.

## Required status check

The required check is named **`checks`**. Renaming the job breaks branch protection.

Branch protection on `main` requires `checks`. It was set through the API on
2026-10-16, with admins exempt and no review requirement. To recreate it:

```sh
gh api -X PUT repos/F1nal04/tikkit/branches/main/protection --input - <<'JSON'
{"required_status_checks":{"strict":false,"checks":[{"context":"checks"}]},
 "enforce_admins":false,"required_pull_request_reviews":null,"restrictions":null}
JSON
```

Or in the UI: Settings → Branches → `main` → Require status checks to pass → add `checks`.
