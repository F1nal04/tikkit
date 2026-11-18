# CI

`.github/workflows/ci.yml` has four jobs. They run in parallel on every pull
request targeting `main`, including Dependabot and release PRs, and on every
push to `main`:

| Job | Runs |
|---|---|
| `api` | With a pinned PostgreSQL 17 service container and `TEST_DATABASE_URL` pointing at it: `bun run check --filter=@tikkit/api` (`cargo fmt --check`, `cargo clippy -D warnings`) and `bun run test --filter=@tikkit/api` (`cargo test --locked`, including the DB integration tests). |
| `api-release` | `bun run build --filter=@tikkit/api` (`cargo build --release --locked`). |
| `web` | For `@tikkit/web` and `@tikkit/api-client`: check (svelte-kit sync + svelte-check), `vite build`, vitest, then `bun run test:e2e` (Playwright against the production build and a stub API). |
| `compose` | Builds both production images, starts PostgreSQL, applies migrations, and checks TLS, readiness, SSR, a client asset, and a same-origin API call. |

`.github/workflows/security.yml` has one more job, `audit` (dependency
advisories and the secret scan); see [security-scanning.md](security-scanning.md).

On failure, Playwright traces, screenshots, and the HTML report are uploaded
as the `playwright` artifact. See [testing.md](testing.md).

The Bun version comes from `packageManager` in the root `package.json`. Node is
24, and Rust is pinned in the workflow. Actions are pinned to commit SHAs, and
Dependabot updates them.

## Required status checks

The required checks are **`api`**, **`api-release`**, and **`web`**. Renaming one
of these jobs breaks branch protection: the old name stays "expected" forever
and every pull request to `main` is blocked.

`audit` is not required. Its advisory steps fail when an advisory is published,
without any change in the repository, and would then block every merge.

Branch protection on `main` requires the three checks, with admins exempt and no
review requirement. It was last set through the API on 2026-11-16. To recreate it:

```sh
gh api -X PUT repos/F1nal04/tikkit/branches/main/protection --input - <<'JSON'
{"required_status_checks":{"strict":false,"checks":[{"context":"api"},{"context":"api-release"},{"context":"web"}]},
 "enforce_admins":false,"required_pull_request_reviews":null,"restrictions":null}
JSON
```

Or in the UI: Settings → Branches → `main` → Require status checks to pass → add
`api`, `api-release`, and `web`.

## Release images

`Release images` runs on stable `web-vX.Y.Z` and `api-vX.Y.Z` tag pushes.
It validates the tag format and that its commit is on main, then runs the
same four CI jobs on that revision. A CI failure prevents publication. Each
tag publishes one Linux AMD64 image to GHCR, using the same Dockerfile as
the Compose smoke job:

- `web-vX.Y.Z` → `ghcr.io/f1nal04/tikkit-web:web-vX.Y.Z`
- `api-vX.Y.Z` → `ghcr.io/f1nal04/tikkit-api:api-vX.Y.Z`

The publishing job alone has `packages: write` and uses the short-lived
job token. Images carry OCI source, revision and version labels and BuildKit
provenance tied to the checked-out commit. Use immutable digests for
deployments that pin the artifact. Restrict creation of both release tag
patterns to release maintainers and disallow moving or deleting released
tags. Make the GHCR packages public if deployments should pull anonymously.
For existing packages, grant this repository Actions access to publish.

GitHub suppresses push-triggered workflow runs for tags created by another
workflow's default `GITHUB_TOKEN`. For a component tag created with that
token, dispatch `Release images` on the tag with
`gh workflow run release-images.yml --ref web-vX.Y.Z` (or the API tag).
The same ancestry and CI gates apply. See [GitHub workflow trigger rules](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow).
